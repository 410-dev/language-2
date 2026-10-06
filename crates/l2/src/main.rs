//! Command-line interface of language-2.

use l2::driver;
use l2::hir::{Backend, Program, Target};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "{name} {ver}

USAGE:
    {name} run [--backend interpreter|bytecode|compiler] <file> [args...]
    {name} build <file> [-o <output>] [--target i386|amd64|arm64]... [-O0|-O1|-O2]
    {name} check <file>
    {name} emit-llvm <file> [--target amd64] [-o <file.ll>]
    {name} disasm <file>
    {name} doctor

Backends: 'interpreter' (tree-walking, default), 'bytecode' (bytecode VM), 'compiler' (LLVM native).
The program's '@runtime' directive lists which backends it supports.",
        name = l2::LANG_NAME,
        ver = env!("CARGO_PKG_VERSION")
    );
    ExitCode::from(2)
}

struct Opts {
    backend: String,
    output: Option<PathBuf>,
    targets: Vec<Target>,
    opt_level: u8,
    quiet: bool,
    positional: Vec<String>,
}

fn parse_opts(args: &[String], passthrough: bool) -> Result<Opts, String> {
    let mut o = Opts { backend: "interpreter".into(), output: None, targets: Vec::new(), opt_level: 2, quiet: false, positional: Vec::new() };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        // everything after the source file belongs to the program
        if passthrough && !o.positional.is_empty() {
            o.positional.push(a.clone());
            i += 1;
            continue;
        }
        match a.as_str() {
            "--backend" | "-b" => {
                i += 1;
                o.backend = args.get(i).cloned().ok_or("--backend needs a value")?;
            }
            "-o" | "--output" => {
                i += 1;
                o.output = Some(PathBuf::from(args.get(i).ok_or("-o needs a value")?));
            }
            "--target" | "-t" => {
                i += 1;
                let t = args.get(i).ok_or("--target needs a value")?;
                o.targets.push(match t.as_str() {
                    "i386" => Target::I386,
                    "amd64" => Target::Amd64,
                    "arm64" => Target::Arm64,
                    other => return Err(format!("unknown target '{}'", other)),
                });
            }
            "-O0" => o.opt_level = 0,
            "-O1" => o.opt_level = 1,
            "-O2" | "-O" => o.opt_level = 2,
            "-q" | "--quiet" => o.quiet = true,
            _ => o.positional.push(a.clone()),
        }
        i += 1;
    }
    Ok(o)
}

fn compile(path: &Path, quiet: bool) -> Result<driver::Compilation, ExitCode> {
    match driver::compile_file(path) {
        Ok(c) => {
            if !quiet {
                for w in &c.warnings {
                    eprintln!("{}", c.sources.render(w));
                }
            }
            Ok(c)
        }
        Err(f) => {
            eprint!("{}", f.render());
            Err(ExitCode::from(1))
        }
    }
}

fn with_big_stack<F: FnOnce() -> i32 + Send + 'static>(f: F) -> i32 {
    std::thread::Builder::new().stack_size(1 << 30).spawn(f).unwrap().join().unwrap_or(101)
}

fn exit(code: i32) -> ExitCode {
    ExitCode::from((code & 0xff) as u8)
}

