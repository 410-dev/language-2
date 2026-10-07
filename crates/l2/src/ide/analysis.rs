//! Analyses of one source file: diagnostics, references (hover / go to definition), completion,
//! signature help and document symbols. Positions are 1-based lines and character columns, as
//! in [`Span`]; the language server converts them to LSP positions.

use super::text::{self, is_ident_char};
use super::{CompItem, IdeRef, SigInfo, SymKind};
use crate::ast::{FileAst, Item};
use crate::check::ide::{IdeRec, Want};
use crate::diag::{Diag, SourceMap, Span};
use crate::driver::{analyze_for_ide, IdeInput, SOURCE_EXT, STDLIB};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The editor's view of the files: workspace folders and unsaved buffers.
#[derive(Default)]
pub struct Workspace {
    pub roots: Vec<PathBuf>,
    pub overlays: HashMap<PathBuf, String>,
}

pub struct Analysis {
    pub sources: SourceMap,
    /// Source id of the analysed file.
    pub file: u32,
    /// Diagnostics of the analysed file.
    pub diags: Vec<Diag>,
    pub refs: Vec<IdeRef>,
    pub ast: Option<FileAst>,
    pub completion: Option<Vec<CompItem>>,
    pub signatures: Option<Vec<SigInfo>>,
}

/// Name used for the identifier inserted where completion is requested.
const MARK: &str = "__l2c";

fn has_main(text: &str) -> bool {
    text.lines().any(|l| {
        let code = l.split("//").next().unwrap_or("");
        let t: Vec<&str> = code.split(|c: char| c.is_whitespace() || c == '(').filter(|w| !w.is_empty()).collect();
        t.iter().position(|w| *w == "function").map(|i| t.get(i + 2) == Some(&"main")).unwrap_or(false)
    })
}

/// Where a file belongs.
#[derive(Debug, PartialEq, Eq)]
pub struct ModuleCtx {
    /// Directory that `using` paths are resolved against.
    pub base: Option<PathBuf>,
    /// Module name (`pkg.Name`; the file stem for an entry file).
    pub module: String,
    pub package: String,
    /// A standard library module (a copy, or the compiler's own source).
    pub stdlib: bool,
    /// The prelude.
    pub prelude: bool,
}

/// The standard library module a path names: `.../stdlib/io/File.l2` -> `io.File`.
fn stdlib_module(path: &Path) -> Option<&'static str> {
    let parts: Vec<String> = path.with_extension("").components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    let at = parts.iter().rposition(|p| p == "stdlib")?;
    let name = parts[at + 1..].join(".");
    STDLIB.iter().map(|(n, _)| *n).find(|n| *n == name)
}

fn is_prelude(path: &Path) -> bool {
    if path.file_name().map(|n| n != "prelude.l2").unwrap_or(true) {
        return false;
    }
    let dir = path.parent().unwrap_or(Path::new(""));
    dir == stdlib_dir() || (dir.file_name().map(|n| n == "src").unwrap_or(false) && dir.parent().map(|p| p.join("stdlib").is_dir()).unwrap_or(false))
}

/// Where a file belongs. Files with `main` are program entry files; other files are modules of
/// the package given by their directory relative to the nearest enclosing directory that holds
/// an entry file. Standard library sources (and the prelude) are recognised by their path.
pub fn module_context(path: &Path, text: &str, ws: &Workspace) -> ModuleCtx {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let dir = path.parent().map(|p| p.to_path_buf());
    let plain = |base: Option<PathBuf>, module: String, package: String| ModuleCtx { base, module, package, stdlib: false, prelude: false };
    if is_prelude(path) {
        return ModuleCtx { base: None, module: "<prelude>".into(), package: String::new(), stdlib: true, prelude: true };
    }
    if let Some(m) = stdlib_module(path) {
        let package = m.rsplit_once('.').map(|(p, _)| p.to_string()).unwrap_or_default();
        return ModuleCtx { base: None, module: m.to_string(), package, stdlib: true, prelude: false };
    }
    if has_main(text) {
        return plain(dir, stem, String::new());
    }
    let Some(dir) = dir else { return plain(None, stem, String::new()) };
    let root = ws.roots.iter().filter(|r| dir.starts_with(r)).max_by_key(|r| r.components().count()).cloned();
    let mut cur = dir.parent().map(|p| p.to_path_buf());
    let mut rel: Vec<String> = vec![dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()];
    while let Some(d) = cur {
        let entry = std::fs::read_dir(&d).ok().map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|e| e == SOURCE_EXT).unwrap_or(false))
                .any(|p| ws.overlays.get(&p).cloned().or_else(|| std::fs::read_to_string(&p).ok()).map(|t| has_main(&t)).unwrap_or(false))
        });
        if entry == Some(true) {
            let mut parts = rel.clone();
            parts.reverse();
            let package = parts.join(".");
            return plain(Some(d), format!("{}.{}", package, stem), package);
        }
        if root.as_ref().map(|r| &d == r || !d.starts_with(r)).unwrap_or(true) {
            break;
        }
        rel.push(d.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
        cur = d.parent().map(|p| p.to_path_buf());
    }
    plain(Some(dir), stem, String::new())
}

