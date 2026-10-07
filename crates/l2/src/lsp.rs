//! Language server (spec 15.4): the Language Server Protocol over stdin / stdout, started with
//! `language-2 lsp`. It publishes diagnostics and answers hover, go to definition, completion,
//! signature help and document symbol requests using [`crate::ide`].
//!
//! Documents are synchronised in full on every change. Diagnostics are computed once the
//! editor has been quiet for a moment, so typing is not slowed down by re-analysis.

use crate::check::ide::Want;
use crate::ide::{self, text, Analysis, CompItem, SymKind, Workspace};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// How long the editor must be quiet before diagnostics are refreshed.
const QUIET: Duration = Duration::from_millis(250);

struct Doc {
    path: PathBuf,
    text: String,
    version: i64,
    /// Analysis of the current text (hover, definition).
    analysis: Option<(i64, Analysis)>,
    /// Diagnostics must be recomputed.
    dirty: bool,
}

struct Server {
    docs: HashMap<String, Doc>,
    ws: Workspace,
    out: std::io::Stdout,
    shutdown: bool,
}

// ---------------------------------------------------------------------- transport
fn read_message(r: &mut impl BufRead) -> Option<Value> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse().ok();
        }
    }
    let mut buf = vec![0u8; len?];
    r.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

impl Server {
    fn send(&mut self, v: Value) {
        let body = v.to_string();
        let mut lock = self.out.lock();
        let _ = write!(lock, "Content-Length: {}\r\n\r\n{}", body.len(), body);
        let _ = lock.flush();
    }

    fn reply(&mut self, id: Value, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn reply_error(&mut self, id: Value, code: i64, msg: &str) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}}));
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
}

// ---------------------------------------------------------------------- URIs and positions
fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let mut s = String::from_utf8(out).ok()?;
    // `/c:/Users/...` on Windows
    if cfg!(windows) {
        let b = s.as_bytes();
        if b.len() > 2 && b[0] == b'/' && b[2] == b':' {
            s.remove(0);
        }
        s = s.replace('/', "\\");
    }
    Some(PathBuf::from(s))
}

