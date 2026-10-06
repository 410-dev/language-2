//! Native build driver: LLVM IR → object files (`opt`/`llc`) → executables (system linker),
//! for every target listed in `@compiler(Target=[...])` (spec 2.5, 15.1).

use crate::hir::{Program, Target};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Toolchain {
    pub llc: PathBuf,
    pub opt: Option<PathBuf>,
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{}.exe", name)
    } else {
        name.to_string()
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(exe(name));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn rustc_info() -> Option<(PathBuf, String)> {
    let sysroot = Command::new("rustc").args(["--print", "sysroot"]).output().ok()?;
    let vv = Command::new("rustc").arg("-vV").output().ok()?;
    let sysroot = PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim());
    let host = String::from_utf8_lossy(&vv.stdout).lines().find_map(|l| l.strip_prefix("host: ").map(|s| s.trim().to_string()))?;
    Some((sysroot, host))
}

/// Locates an LLVM tool: `$L2_LLVM_DIR`, `PATH`, then rustup's `llvm-tools` component.
pub fn find_llvm_tool(name: &str) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("L2_LLVM_DIR") {
        let p = PathBuf::from(dir).join(exe(name));
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(p) = which(name) {
        return Some(p);
    }
    let (sysroot, host) = rustc_info()?;
    let p = sysroot.join("lib").join("rustlib").join(host).join("bin").join(exe(name));
    p.is_file().then_some(p)
}

pub fn find_toolchain() -> Result<Toolchain, String> {
    let llc = find_llvm_tool("llc").ok_or("LLVM 'llc' was not found. Install it (e.g. `rustup component add llvm-tools`) or set L2_LLVM_DIR")?;
    Ok(Toolchain { llc, opt: find_llvm_tool("opt") })
}

pub fn host_target() -> Target {
    match std::env::consts::ARCH {
        "x86" => Target::I386,
        "aarch64" => Target::Arm64,
        _ => Target::Amd64,
    }
}

fn runtime_lib_name() -> &'static str {
    if cfg!(windows) {
        "l2_native_rt.lib"
    } else {
        "libl2_native_rt.a"
    }
}