/// Analyses `text` as the contents of `path`.
pub fn analyze(path: &Path, text: &str, ws: &Workspace, want: Want, target: Option<(u32, u32)>) -> Analysis {
    let m = module_context(path, text, ws);
    let input = IdeInput {
        path: path.display().to_string(),
        text,
        base: m.base.as_deref(),
        module: m.module,
        package: m.package,
        stdlib: m.stdlib,
        prelude: m.prelude,
        overlays: &ws.overlays,
    };
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| analyze_for_ide(&input, |file| IdeRec::new(file, text, want, target))));
    match run {
        Ok(a) => {
            let file = a.file;
            let mut diags: Vec<Diag> = a.diags.into_iter().filter(|d| d.span.file == file && d.span.line > 0).collect();
            diags.sort_by_key(|d| (d.span.line, d.span.col));
            diags.dedup_by(|a, b| a.span == b.span && a.msg == b.msg);
            Analysis { sources: a.sources, file, diags, refs: a.rec.refs, ast: Some(a.ast), completion: a.rec.completion, signatures: a.rec.signatures }
        }
        Err(_) => {
            let mut sources = SourceMap::default();
            sources.add(path.display().to_string(), text.to_string());
            Analysis {
                sources,
                file: 0,
                diags: vec![Diag::error(Span::new(0, 1, 1), "internal error in the language server while analysing this file")],
                refs: Vec::new(),
                ast: None,
                completion: None,
                signatures: None,
            }
        }
    }
}

// ---------------------------------------------------------------------- hover and definition
pub struct Hover {
    pub markdown: String,
    pub line: u32,
    pub col: u32,
    pub len: u32,
}

/// The reference covering (line, col).
pub fn ref_at(a: &Analysis, line: u32, col: u32) -> Option<&IdeRef> {
    a.refs.iter().filter(|r| r.line == line && r.col <= col && col <= r.col + r.len).min_by_key(|r| r.len)
}

/// Position of the declared name of `def` in its source: (file, line, col).
pub fn def_position(sources: &SourceMap, def: &(Span, String)) -> (u32, u32, u32) {
    let (span, name) = def;
    if let Some((_, src)) = sources.files.get(span.file as usize) {
        let line: Vec<char> = text::line_of(src, span.line.saturating_sub(1) as usize).chars().collect();
        if let Some(c) = text::find_word(&line, span.col.saturating_sub(1) as usize, name) {
            return (span.file, span.line, c as u32 + 1);
        }
    }
    (span.file, span.line.max(1), span.col.max(1))
}

/// Documentation of a declaration: the comment above it; for a module's main class without
/// one, the comment at the top of the file.
pub fn def_doc(sources: &SourceMap, def: &(Span, String)) -> Option<String> {
    let (span, name) = def;
    let (path, src) = sources.files.get(span.file as usize)?;
    if span.line == 0 {
        return None;
    }
    if let Some(d) = text::doc_comment(src, span.line) {
        return Some(d);
    }
    let decl = text::line_of(src, span.line as usize - 1);
    let stem = Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let is_type = decl.split_whitespace().any(|w| w == "class" || w == "interface");
    if is_type && &stem == name {
        return text::header_comment(src);
    }
    None
}

