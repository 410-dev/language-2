//! Front-end driver: loads the entry file and the modules it imports, parses and checks them.

use crate::ast::{FileAst, Item, StmtKind};
use crate::diag::{Diag, Severity, SourceMap};
use crate::hir::Program;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

pub const PRELUDE: &str = include_str!("prelude.l2");
/// File extension of source files.
pub const SOURCE_EXT: &str = "l2";

pub struct Compilation {
    pub program: Program,
    pub sources: SourceMap,
    pub warnings: Vec<Diag>,
}

pub struct Failure {
    pub sources: SourceMap,
    pub diags: Vec<Diag>,
}

impl Failure {
    pub fn render(&self) -> String {
        let mut out = String::new();
        for d in &self.diags {
            out.push_str(&self.sources.render(d));
            out.push('\n');
        }
        let n = self.diags.iter().filter(|d| d.severity == Severity::Error).count();
        out.push_str(&format!("error: aborting due to {} previous error{}\n", n, if n == 1 { "" } else { "s" }));
        out
    }
}

fn parse(sources: &mut SourceMap, path: String, src: String) -> Result<FileAst, Diag> {
    let id = sources.add(path, src.clone());
    let toks = crate::lexer::lex(&src, id)?;
    crate::parser::parse_file(toks, id)
}

fn imports_of(f: &FileAst) -> Vec<String> {
    let mut out = Vec::new();
    for it in &f.items {
        match it {
            Item::Using { module, .. } => out.push(module.clone()),
            Item::Function(d) => {
                if let Some(b) = &d.body {
                    collect_stmt_imports(&b.stmts, &mut out);
                }
            }
            Item::Class(c) => {
                for m in &c.methods {
                    if let Some(b) = &m.body {
                        collect_stmt_imports(&b.stmts, &mut out);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn collect_stmt_imports(stmts: &[crate::ast::Stmt], out: &mut Vec<String>) {
    for s in stmts {
        if let StmtKind::Using { module, .. } = &s.kind {
            out.push(module.clone());
        }
    }
}

/// Compiles a program given the entry source text. `base` is the directory used to resolve
/// imported modules (`using foo` loads `<base>/foo.l2`).
pub fn compile_source(entry_name: &str, src: &str, base: Option<&Path>) -> Result<Compilation, Failure> {
    let mut sources = SourceMap::default();
    let mut files = Vec::new();
    let mut names = Vec::new();
    let prelude = parse(&mut sources, "<prelude>".into(), PRELUDE.into()).map_err(|d| Failure { sources: sources.clone(), diags: vec![d] })?;
    files.push(prelude);
    names.push("<prelude>".to_string());
    let entry = match parse(&mut sources, entry_name.to_string(), src.to_string()) {
        Ok(f) => f,
        Err(d) => return Err(Failure { sources, diags: vec![d] }),
    };
    let stem = Path::new(entry_name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    files.push(entry);
    names.push(stem);
    let mut queue: VecDeque<usize> = VecDeque::from([1]);
    let mut diags = Vec::new();
    while let Some(i) = queue.pop_front() {
        for m in imports_of(&files[i]) {
            if m == "stdio" || names.contains(&m) {
                continue;
            }
            let Some(base) = base else { continue };
            let mut p: PathBuf = base.to_path_buf();
            for part in m.split('.') {
                p.push(part);
            }
            p.set_extension(SOURCE_EXT);
            match std::fs::read_to_string(&p) {
                Ok(text) => match parse(&mut sources, p.display().to_string(), text) {
                    Ok(f) => {
                        files.push(f);
                        names.push(m.clone());
                        queue.push_back(files.len() - 1);
                    }
                    Err(d) => diags.push(d),
                },
                Err(_) => {} // reported by the checker as an unknown module
            }
        }
    }
    if !diags.is_empty() {
        return Err(Failure { sources, diags });
    }
    match crate::check::check_program(&files, &names, 1) {
        Ok(out) => Ok(Compilation { program: out.program, sources, warnings: out.warnings }),
        Err(diags) => Err(Failure { sources, diags }),
    }
}

pub fn compile_file(path: &Path) -> Result<Compilation, Failure> {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return Err(Failure { sources: SourceMap::default(), diags: vec![Diag::error(Default::default(), format!("cannot read {}: {}", path.display(), e))] });
        }
    };
    compile_source(&path.display().to_string(), &src, path.parent())
}