/// Finds the runtime static library for a target.
pub fn find_runtime(target: Target) -> Option<PathBuf> {
    let triple = crate::llvm::triple_for(target);
    let name = runtime_lib_name();
    let mut cands = Vec::new();
    if let Some(dir) = std::env::var_os("L2_RUNTIME_DIR") {
        let d = PathBuf::from(dir);
        cands.push(d.join(&triple).join(name));
        if target == host_target() {
            cands.push(d.join(name));
        }
    }
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let tdir = dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| dir.to_path_buf());
            if target == host_target() {
                cands.push(tdir.join("release").join(name));
                cands.push(dir.join(name));
                cands.push(tdir.join("debug").join(name));
            }
            cands.push(tdir.join(&triple).join("release").join(name));
            cands.push(tdir.join(&triple).join("debug").join(name));
        }
    }
    cands.into_iter().find(|p| p.is_file())
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("failed to run {}: {}", what, e))?;
    if !out.status.success() {
        return Err(format!("{} failed:\n{}{}", what, String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

/// Compiles LLVM IR text to an object file.
pub fn compile_ir(tc: &Toolchain, ir: &str, target: Target, out_obj: &Path, opt_level: u8) -> Result<(), String> {
    let ll = out_obj.with_extension("ll");
    std::fs::write(&ll, ir).map_err(|e| format!("cannot write {}: {}", ll.display(), e))?;
    let triple = crate::llvm::triple_for(target);
    let mut input = ll.clone();
    if let (Some(opt), true) = (&tc.opt, opt_level > 0) {
        let bc = out_obj.with_extension("bc");
        run(Command::new(opt).arg(format!("-O{}", opt_level)).arg(&ll).arg("-o").arg(&bc), "opt")?;
        input = bc;
    }
    run(
        Command::new(&tc.llc).arg(format!("-O{}", opt_level)).arg("-filetype=obj").arg(format!("-mtriple={}", triple)).arg(&input).arg("-o").arg(out_obj),
        "llc",
    )
}

/// Links an object file with the runtime into an executable.
pub fn link(obj: &Path, runtime: &Path, target: Target, out: &Path) -> Result<(), String> {
    if cfg!(windows) {
        link_msvc(obj, runtime, target, out)
    } else {
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
        let mut cmd = Command::new(cc);
        cmd.arg(obj).arg(runtime).arg("-o").arg(out);
        if !cfg!(target_os = "macos") {
            cmd.args(["-lpthread", "-ldl", "-lm"]);
        }
        run(&mut cmd, "linker")
    }
}

#[cfg(windows)]
fn link_msvc(obj: &Path, runtime: &Path, target: Target, out: &Path) -> Result<(), String> {
    let triple = crate::llvm::triple_for(target);
    let mut cmd = match cc::windows_registry::find(&triple, "link.exe") {
        Some(c) => c,
        None => {
            // fall back to the LLVM linker shipped with rustup, using the host's library paths
            let lld = find_llvm_tool("rust-lld").ok_or("no MSVC linker (link.exe) or rust-lld found")?;
            let mut c = Command::new(lld);
            c.args(["-flavor", "link"]);
            c
        }
    };
    let machine = match target {
        Target::Amd64 => "X64",
        Target::I386 => "X86",
        Target::Arm64 => "ARM64",
    };
    cmd.arg("/NOLOGO")
        .arg(format!("/MACHINE:{}", machine))
        .arg(format!("/OUT:{}", out.display()))
        .arg(obj)
        .arg(runtime)
        .args(["kernel32.lib", "ntdll.lib", "userenv.lib", "ws2_32.lib", "dbghelp.lib", "advapi32.lib", "bcrypt.lib", "msvcrt.lib", "/SUBSYSTEM:CONSOLE"]);
    run(&mut cmd, "linker")
}

#[cfg(not(windows))]
fn link_msvc(_: &Path, _: &Path, _: Target, _: &Path) -> Result<(), String> {
    unreachable!()
}

pub struct BuildResult {
    pub target: Target,
    pub ir: PathBuf,
    pub object: PathBuf,
    /// `None` when no runtime library / linker is available for this target.
    pub executable: Option<PathBuf>,
    pub note: Option<String>,
}

/// Builds the program for each target. `stem` is the output path without extension.
pub fn build(p: &Program, targets: &[Target], stem: &Path, opt_level: u8, suffix_targets: bool) -> Result<Vec<BuildResult>, String> {
    let tc = find_toolchain()?;
    if let Some(dir) = stem.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
        }
    }
    let mut results = Vec::new();
    for &t in targets {
        let base = if suffix_targets { PathBuf::from(format!("{}-{}", stem.display(), t.name())) } else { stem.to_path_buf() };
        let obj = base.with_extension(if cfg!(windows) { "obj" } else { "o" });
        let ir = crate::llvm::emit_module(p, t);
        compile_ir(&tc, &ir, t, &obj, opt_level)?;
        let exe_path = if cfg!(windows) { base.with_extension("exe") } else { base.clone() };
        let (executable, note) = match find_runtime(t) {
            Some(rt) => match link(&obj, &rt, t, &exe_path) {
                Ok(()) => (Some(exe_path), None),
                Err(e) => (None, Some(format!("linking for {} failed: {}", t.name(), e))),
            },
            None => (
                None,
                Some(format!(
                    "object file only: no runtime library for {} (build it with `cargo build -p l2-native-rt --release --target {}`)",
                    t.name(),
                    crate::llvm::triple_for(t)
                )),
            ),
        };
        results.push(BuildResult { target: t, ir: obj.with_extension("ll"), object: obj, executable, note });
    }
    Ok(results)
}