fn markdown(code: &str, about: &str, doc: Option<&str>) -> String {
    let mut s = format!("```l2\n{}\n```", code);
    if !about.is_empty() {
        s.push_str(&format!("\n\n{}", about));
    }
    if let Some(d) = doc {
        s.push_str(&format!("\n\n---\n\n{}", d));
    }
    s
}

pub fn hover(a: &Analysis, line: u32, col: u32) -> Option<Hover> {
    let r = ref_at(a, line, col)?;
    let doc = r.doc.clone().or_else(|| r.def.as_ref().and_then(|d| def_doc(&a.sources, d)));
    Some(Hover { markdown: markdown(&r.code, &r.about, doc.as_deref()), line: r.line, col: r.col, len: r.len })
}

/// A location in a file on disk: (path, line, col, length).
pub type Location = (PathBuf, u32, u32, u32);

/// Directory holding copies of the embedded standard library, so that editors can open them.
pub fn stdlib_dir() -> PathBuf {
    std::env::temp_dir().join(crate::LANG_NAME).join(format!("sdk-{}-{}", crate::sdk::SDK_VERSION, env!("CARGO_PKG_VERSION")))
}

/// The file on disk for source `file`: the file itself, or a read-only copy of an embedded
/// standard library module.
pub fn source_path(sources: &SourceMap, file: u32) -> Option<PathBuf> {
    let (label, src) = sources.files.get(file as usize)?;
    let rel = if label == "<prelude>" {
        "prelude.l2".to_string()
    } else if let Some(r) = label.strip_prefix("<stdlib>/") {
        format!("stdlib/{}", r)
    } else {
        return Some(PathBuf::from(label));
    };
    let p = stdlib_dir().join(rel);
    if std::fs::read_to_string(&p).ok().as_deref() != Some(src.as_str()) {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(meta) = std::fs::metadata(&p) {
            let mut perm = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(&p, perm);
        }
        std::fs::write(&p, src).ok()?;
        if let Ok(meta) = std::fs::metadata(&p) {
            let mut perm = meta.permissions();
            perm.set_readonly(true);
            let _ = std::fs::set_permissions(&p, perm);
        }
    }
    Some(p)
}

pub fn definition(a: &Analysis, line: u32, col: u32) -> Option<Location> {
    let r = ref_at(a, line, col)?;
    let def = r.def.as_ref()?;
    let (file, l, c) = def_position(&a.sources, def);
    let path = source_path(&a.sources, file)?;
    Some((path, l, c, def.1.chars().count() as u32))
}

// ---------------------------------------------------------------------- references and rename
/// A path for comparisons: case-insensitive with `/` separators on Windows.
fn path_key(p: &Path) -> String {
    let s = p.display().to_string();
    if cfg!(windows) {
        s.replace('\\', "/").to_lowercase()
    } else {
        s
    }
}

/// What a reference refers to, comparable across analyses of different files.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    /// A declaration: (path, line, column).
    Decl(String, u32, u32),
    /// A class and its constructors: (path of the declaring file, class name).
    Class(String, String),
}

fn target_of(a: &Analysis, r: &IdeRef) -> Option<Target> {
    let def = r.def.as_ref()?;
    let (file, l, c) = def_position(&a.sources, def);
    let label = &a.sources.files.get(file as usize)?.0;
    let path = if label.starts_with('<') { label.clone() } else { path_key(Path::new(label)) };
    Some(match r.kind {
        SymKind::Class | SymKind::Interface | SymKind::Constructor => Target::Class(path, def.1.clone()),
        _ => Target::Decl(path, l, c),
    })
}

/// The `.l2` files of the workspace (or of the file's directory without a workspace).
fn workspace_files(path: &Path, ws: &Workspace) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
        if depth > 12 || out.len() > 5000 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for p in entries {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if p.is_dir() {
                if !name.starts_with('.') && name != "target" && name != "node_modules" {
                    walk(&p, out, depth + 1);
                }
            } else if p.extension().map(|e| e == SOURCE_EXT).unwrap_or(false) {
                out.push(p);
            }
        }
    }
    let mut roots: Vec<PathBuf> = ws.roots.iter().filter(|r| path.starts_with(r)).cloned().collect();
    if roots.is_empty() {
        roots = ws.roots.clone();
    }
    if roots.is_empty() {
        roots.extend(path.parent().map(|p| p.to_path_buf()));
    }
    let mut out = Vec::new();
    for r in roots {
        walk(&r, &mut out, 0);
    }
    for p in ws.overlays.keys() {
        if p.extension().map(|e| e == SOURCE_EXT).unwrap_or(false) && !out.iter().any(|q| path_key(q) == path_key(p)) {
            out.push(p.clone());
        }
    }
    out
}

