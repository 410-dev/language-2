//! Command-line interface of language-2.

use l2::driver;
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "{name} {ver}

USAGE:
    {name} run [--backend interpreter|bytecode|compiler] <file> [args...]
    {name} check <file>
",
        name = l2::LANG_NAME,
        ver = env!("CARGO_PKG_VERSION")
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return usage();
    }
    let cmd = args[0].as_str();
    let mut rest = args[1..].to_vec();
    let mut backend = "interpreter".to_string();
    if let Some(i) = rest.iter().position(|a| a == "--backend" || a == "-b") {
        if i + 1 < rest.len() {
            backend = rest[i + 1].clone();
            rest.drain(i..=i + 1);
        }
    }
    if rest.is_empty() {
        return usage();
    }
    let path = PathBuf::from(&rest[0]);
    let prog_args = rest[1..].to_vec();
    let comp = match driver::compile_file(&path) {
        Ok(c) => c,
        Err(f) => {
            eprint!("{}", f.render());
            return ExitCode::from(1);
        }
    };
    for w in &comp.warnings {
        eprintln!("{}", comp.sources.render(w));
    }
    match cmd {
        "check" => ExitCode::SUCCESS,
        "disasm" => {
            let m = l2::bytecode::compile(&comp.program);
            print!("{}", l2::bytecode::disassemble(&comp.program, &m));
            ExitCode::SUCCESS
        }
        "run" => {
            let p = comp.program;
            let code = std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn(move || match backend.as_str() {
                    "bytecode" | "vm" => l2::vm::run(&p, prog_args),
                    _ => l2::interp::run(&p, prog_args),
                })
                .unwrap()
                .join()
                .unwrap_or(101);
            ExitCode::from(code as u8)
        }
        _ => usage(),
    }
}