pub fn path_to_uri(p: &Path) -> String {
    let s = p.display().to_string().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// LSP position (0-based line, UTF-16 character) -> 1-based (line, column in characters).
fn from_lsp(text: &str, pos: &Value) -> (u32, u32) {
    let line = pos["line"].as_u64().unwrap_or(0) as usize;
    let ch = pos["character"].as_u64().unwrap_or(0) as u32;
    let l = text::line_of(text, line);
    (line as u32 + 1, text::chars_of(l, ch) as u32 + 1)
}

/// 1-based (line, column in characters) -> LSP position.
fn to_lsp(text: &str, line: u32, col: u32) -> Value {
    let l = text::line_of(text, line.saturating_sub(1) as usize);
    json!({"line": line.saturating_sub(1), "character": text::utf16_of(l, col.saturating_sub(1) as usize)})
}

fn range(text: &str, line: u32, col: u32, len: u32) -> Value {
    json!({"start": to_lsp(text, line, col), "end": to_lsp(text, line, col + len)})
}

fn symbol_kind_completion(k: SymKind) -> u32 {
    match k {
        SymKind::Variable | SymKind::Parameter => 6,
        SymKind::Field => 5,
        SymKind::Method | SymKind::BuiltinMethod => 2,
        SymKind::Function => 3,
        SymKind::Constructor => 4,
        SymKind::Class | SymKind::BuiltinType => 7,
        SymKind::Interface => 8,
        SymKind::TypeParam => 25,
        SymKind::Module => 9,
        SymKind::Keyword => 14,
        SymKind::Constant => 21,
    }
}

fn symbol_kind_outline(k: SymKind) -> u32 {
    match k {
        SymKind::Class | SymKind::BuiltinType => 5,
        SymKind::Method | SymKind::BuiltinMethod => 6,
        SymKind::Field => 8,
        SymKind::Constructor => 9,
        SymKind::Interface => 11,
        SymKind::Function => 12,
        SymKind::Constant => 14,
        SymKind::Module => 2,
        _ => 13,
    }
}

fn sort_group(k: SymKind) -> u32 {
    match k {
        SymKind::Variable | SymKind::Parameter => 0,
        SymKind::Field | SymKind::Method | SymKind::BuiltinMethod | SymKind::Constant => 1,
        SymKind::Function | SymKind::Constructor => 2,
        SymKind::Module => 3,
        SymKind::Class | SymKind::Interface | SymKind::TypeParam | SymKind::BuiltinType => 4,
        SymKind::Keyword => 5,
    }
}

// ---------------------------------------------------------------------- requests
impl Server {
    fn doc(&self, params: &Value) -> Option<(String, &Doc)> {
        let uri = params["textDocument"]["uri"].as_str()?.to_string();
        let d = self.docs.get(&uri)?;
        Some((uri, d))
    }

    /// The analysis of the document's current text (computed if needed).
    fn analysis(&mut self, uri: &str) -> Option<&Analysis> {
        let d = self.docs.get(uri)?;
        let fresh = d.analysis.as_ref().map(|(v, _)| *v == d.version).unwrap_or(false);
        if !fresh {
            let a = ide::analyze(&d.path, &d.text, &self.ws, Want::Refs, None);
            let d = self.docs.get_mut(uri)?;
            d.analysis = Some((d.version, a));
        }
        self.docs.get(uri)?.analysis.as_ref().map(|(_, a)| a)
    }

    fn publish(&mut self, uri: &str) {
        if self.analysis(uri).is_none() {
            return;
        }
        let d = &self.docs[uri];
        let (text, a) = (&d.text, &d.analysis.as_ref().unwrap().1);
        let mut out = Vec::new();
        for d in &a.diags {
            let line: Vec<char> = text::line_of(text, d.span.line.saturating_sub(1) as usize).chars().collect();
            let c = d.span.col.saturating_sub(1) as usize;
            let len = match text::word_at(&line, c) {
                Some((s, e)) if s == c => e - s,
                _ => 1,
            };
            out.push(json!({
                "range": range(text, d.span.line, d.span.col, len as u32),
                "severity": if d.severity == crate::diag::Severity::Error { 1 } else { 2 },
                "source": crate::LANG_NAME,
                "message": d.msg,
            }));
        }
        let version = self.docs[uri].version;
        self.notify("textDocument/publishDiagnostics", json!({"uri": uri, "version": version, "diagnostics": out}));
        if let Some(d) = self.docs.get_mut(uri) {
            d.dirty = false;
        }
    }

    fn hover(&mut self, params: &Value) -> Value {
        let Some((uri, d)) = self.doc(params) else { return Value::Null };
        let (line, col) = from_lsp(&d.text, &params["position"]);
        let text = d.text.clone();
        let Some(a) = self.analysis(&uri) else { return Value::Null };
        match ide::hover(a, line, col) {
            Some(h) => json!({"contents": {"kind": "markdown", "value": h.markdown}, "range": range(&text, h.line, h.col, h.len)}),
            None => Value::Null,
        }
    }

    fn definition(&mut self, params: &Value) -> Value {
        let Some((uri, d)) = self.doc(params) else { return Value::Null };
        let (line, col) = from_lsp(&d.text, &params["position"]);
        let Some(a) = self.analysis(&uri) else { return Value::Null };
        let Some((path, l, c, len)) = ide::definition(a, line, col) else { return Value::Null };
        let text = self.ws.overlays.get(&path).cloned().or_else(|| std::fs::read_to_string(&path).ok()).unwrap_or_default();
        json!({"uri": path_to_uri(&path), "range": range(&text, l, c, len)})
    }

    fn completion(&mut self, params: &Value) -> Value {
        let Some((_, d)) = self.doc(params) else { return Value::Null };
        let (line, col) = from_lsp(&d.text, &params["position"]);
        let (path, text) = (d.path.clone(), d.text.clone());
        let Some(c) = ide::complete(&path, &text, &self.ws, line, col) else { return json!({"isIncomplete": false, "items": []}) };
        let cur: Vec<char> = text::line_of(&text, line as usize - 1).chars().collect();
        let call_follows = cur.get(c.end as usize - 1) == Some(&'(');
        let edit_range = json!({"start": to_lsp(&text, line, c.start), "end": to_lsp(&text, line, col)});
        let items: Vec<Value> = c.items.iter().enumerate().map(|(i, it)| completion_item(it, i, &edit_range, call_follows)).collect();
        json!({"isIncomplete": false, "items": items})
    }

    fn signature_help(&mut self, params: &Value) -> Value {
        let Some((_, d)) = self.doc(params) else { return Value::Null };
        let (line, col) = from_lsp(&d.text, &params["position"]);
        let (path, text) = (d.path.clone(), d.text.clone());
        let Some(h) = ide::signature_help(&path, &text, &self.ws, line, col) else { return Value::Null };
        let sigs: Vec<Value> = h
            .sigs
            .iter()
            .map(|s| {
                let open = s.label.find('(').unwrap_or(0);
                let mut at = open;
                let params: Vec<Value> = s
                    .params
                    .iter()
                    .map(|p| {
                        let (a, b) = match s.label[at..].find(p.as_str()) {
                            Some(k) => {
                                let a = at + k;
                                at = a + p.len();
                                (a, a + p.len())
                            }
                            None => (0, 0),
                        };
                        let u = |i: usize| s.label[..i].encode_utf16().count();
                        json!({"label": [u(a), u(b)]})
                    })
                    .collect();
                let mut v = json!({"label": s.label, "parameters": params});
                if let Some(doc) = &s.doc {
                    v["documentation"] = json!({"kind": "markdown", "value": doc});
                }
                v
            })
            .collect();
        json!({"signatures": sigs, "activeSignature": h.active_sig, "activeParameter": h.active_param})
    }

    /// Text of a file: the editor's copy when it is open.
    fn file_text(&self, p: &Path) -> String {
        self.ws.overlays.get(p).cloned().or_else(|| std::fs::read_to_string(p).ok()).unwrap_or_default()
    }

    fn position(&self, params: &Value) -> Option<(PathBuf, String, u32, u32)> {
        let (_, d) = self.doc(params)?;
        let (line, col) = from_lsp(&d.text, &params["position"]);
        Some((d.path.clone(), d.text.clone(), line, col))
    }

    fn references(&mut self, params: &Value) -> Value {
        let Some((path, text, line, col)) = self.position(params) else { return Value::Null };
        let include = params["context"]["includeDeclaration"].as_bool().unwrap_or(true);
        let occ = ide::references(&path, &text, &self.ws, line, col, include);
        let mut texts: HashMap<PathBuf, String> = HashMap::new();
        let locs: Vec<Value> = occ
            .into_iter()
            .map(|(p, l, c, n)| {
                let t = texts.entry(p.clone()).or_insert_with(|| self.file_text(&p));
                json!({"uri": path_to_uri(&p), "range": range(t, l, c, n)})
            })
            .collect();
        Value::Array(locs)
    }

    fn highlights(&mut self, params: &Value) -> Value {
        let Some((path, text, line, col)) = self.position(params) else { return Value::Null };
        let hs = ide::highlights(&path, &text, &self.ws, line, col);
        // kind 3 = write (the declaration), 2 = read
        Value::Array(hs.into_iter().map(|(l, c, n, decl)| json!({"range": range(&text, l, c, n), "kind": if decl { 3 } else { 2 }})).collect())
    }

    fn prepare_rename(&mut self, params: &Value) -> Result<Value, String> {
        let Some((path, text, line, col)) = self.position(params) else { return Ok(Value::Null) };
        let (name, l, c, n) = ide::prepare_rename(&path, &text, &self.ws, line, col)?;
        Ok(json!({"range": range(&text, l, c, n), "placeholder": name}))
    }

    fn rename(&mut self, params: &Value) -> Result<Value, String> {
        let Some((path, text, line, col)) = self.position(params) else { return Ok(Value::Null) };
        let new_name = params["newName"].as_str().unwrap_or("").trim().to_string();
        let edits = ide::rename(&path, &text, &self.ws, line, col, &new_name)?;
        let mut changes = serde_json::Map::new();
        for (p, spots) in edits {
            let t = if p == path { text.clone() } else { self.file_text(&p) };
            let list: Vec<Value> = spots.into_iter().map(|(l, c, n)| json!({"range": range(&t, l, c, n), "newText": new_name})).collect();
            changes.insert(path_to_uri(&p), Value::Array(list));
        }
        Ok(json!({"changes": changes}))
    }

    fn document_symbols(&mut self, params: &Value) -> Value {
        let Some((_, d)) = self.doc(params) else { return Value::Null };
        let text = d.text.clone();
        let syms = ide::document_symbols(&text);
        Value::Array(syms.iter().map(|s| symbol_json(&text, s)).collect())
    }
}

fn completion_item(it: &CompItem, i: usize, edit_range: &Value, call_follows: bool) -> Value {
    let mut v = json!({
        "label": it.label,
        "kind": symbol_kind_completion(it.kind),
        "sortText": format!("{}{:05}", sort_group(it.kind), i),
        "filterText": it.label,
    });
    if !it.detail.is_empty() {
        v["detail"] = json!(it.detail);
    }
    if let Some(doc) = &it.doc {
        v["documentation"] = json!({"kind": "markdown", "value": doc});
    }
    let (new_text, snippet) = match it.takes_args {
        Some(args) if !call_follows => {
            if args {
                (format!("{}($0)", it.label), true)
            } else {
                (format!("{}()", it.label), false)
            }
        }
        _ => (it.label.clone(), false),
    };
    v["textEdit"] = json!({"range": edit_range, "newText": new_text});
    if snippet {
        v["insertTextFormat"] = json!(2);
        v["command"] = json!({"title": "signature help", "command": "editor.action.triggerParameterHints"});
    }
    v
}

fn symbol_json(text: &str, s: &ide::Symbol) -> Value {
    let start = to_lsp(text, s.start.0, s.start.1);
    let name_start = to_lsp(text, s.name_pos.0, s.name_pos.1);
    let name_end = to_lsp(text, s.name_pos.0, s.name_pos.1 + s.name.chars().count() as u32);
    let end_line = text::line_of(text, s.end.0 as usize);
    let mut end = json!({"line": s.end.0, "character": text::utf16_of(end_line, s.end.1 as usize)});
    let after = |a: &Value, b: &Value| (a["line"].as_u64(), a["character"].as_u64()) >= (b["line"].as_u64(), b["character"].as_u64());
    if !after(&end, &name_end) {
        end = name_end.clone();
    }
    json!({
        "name": s.name,
        "detail": s.detail,
        "kind": symbol_kind_outline(s.kind),
        "range": {"start": start, "end": end},
        "selectionRange": {"start": name_start, "end": name_end},
        "children": s.children.iter().map(|c| symbol_json(text, c)).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------- main loop
impl Server {
    fn set_text(&mut self, uri: &str, path: PathBuf, text: String, version: i64) {
        self.ws.overlays.insert(path.clone(), text.clone());
        let d = self.docs.entry(uri.to_string()).or_insert(Doc { path, text: String::new(), version, analysis: None, dirty: true });
        d.text = text;
        d.version = version;
        // other open files may depend on this one
        for d in self.docs.values_mut() {
            d.dirty = true;
            d.analysis = None;
        }
    }

    fn handle(&mut self, msg: Value) {
        let method = msg["method"].as_str().unwrap_or("").to_string();
        let params = msg["params"].clone();
        let id = msg.get("id").cloned();
        match method.as_str() {
            "initialize" => {
                if let Some(folders) = params["workspaceFolders"].as_array() {
                    for f in folders {
                        if let Some(p) = f["uri"].as_str().and_then(uri_to_path) {
                            self.ws.roots.push(p);
                        }
                    }
                }
                if self.ws.roots.is_empty() {
                    if let Some(p) = params["rootUri"].as_str().and_then(uri_to_path) {
                        self.ws.roots.push(p);
                    }
                }
                let result = json!({
                    "capabilities": {
                        "textDocumentSync": {"openClose": true, "change": 1},
                        "hoverProvider": true,
                        "definitionProvider": true,
                        "referencesProvider": true,
                        "documentHighlightProvider": true,
                        "renameProvider": {"prepareProvider": true},
                        "documentSymbolProvider": true,
                        "completionProvider": {"triggerCharacters": ["."], "resolveProvider": false},
                        "signatureHelpProvider": {"triggerCharacters": ["(", ","], "retriggerCharacters": [","]},
                    },
                    "serverInfo": {"name": crate::LANG_NAME, "version": env!("CARGO_PKG_VERSION")},
                });
                if let Some(id) = id {
                    self.reply(id, result);
                }
            }
            "initialized" | "$/cancelRequest" | "$/setTrace" | "workspace/didChangeConfiguration" | "textDocument/didSave" => {}
            "shutdown" => {
                self.shutdown = true;
                if let Some(id) = id {
                    self.reply(id, Value::Null);
                }
            }
            "exit" => std::process::exit(if self.shutdown { 0 } else { 1 }),
            "textDocument/didOpen" => {
                let td = &params["textDocument"];
                let (Some(uri), Some(text)) = (td["uri"].as_str(), td["text"].as_str()) else { return };
                let Some(path) = uri_to_path(uri) else { return };
                let version = td["version"].as_i64().unwrap_or(0);
                self.set_text(uri, path, text.to_string(), version);
            }
            "textDocument/didChange" => {
                let Some(uri) = params["textDocument"]["uri"].as_str() else { return };
                let version = params["textDocument"]["version"].as_i64().unwrap_or(0);
                let Some(text) = params["contentChanges"].as_array().and_then(|c| c.last()).and_then(|c| c["text"].as_str()) else { return };
                let Some(path) = self.docs.get(uri).map(|d| d.path.clone()) else { return };
                self.set_text(uri, path, text.to_string(), version);
            }
            "textDocument/didClose" => {
                let Some(uri) = params["textDocument"]["uri"].as_str() else { return };
                if let Some(d) = self.docs.remove(uri) {
                    self.ws.overlays.remove(&d.path);
                }
                self.notify("textDocument/publishDiagnostics", json!({"uri": uri, "diagnostics": []}));
            }
            _ => {
                let Some(id) = id else { return };
                let result = match method.as_str() {
                    "textDocument/hover" => self.hover(&params),
                    "textDocument/definition" => self.definition(&params),
                    "textDocument/completion" => self.completion(&params),
                    "textDocument/signatureHelp" => self.signature_help(&params),
                    "textDocument/documentSymbol" => self.document_symbols(&params),
                    "textDocument/references" => self.references(&params),
                    "textDocument/documentHighlight" => self.highlights(&params),
                    "textDocument/prepareRename" | "textDocument/rename" => {
                        let r = if method == "textDocument/rename" { self.rename(&params) } else { self.prepare_rename(&params) };
                        match r {
                            Ok(v) => v,
                            Err(msg) => {
                                // RequestFailed: the editor shows the message
                                self.reply_error(id, -32803, &msg);
                                return;
                            }
                        }
                    }
                    _ => {
                        self.reply_error(id, -32601, &format!("method '{}' is not supported", method));
                        return;
                    }
                };
                self.reply(id, result);
            }
        }
    }

    fn publish_dirty(&mut self) {
        let dirty: Vec<String> = self.docs.iter().filter(|(_, d)| d.dirty).map(|(u, _)| u.clone()).collect();
        for uri in dirty {
            self.publish(&uri);
        }
    }
}

/// Runs the language server until the client sends `exit` or closes stdin.
pub fn serve() -> i32 {
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut r = BufReader::new(stdin.lock());
        while let Some(m) = read_message(&mut r) {
            if tx.send(m).is_err() {
                break;
            }
        }
    });
    let mut s = Server { docs: HashMap::new(), ws: Workspace::default(), out: std::io::stdout(), shutdown: false };
    loop {
        match rx.recv_timeout(QUIET) {
            Ok(m) => s.handle(m),
            Err(RecvTimeoutError::Timeout) => s.publish_dirty(),
            Err(RecvTimeoutError::Disconnected) => return if s.shutdown { 0 } else { 1 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris() {
        if cfg!(windows) {
            let p = uri_to_path("file:///c%3A/Users/A%20B/%ED%95%9C.l2").unwrap();
            assert_eq!(p, PathBuf::from(r"c:\Users\A B\한.l2"));
            assert_eq!(path_to_uri(&p), "file:///c%3A/Users/A%20B/%ED%95%9C.l2");
        } else {
            let p = uri_to_path("file:///home/a%20b/x.l2").unwrap();
            assert_eq!(p, PathBuf::from("/home/a b/x.l2"));
            assert_eq!(path_to_uri(&p), "file:///home/a%20b/x.l2");
        }
    }
}