/// A place in a file: (path, line, column, length).
pub type Occurrence = (PathBuf, u32, u32, u32);

/// The symbol at (line, col) and every place that refers to it. Local variables are searched in
/// the file only; other symbols in all `.l2` files of the workspace. The first element is the
/// symbol's reference at the position.
fn occurrences(path: &Path, src: &str, ws: &Workspace, line: u32, col: u32, workspace: bool) -> Option<(IdeRef, Vec<(Occurrence, bool)>)> {
    let a = analyze(path, src, ws, Want::Refs, None);
    let r = ref_at(&a, line, col)?.clone();
    let target = target_of(&a, &r)?;
    let local = matches!(r.kind, SymKind::Variable | SymKind::Parameter | SymKind::TypeParam);
    let mut out: Vec<(Occurrence, bool)> = Vec::new();
    let mut collect = |a: &Analysis, p: &Path| {
        for x in &a.refs {
            if target_of(a, x).as_ref() == Some(&target) {
                let is_decl = x.def.as_ref().map(|d| def_position(&a.sources, d) == (a.file, x.line, x.col)).unwrap_or(false);
                out.push(((p.to_path_buf(), x.line, x.col, x.len), is_decl));
            }
        }
    };
    collect(&a, path);
    if workspace && !local {
        let me = path_key(path);
        for f in workspace_files(path, ws) {
            if path_key(&f) == me {
                continue;
            }
            let Some(text) = ws.overlays.get(&f).cloned().or_else(|| std::fs::read_to_string(&f).ok()) else { continue };
            // only files that mention the name can refer to it
            if !text.contains(r.def.as_ref().map(|d| d.1.as_str()).unwrap_or("")) {
                continue;
            }
            let b = analyze(&f, &text, ws, Want::Refs, None);
            collect(&b, &f);
        }
    }
    out.sort_by_key(|x| (path_key(&x.0 .0), x.0 .1, x.0 .2));
    out.dedup_by(|x, y| path_key(&x.0 .0) == path_key(&y.0 .0) && x.0 .1 == y.0 .1 && x.0 .2 == y.0 .2);
    Some((r, out))
}

/// All references to the symbol at (line, col), declarations included or not.
pub fn references(path: &Path, src: &str, ws: &Workspace, line: u32, col: u32, include_decl: bool) -> Vec<Occurrence> {
    match occurrences(path, src, ws, line, col, true) {
        Some((_, occ)) => occ.into_iter().filter(|(_, d)| include_decl || !d).map(|(o, _)| o).collect(),
        None => Vec::new(),
    }
}

/// Occurrences of the symbol at (line, col) in this file only (highlighting): (line, col, len,
/// is the declaration).
pub fn highlights(path: &Path, src: &str, ws: &Workspace, line: u32, col: u32) -> Vec<(u32, u32, u32, bool)> {
    match occurrences(path, src, ws, line, col, false) {
        Some((_, occ)) => occ.into_iter().filter(|(o, _)| path_key(&o.0) == path_key(path)).map(|((_, l, c, n), d)| (l, c, n, d)).collect(),
        None => Vec::new(),
    }
}

fn valid_name(n: &str) -> bool {
    let mut cs = n.chars();
    matches!(cs.next(), Some(c) if c.is_alphabetic() || c == '_') && cs.all(|c| c.is_alphanumeric() || c == '_') && !super::builtins::KEYWORDS.contains(&n) && !n.starts_with("__")
}

