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
            Item::Using(u) => out.push(u.path.clone()),
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
        if let StmtKind::Using(u) = &s.kind {
            out.push(u.path.clone());
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
    let mut stdlib = vec![false, false];
    let mut queue: VecDeque<usize> = VecDeque::from([1]);
    let mut diags = Vec::new();
    while let Some(i) = queue.pop_front() {
        for path in imports_of(&files[i]) {
            if path == "stdio" || path == "intrinsics" {
                continue;
            }
            for (name, src) in modules_for(&path, base) {
                if names.contains(&name) {
                    continue;
                }
                let (label, text, is_std) = match src {
                    Source::File(p) => match std::fs::read_to_string(&p) {
                        Ok(t) => (p.display().to_string(), t, false),
                        Err(_) => continue, // reported by the checker as an unknown module
                    },
                    Source::Std(t) => (format!("<stdlib>/{}.{}", name.replace('.', "/"), SOURCE_EXT), t.to_string(), true),
                };
                match parse(&mut sources, label, text) {
                    Ok(f) => {
                        files.push(f);
                        names.push(name);
                        stdlib.push(is_std);
                        queue.push_back(files.len() - 1);
                    }
                    Err(d) => diags.push(d),
                }
            }
        }
    }
    if !diags.is_empty() {
        return Err(Failure { sources, diags });
    }
    match crate::check::check_program(&files, &names, 1, &stdlib) {
        Ok(out) => Ok(Compilation { program: out.program, sources, warnings: out.warnings }),
        Err(diags) => Err(Failure { sources, diags }),
    }
}

/// Standard library modules (spec 14.4), embedded in the compiler.
pub const STDLIB: &[(&str, &str)] = &[
    ("math.linear.Tensor", include_str!("../stdlib/math/linear/Tensor.l2")),
    ("math.linear.Matrix", include_str!("../stdlib/math/linear/Matrix.l2")),
    ("math.linear.Vector", include_str!("../stdlib/math/linear/Vector.l2")),
];

enum Source {
    File(PathBuf),
    Std(&'static str),
}

fn package_of(module: &str) -> &str {
    module.rsplit_once('.').map(|(p, _)| p).unwrap_or("")
}

fn user_module(base: Option<&Path>, module: &str) -> Option<PathBuf> {
    let mut p: PathBuf = base?.to_path_buf();
    for part in module.split('.') {
        p.push(part);
    }
    p.set_extension(SOURCE_EXT);
    p.is_file().then_some(p)
}

/// All modules of a package: the source files of a project directory, or standard library
/// modules. User files take precedence over standard library modules of the same name.
fn package_modules(base: Option<&Path>, package: &str) -> Vec<(String, Source)> {
    let mut out: Vec<(String, Source)> = Vec::new();
    if package.is_empty() {
        return out;
    }
    if let Some(b) = base {
        let mut dir = b.to_path_buf();
        for part in package.split('.') {
            dir.push(part);
        }
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_file() && p.extension().map(|e| e == SOURCE_EXT).unwrap_or(false)).collect();
            files.sort();
            for f in files {
                let stem = f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                out.push((format!("{}.{}", package, stem), Source::File(f)));
            }
        }
    }
    for (name, src) in STDLIB {
        if package_of(name) == package && !out.iter().any(|(n, _)| n == name) {
            out.push((name.to_string(), Source::Std(src)));
        }
    }
    out
}

fn single_module(base: Option<&Path>, module: &str) -> Option<Source> {
    if let Some(p) = user_module(base, module) {
        return Some(Source::File(p));
    }
    STDLIB.iter().find(|(n, _)| *n == module).map(|(_, s)| Source::Std(s))
}

/// The modules to load for `using path`: the module itself (`a.b.C` -> `a/b/C.l2`), a whole
/// package (`a.b` / `a.b.*`), or the module / package declaring a type (`a.b.Type`). Modules
/// of a named package bring their whole package along, so that types of the same package can
/// refer to each other.
fn modules_for(path: &str, base: Option<&Path>) -> Vec<(String, Source)> {
    let mut out: Vec<(String, Source)> = Vec::new();
    let add_package = |out: &mut Vec<(String, Source)>, pkg: &str| {
        for (n, s) in package_modules(base, pkg) {
            if !out.iter().any(|(m, _)| *m == n) {
                out.push((n, s));
            }
        }
    };
    let candidates = [Some(path), path.rsplit_once('.').map(|(p, _)| p)];
    for cand in candidates.into_iter().flatten() {
        if let Some(src) = single_module(base, cand) {
            out.push((cand.to_string(), src));
            add_package(&mut out, package_of(cand));
            return out;
        }
        let pkg = package_modules(base, cand);
        if !pkg.is_empty() {
            add_package(&mut out, cand);
            return out;
        }
    }
    out
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