fn backend_allowed(p: &Program, b: Backend) -> bool {
    p.config.runtimes.contains(&b)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().cloned() else {
        return usage();
    };
    let o = match parse_opts(&args[1..], cmd == "run") {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {}", e);
            return ExitCode::from(2);
        }
    };
    match cmd.as_str() {
        "doctor" => return doctor(o.quiet),
        "help" | "--help" | "-h" => {
            usage();
            return ExitCode::SUCCESS;
        }
        "version" | "--version" | "-V" => {
            println!("{} {}", l2::LANG_NAME, env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let Some(file) = o.positional.first() else {
        return usage();
    };
    let path = PathBuf::from(file);
    let prog_args: Vec<String> = o.positional[1..].to_vec();
    let comp = match compile(&path, o.quiet) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let p = comp.program;
    match cmd.as_str() {
        "check" => ExitCode::SUCCESS,
        "disasm" => {
            let m = l2::bytecode::compile(&p);
            print!("{}", l2::bytecode::disassemble(&p, &m));
            ExitCode::SUCCESS
        }
        "emit-llvm" => {
            let t = o.targets.first().copied().unwrap_or_else(l2::native::host_target);
            let ir = l2::llvm::emit_module(&p, t);
            match o.output {
                Some(out) => {
                    if let Err(e) = std::fs::write(&out, ir) {
                        eprintln!("error: cannot write {}: {}", out.display(), e);
                        return ExitCode::from(1);
                    }
                }
                None => print!("{}", ir),
            }
            ExitCode::SUCCESS
        }
        "build" => {
            if !backend_allowed(&p, Backend::Compiler) {
                eprintln!("error: this program does not list 'compiler' in its @runtime directive");
                return ExitCode::from(1);
            }
            let targets = if o.targets.is_empty() { p.config.targets.clone() } else { o.targets.clone() };
            let stem = o.output.clone().map(|p| p.with_extension("")).unwrap_or_else(|| PathBuf::from(path.file_stem().unwrap()));
            let suffix = targets.len() > 1;
            match l2::native::build(&p, &targets, &stem, o.opt_level, suffix) {
                Ok(results) => {
                    for r in results {
                        match &r.executable {
                            Some(e) => println!("[{}] {}", r.target.name(), e.display()),
                            None => println!("[{}] {}", r.target.name(), r.object.display()),
                        }
                        if let Some(n) = r.note {
                            eprintln!("  note: {}", n);
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    ExitCode::from(1)
                }
            }
        }
        "run" => {
            let backend = match o.backend.as_str() {
                "interpreter" | "interp" | "i" => Backend::Interpreter,
                "bytecode" | "vm" | "b" => Backend::Bytecode,
                "compiler" | "native" | "c" => Backend::Compiler,
                other => {
                    eprintln!("error: unknown backend '{}'", other);
                    return ExitCode::from(2);
                }
            };
            if !backend_allowed(&p, backend) {
                eprintln!("error: this program does not support the '{}' runtime (see its @runtime directive)", o.backend);
                return ExitCode::from(1);
            }
            match backend {
                Backend::Interpreter => exit(with_big_stack(move || l2::interp::run(&p, prog_args))),
                Backend::Bytecode => exit(with_big_stack(move || l2::vm::run(&p, prog_args))),
                Backend::Compiler => run_native(&p, &path, prog_args, o.opt_level, o.targets.first().copied()),
            }
        }
        _ => usage(),
    }
}

fn run_native(p: &Program, path: &Path, args: Vec<String>, opt: u8, target: Option<Target>) -> ExitCode {
    let dir = std::env::temp_dir().join(format!("{}-run-{}", l2::LANG_NAME, std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let stem = dir.join(path.file_stem().unwrap());
    let target = target.unwrap_or_else(l2::native::host_target);
    let results = match l2::native::build(p, &[target], &stem, opt, false) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {}", e);
            return ExitCode::from(1);
        }
    };
    let Some(exe) = results.into_iter().next().and_then(|r| {
        if let Some(n) = &r.note {
            eprintln!("error: {}", n);
        }
        r.executable
    }) else {
        return ExitCode::from(1);
    };
    let status = std::process::Command::new(&exe).args(&args).status();
    let _ = std::fs::remove_dir_all(&dir);
    match status {
        Ok(s) => exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("error: cannot run {}: {}", exe.display(), e);
            ExitCode::from(1)
        }
    }
}

fn doctor(quiet: bool) -> ExitCode {
    let mut ok = true;
    let say = |s: String| {
        if !quiet {
            println!("{}", s);
        }
    };
    match l2::native::find_toolchain() {
        Ok(tc) => {
            say(format!("llc:     {}", tc.llc.display()));
            say(format!("opt:     {}", tc.opt.map(|p| p.display().to_string()).unwrap_or("(not found; IR is not pre-optimised)".into())));
        }
        Err(e) => {
            say(format!("llc:     missing ({})", e));
            ok = false;
        }
    }
    for t in [Target::Amd64, Target::I386, Target::Arm64] {
        match l2::native::find_runtime(t) {
            Some(p) => say(format!("runtime [{}]: {}", t.name(), p.display())),
            None => {
                say(format!("runtime [{}]: not built (objects only)", t.name()));
                if t == l2::native::host_target() {
                    ok = false;
                }
            }
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