/// Whether the symbol at (line, col) can be renamed: its name and range, or why not.
pub fn prepare_rename(path: &Path, src: &str, ws: &Workspace, line: u32, col: u32) -> Result<(String, u32, u32, u32), String> {
    let a = analyze(path, src, ws, Want::Refs, None);
    let Some(r) = ref_at(&a, line, col) else { return Err("이름을 바꿀 수 있는 기호가 아닙니다".into()) };
    let Some(def) = &r.def else { return Err("내장 멤버와 타입은 이름을 바꿀 수 없습니다".into()) };
    if matches!(r.kind, SymKind::Module) {
        return Err("모듈 이름은 파일 이름이므로 여기서 바꿀 수 없습니다".into());
    }
    let (file, _, _) = def_position(&a.sources, def);
    let label = a.sources.files.get(file as usize).map(|f| f.0.clone()).unwrap_or_default();
    let decl_path = PathBuf::from(&label);
    if label.starts_with('<') || module_context(&decl_path, "", ws).stdlib || decl_path.starts_with(stdlib_dir()) {
        return Err("표준 라이브러리의 선언은 이름을 바꿀 수 없습니다".into());
    }
    if matches!(r.kind, SymKind::Class | SymKind::Interface | SymKind::Constructor) && decl_path.file_stem().map(|s| s.to_string_lossy() == def.1).unwrap_or(false) {
        return Err(format!("'{}'은(는) 모듈 파일 이름과 같은 타입이라 이름을 바꾸면 using 경로가 깨집니다. 파일 이름과 함께 바꿔 주세요", def.1));
    }
    let name: String = text::line_of(src, line as usize - 1).chars().skip(r.col as usize - 1).take(r.len as usize).collect();
    if name != def.1 {
        return Err(format!("'{}'은(는) 별칭이라 여기서 이름을 바꿀 수 없습니다", name));
    }
    Ok((name, r.line, r.col, r.len))
}

/// The places to replace in each file: (line, col, length).
pub type FileEdits = Vec<(PathBuf, Vec<(u32, u32, u32)>)>;

