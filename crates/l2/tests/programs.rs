//! Differential tests (spec 15.3): every program in `tests/programs` is run on each backend and
//! must produce the expected stdout / stderr / exit code. The tree-walking interpreter's output
//! is the reference. Programs in `tests/errors` must fail to compile with the given message.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("tests")
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_language-2")
}

fn programs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(root().join("programs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "l2").unwrap_or(false) && p.with_extension("out").exists())
        .collect();
    v.sort();
    v
}

fn expected_exit(src: &str) -> i32 {
    src.lines().find_map(|l| l.trim().strip_prefix("// expect-exit:").map(|v| v.trim().parse().unwrap())).unwrap_or(0)
}

fn native_available() -> bool {
    Command::new(bin()).args(["doctor", "--quiet"]).status().map(|s| s.success()).unwrap_or(false)
}

fn run_backend(backend: &str) {
    let mut failures = Vec::new();
    for p in programs() {
        let src = std::fs::read_to_string(&p).unwrap();
        if backend == "compiler" && src.contains("@compiler(MemoryManagement=manual)") && src.contains("useAfterFree") {
            // invalid memory access is undefined behaviour in native code (spec 15.3)
            continue;
        }
        let out = Command::new(bin()).args(["run", "--backend", backend]).arg(&p).env("L2_SEED", "7").output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
        let stderr = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
        let want = std::fs::read_to_string(p.with_extension("out")).unwrap().replace("\r\n", "\n");
        let code = out.status.code().unwrap_or(-1);
        let mut ok = stdout == want && code == expected_exit(&src);
        if let Ok(err) = std::fs::read_to_string(p.with_extension("err")) {
            ok &= stderr.trim_end() == err.replace("\r\n", "\n").trim_end();
        }
        if !ok {
            failures.push(format!("{} [{}] exit={}\n--- stdout ---\n{}\n--- stderr ---\n{}", p.display(), backend, code, stdout, stderr));
        }
    }
    assert!(failures.is_empty(), "{} failure(s):\n{}", failures.len(), failures.join("\n\n"));
}

#[test]
fn interpreter() {
    run_backend("interpreter");
}

#[test]
fn bytecode_vm() {
    run_backend("bytecode");
}

#[test]
fn native_compiler() {
    if !native_available() {
        eprintln!("skipping native tests: LLVM tools (llc) or a linker were not found");
        return;
    }
    run_backend("compiler");
}

#[test]
fn compile_errors() {
    let mut failures = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(root().join("errors")).unwrap().filter_map(|e| e.ok().map(|e| e.path())).collect();
    files.sort();
    for p in files {
        let src = std::fs::read_to_string(&p).unwrap();
        let want = src.lines().next().and_then(|l| l.strip_prefix("// error: ")).expect("first line must be `// error: ...`").trim().to_string();
        let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() || !stderr.contains(&want) {
            failures.push(format!("{}: expected error containing {:?}, got:\n{}", p.display(), want, stderr));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
