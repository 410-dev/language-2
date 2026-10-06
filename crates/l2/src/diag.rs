//! Source positions and diagnostics.

use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: u32,
    pub line: u32,
    pub col: u32,
}

impl Span {
    pub fn new(file: u32, line: u32, col: u32) -> Span {
        Span { file, line, col }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub severity: Severity,
    pub span: Span,
    pub msg: String,
}

impl Diag {
    pub fn error(span: Span, msg: impl Into<String>) -> Diag {
        Diag { severity: Severity::Error, span, msg: msg.into() }
    }
    pub fn warning(span: Span, msg: impl Into<String>) -> Diag {
        Diag { severity: Severity::Warning, span, msg: msg.into() }
    }
}

/// The set of source files of a compilation, used to render diagnostics.
#[derive(Default, Debug, Clone)]
pub struct SourceMap {
    pub files: Vec<(String, String)>, // (path, contents)
}

impl SourceMap {
    pub fn add(&mut self, path: String, src: String) -> u32 {
        self.files.push((path, src));
        (self.files.len() - 1) as u32
    }
    pub fn path(&self, id: u32) -> &str {
        self.files.get(id as usize).map(|f| f.0.as_str()).unwrap_or("<unknown>")
    }
    pub fn render(&self, d: &Diag) -> String {
        let kind = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let mut s = format!("{}: {}\n  --> {}:{}:{}", kind, d.msg, self.path(d.span.file), d.span.line, d.span.col);
        if let Some((_, src)) = self.files.get(d.span.file as usize) {
            if let Some(line) = src.lines().nth(d.span.line.saturating_sub(1) as usize) {
                s.push_str(&format!("\n   | {}\n   | {}^", line, " ".repeat(d.span.col.saturating_sub(1) as usize)));
            }
        }
        s
    }
}

/// Errors from a compilation stage.
#[derive(Debug, Clone)]
pub struct Diags(pub Vec<Diag>);

impl fmt::Display for Diags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for d in &self.0 {
            writeln!(f, "{}:{}:{}: {}", d.span.file, d.span.line, d.span.col, d.msg)?;
        }
        Ok(())
    }
}