/// The edits renaming the symbol at (line, col) to `new_name`, by file.
pub fn rename(path: &Path, src: &str, ws: &Workspace, line: u32, col: u32, new_name: &str) -> Result<FileEdits, String> {
    let (old, _, _, _) = prepare_rename(path, src, ws, line, col)?;
    if !valid_name(new_name) {
        return Err(format!("'{}'은(는) 이름으로 쓸 수 없습니다", new_name));
    }
    let Some((r, occ)) = occurrences(path, src, ws, line, col, true) else { return Err("기호를 찾지 못했습니다".into()) };
    if matches!(r.kind, SymKind::Class | SymKind::Interface | SymKind::Constructor) && super::builtins::TYPES.iter().any(|(n, _)| *n == new_name) {
        return Err(format!("'{}'은(는) 내장 타입 이름입니다", new_name));
    }
    let mut out: FileEdits = Vec::new();
    for ((p, l, c, n), _) in occ {
        // only places that spell the old name (not an alias of it)
        let text = if path_key(&p) == path_key(path) { Some(src.to_string()) } else { ws.overlays.get(&p).cloned().or_else(|| std::fs::read_to_string(&p).ok()) };
        let Some(text) = text else { continue };
        let at: String = text::line_of(&text, l as usize - 1).chars().skip(c as usize - 1).take(n as usize).collect();
        if at != old {
            continue;
        }
        match out.iter_mut().find(|(q, _)| path_key(q) == path_key(&p)) {
            Some((_, v)) => v.push((l, c, n)),
            None => out.push((p, vec![(l, c, n)])),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------- completion
pub struct Completion {
    pub items: Vec<CompItem>,
    /// Line and character range (1-based, end exclusive) of the word being replaced.
    pub line: u32,
    pub start: u32,
    pub end: u32,
}

/// Openers on `line[..end]` (outside strings and comments) that are still open, innermost last.
fn open_brackets(line: &[char], end: usize) -> Vec<char> {
    let code = text::code_positions(line);
    let mut stack = Vec::new();
    for (i, &c) in line.iter().enumerate().take(end) {
        if !(code[i] && code[i + 1]) {
            continue;
        }
        match c {
            '(' | '[' | '{' => stack.push(c),
            ')' | ']' | '}' => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack
}

fn closers(open: &[char]) -> String {
    open.iter()
        .rev()
        .filter(|c| **c != '{')
        .map(|c| match c {
            '(' => ')',
            _ => ']',
        })
        .collect()
}

fn replace_line(src: &str, line: u32, new: &str) -> String {
    src.split('\n').enumerate().map(|(i, l)| if i + 1 == line as usize { new.to_string() } else { l.to_string() }).collect::<Vec<_>>().join("\n")
}

fn module_items(prefix: &str, base: Option<&Path>) -> Vec<CompItem> {
    let (pkg, _) = prefix.rsplit_once('.').unwrap_or(("", prefix));
    let pre = if pkg.is_empty() { String::new() } else { format!("{}.", pkg) };
    let mut names: Vec<(String, bool)> = Vec::new(); // (next segment, is a module)
    let mut add = |full: &str| {
        if let Some(rest) = full.strip_prefix(&pre) {
            let (seg, module) = match rest.split_once('.') {
                Some((s, _)) => (s, false),
                None => (rest, true),
            };
            if !names.iter().any(|(n, m)| n == seg && *m == module) {
                names.push((seg.to_string(), module));
            }
        }
    };
    for (n, _) in STDLIB {
        add(n);
    }
    if let Some(b) = base {
        let mut dir = b.to_path_buf();
        for p in pkg.split('.').filter(|p| !p.is_empty()) {
            dir.push(p);
        }
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                if p.is_dir() && !name.starts_with('.') && name != "target" {
                    add(&format!("{}{}.x", pre, name));
                } else if p.extension().map(|e| e == SOURCE_EXT).unwrap_or(false) {
                    add(&format!("{}{}", pre, name));
                }
            }
        }
    }
    if pkg.is_empty() {
        add("stdio");
    }
    names.sort();
    names
        .into_iter()
        .map(|(n, module)| CompItem {
            label: n.clone(),
            kind: if module { SymKind::Class } else { SymKind::Module },
            detail: if module { format!("module {}{}", pre, n) } else { format!("package {}{}", pre, n) },
            doc: None,
            def: None,
            takes_args: None,
        })
        .collect()
}

fn directive_items() -> Vec<CompItem> {
    [
        ("using", "@using sdk 1", "사용할 SDK 버전 (2.3)"),
        ("runtime", "@runtime compiler interpreter bytecode", "실행 가능한 런타임 (2.4)"),
        ("compiler", "@compiler(Target=[amd64], IncludeDependencies=false, MemoryManagement=ownership)", "컴파일러 옵션 (2.5)"),
        ("runtimecfg", "@runtimecfg(IntegerOverflow=error)", "런타임 정책 (2.6)"),
    ]
    .iter()
    .map(|(l, d, doc)| CompItem { label: l.to_string(), kind: SymKind::Keyword, detail: d.to_string(), doc: Some(doc.to_string()), def: None, takes_args: None })
    .collect()
}

/// Completion candidates at (line, col).
pub fn complete(path: &Path, src: &str, ws: &Workspace, line: u32, col: u32) -> Option<Completion> {
    let cur: Vec<char> = text::line_of(src, line.checked_sub(1)? as usize).chars().collect();
    let ci = (col.saturating_sub(1) as usize).min(cur.len());
    if text::in_string_or_comment(&cur, ci) {
        return None;
    }
    let mut start = ci;
    while start > 0 && is_ident_char(cur[start - 1]) {
        start -= 1;
    }
    let mut end = ci;
    while end < cur.len() && is_ident_char(cur[end]) {
        end += 1;
    }
    let before: String = cur[..start].iter().collect();
    let result = |items: Vec<CompItem>| Some(Completion { items, line, start: start as u32 + 1, end: end as u32 + 1 });
    let trimmed = before.trim_start();
    // `using a.b.` : modules and packages
    if let Some(rest) = trimmed.strip_prefix("using ") {
        let typed: String = rest.trim_start().chars().chain(cur[start..ci].iter().copied()).collect();
        if typed.chars().all(|c| is_ident_char(c) || c == '.') && !typed.contains(" as ") {
            let m = module_context(path, src, ws);
            return result(module_items(&typed, m.base.as_deref()));
        }
        return None;
    }
    if trimmed == "@" {
        return result(directive_items());
    }
    if trimmed.starts_with('@') {
        return None;
    }
    let member = start > 0 && cur[start - 1] == '.';
    let after_new = {
        let b = before.trim_end();
        b.ends_with("new") && b[..b.len() - 3].chars().last().map(|c| !is_ident_char(c)).unwrap_or(true)
    };
    let head: String = cur[..ci].iter().collect();
    let tail: String = cur[ci..].iter().collect();
    let mark = if after_new && !tail.trim_start_matches(is_ident_char).starts_with(['(', '[']) { format!("{}()", MARK) } else { MARK.to_string() };
    let open = open_brackets(&cur, ci);
    let full_open = open_brackets(&cur, cur.len());
    let mut variants: Vec<String> = vec![format!("{}{}{}{}", head, mark, tail, closers(&full_open)), format!("{}{}{}", head, mark, closers(&open))];
    if !member && !after_new {
        // a type: of a parameter, of a declaration (also as a type argument), of a function
        variants.push(format!("{}{} __l2v{}", head, mark, closers(&open)));
        variants.push(format!("{}{}{} __l2v", head, mark, closers(&open)));
        variants.push(format!("{}{} __l2v() {{}}", head, mark));
        variants.push(format!("{}{} __l2v{} {{}}", head, mark, closers(&open)));
        // a fresh statement line, for the names in scope
        let indent: String = cur.iter().take_while(|c| c.is_whitespace()).collect();
        variants.push(format!("{}{}", indent, MARK));
    }
    let target = (line, start as u32 + 1);
    for v in variants {
        let patched = replace_line(src, line, &v);
        let target = if v.trim_start() == MARK { (line, v.chars().take_while(|c| c.is_whitespace()).count() as u32 + 1) } else { target };
        let a = analyze(path, &patched, ws, Want::Completion, Some(target));
        if let Some(items) = a.completion {
            return result(items);
        }
    }
    None
}

// ---------------------------------------------------------------------- signature help
pub struct SignatureHelp {
    pub sigs: Vec<SigInfo>,
    pub active_sig: usize,
    pub active_param: usize,
}

/// The unclosed `(` of the call around the cursor: ((line, index), commas before the cursor).
fn enclosing_call(lines: &[Vec<char>], line: usize, ci: usize) -> Option<((usize, usize), usize)> {
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut l = line;
    let mut i = ci;
    for _ in 0..40 {
        let cur = &lines[l];
        let code = text::code_positions(cur);
        while i > 0 {
            i -= 1;
            if !(code[i] && code[i + 1]) {
                continue;
            }
            match cur[i] {
                ')' | ']' | '}' => depth += 1,
                '(' if depth == 0 => return Some(((l, i), commas)),
                '[' | '{' if depth == 0 => return None,
                '(' | '[' | '{' => depth -= 1,
                ',' if depth == 0 => commas += 1,
                _ => {}
            }
        }
        if l == 0 {
            return None;
        }
        l -= 1;
        i = lines[l].len();
    }
    None
}

/// Signatures of the call around (line, col).
pub fn signature_help(path: &Path, src: &str, ws: &Workspace, line: u32, col: u32) -> Option<SignatureHelp> {
    let lines: Vec<Vec<char>> = src.split('\n').map(|l| l.trim_end_matches('\r').chars().collect()).collect();
    let li = line.checked_sub(1)? as usize;
    let cur = lines.get(li)?;
    let ci = (col.saturating_sub(1) as usize).min(cur.len());
    if text::in_string_or_comment(cur, ci) {
        return None;
    }
    let ((pl, pi), active) = enclosing_call(&lines, li, ci)?;
    // the callee name before `(`, skipping type arguments
    let pline = &lines[pl];
    let mut k = pi;
    while k > 0 && pline[k - 1].is_whitespace() {
        k -= 1;
    }
    if k > 0 && pline[k - 1] == ']' {
        let mut d = 0;
        while k > 0 {
            k -= 1;
            match pline[k] {
                ']' => d += 1,
                '[' => {
                    d -= 1;
                    if d == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    let (s, e) = text::word_at(pline, k.checked_sub(1)?)?;
    let name: String = pline[s..e].iter().collect();
    if e != k || matches!(name.as_str(), "if" | "for" | "switch" | "catch" | "function" | "return" | "and" | "or" | "not" | "in") {
        return None;
    }
    let target = (pl as u32 + 1, s as u32 + 1);
    let head: String = cur[..ci].iter().collect();
    let tail: String = cur[ci..].iter().collect();
    // an empty argument slot gets a name, so that the call parses
    let prev = head.trim_end().chars().last();
    let next = tail.trim_start().chars().next();
    let mark = if matches!(prev, Some('(') | Some(',')) && matches!(next, None | Some(')') | Some(',')) { MARK } else { "" };
    let open = if pl == li { open_brackets(cur, ci) } else { vec!['('] };
    let variants = [format!("{}{}{}", head, mark, tail), format!("{}{}{}", head, mark, closers(&open))];
    for v in variants {
        let patched = replace_line(src, line, &v);
        let a = analyze(path, &patched, ws, Want::Signature, Some(target));
        if let Some(sigs) = a.signatures {
            if sigs.is_empty() {
                return None;
            }
            let fits = |s: &SigInfo| s.params.len() > active || s.params.last().map(|p| p.ends_with("...")).unwrap_or(false) || (active == 0 && s.params.is_empty());
            let active_sig = sigs.iter().position(fits).unwrap_or(0);
            return Some(SignatureHelp { sigs, active_sig, active_param: active });
        }
    }
    None
}

// ---------------------------------------------------------------------- document symbols
pub struct Symbol {
    pub name: String,
    pub detail: String,
    pub kind: SymKind,
    /// 1-based start (line, col) of the declaration and of its name, and the 0-based end.
    pub start: (u32, u32),
    pub name_pos: (u32, u32),
    pub end: (u32, u32),
    pub children: Vec<Symbol>,
}

fn symbol(src: &str, span: Span, name: &str, detail: String, kind: SymKind, block: bool) -> Symbol {
    let line: Vec<char> = text::line_of(src, span.line.saturating_sub(1) as usize).chars().collect();
    let nc = text::find_word(&line, span.col.saturating_sub(1) as usize, name).map(|c| c as u32 + 1).unwrap_or(span.col);
    let end = if block { text::block_end(src, span.line, span.col) } else { None };
    let end = end.unwrap_or((span.line.saturating_sub(1), line.len() as u32));
    Symbol { name: name.to_string(), detail, kind, start: (span.line, span.col.max(1)), name_pos: (span.line, nc), end, children: Vec::new() }
}

/// Outline of a file: classes with their members, interfaces and functions.
pub fn document_symbols(src: &str) -> Vec<Symbol> {
    use crate::check::ide::{decl_signature, type_text};
    let mut sources = SourceMap::default();
    let id = sources.add(String::new(), src.to_string());
    let toks = match crate::lexer::lex(src, id) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let (f, _) = crate::parser::parse_file_recovering(toks, id);
    let mut out = Vec::new();
    for it in &f.items {
        match it {
            Item::Class(c) => {
                let mut s = symbol(src, c.span, &c.name, String::new(), SymKind::Class, true);
                for fd in &c.fields {
                    s.children.push(symbol(src, fd.span, &fd.name, type_text(&fd.ty), SymKind::Field, false));
                }
                for k in &c.ctors {
                    let params: Vec<String> = k.params.iter().map(|p| format!("{} {}", type_text(&p.ty), p.name)).collect();
                    s.children.push(symbol(src, k.span, &c.name, format!("({})", params.join(", ")), SymKind::Constructor, true));
                }
                for m in &c.methods {
                    let d = decl_signature(m, None).1.join(", ");
                    s.children.push(symbol(src, m.span, &m.name, format!("({}) -> {}", d, type_text(&m.ret)), SymKind::Method, m.body.is_some()));
                }
                s.children.sort_by_key(|x| x.start);
                out.push(s);
            }
            Item::Interface(d) => {
                let mut s = symbol(src, d.span, &d.name, String::new(), SymKind::Interface, true);
                for m in &d.methods {
                    let p = decl_signature(m, None).1.join(", ");
                    s.children.push(symbol(src, m.span, &m.name, format!("({}) -> {}", p, type_text(&m.ret)), SymKind::Method, m.body.is_some()));
                }
                out.push(s);
            }
            Item::Function(d) => {
                let p = decl_signature(d, None).1.join(", ");
                out.push(symbol(src, d.span, &d.name, format!("({}) -> {}", p, type_text(&d.ret)), SymKind::Function, d.body.is_some()));
            }
            Item::Using(_) => {}
        }
    }
    out
}
