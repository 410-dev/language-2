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

/// File name of the shared runtime on Windows (loaded at start-up, see `llvm::windows_preload`).
pub const SHARED_RUNTIME_DLL: &str = "l2_native_rt.dll";

/// The file linked against: the static library, or for the shared runtime the import library
/// (Windows) / the shared object itself.
fn runtime_lib_name(shared: bool) -> &'static str {
    match (shared, cfg!(windows), cfg!(target_os = "macos")) {
        (false, true, _) => "l2_native_rt.lib",
        (false, false, _) => "libl2_native_rt.a",
        (true, true, _) => "l2_native_rt.dll.lib",
        (true, false, true) => "libl2_native_rt.dylib",
        (true, false, false) => "libl2_native_rt.so",
    }
}

/// For a shared runtime found at `link` (the file linked against), the file loaded when the
/// program runs (the DLL on Windows; the same file elsewhere).
pub fn shared_runtime_file(link: &Path) -> Option<PathBuf> {
    if cfg!(windows) {
        let dll = link.with_file_name(SHARED_RUNTIME_DLL);
        dll.is_file().then_some(dll)
    } else {
        link.is_file().then(|| link.to_path_buf())
    }
}

/// Finds the runtime library for a target: static (`IncludeDependencies=true`) or shared.
/// Looks in `$L2_RUNTIME_DIR`, the build directories next to the compiler, then the installed
/// SDK (spec 2.5).
pub fn find_runtime(target: Target, shared: bool) -> Option<PathBuf> {
    let triple = crate::llvm::triple_for(target);
    let name = runtime_lib_name(shared);
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
    cands.push(crate::sdk::lib_dir(crate::sdk::SDK_VERSION, target).join(name));
    cands.into_iter().find(|p| p.is_file() && (!shared || shared_runtime_file(p).is_some()))
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

/// Links an object file with the runtime into an executable. With the shared runtime the
/// program finds it in the installed SDK (or next to itself) when it starts.
pub fn link(obj: &Path, runtime: &Path, target: Target, out: &Path, shared: bool, sdk: u32) -> Result<(), String> {
    if cfg!(windows) {
        link_msvc(obj, runtime, target, out, shared)
    } else {
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
        let mut cmd = Command::new(cc);
        cmd.arg(obj).arg(runtime).arg("-o").arg(out);
        if shared {
            let origin = if cfg!(target_os = "macos") { "@executable_path" } else { "$ORIGIN" };
            cmd.arg(format!("-Wl,-rpath,{}", crate::sdk::lib_dir(sdk, target).display()));
            cmd.arg(format!("-Wl,-rpath,{}", origin));
        }
        if !cfg!(target_os = "macos") {
            cmd.args(["-lpthread", "-ldl", "-lm"]);
        }
        run(&mut cmd, "linker")
    }
}

#[cfg(windows)]
fn link_msvc(obj: &Path, runtime: &Path, target: Target, out: &Path, shared: bool) -> Result<(), String> {
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
        .args(["kernel32.lib", "ntdll.lib", "userenv.lib", "ws2_32.lib", "dbghelp.lib", "advapi32.lib", "bcrypt.lib", "secur32.lib", "crypt32.lib", "ncrypt.lib", "ole32.lib", "msvcrt.lib", "/SUBSYSTEM:CONSOLE"]);
    if shared {
        // loaded on first use, after the start-up code located it (llvm::windows_preload)
        cmd.arg(format!("/DELAYLOAD:{}", SHARED_RUNTIME_DLL)).arg("delayimp.lib");
    }
    run(&mut cmd, "linker")
}

#[cfg(not(windows))]
fn link_msvc(_: &Path, _: &Path, _: Target, _: &Path, _: bool) -> Result<(), String> {
    unreachable!()
}

pub struct BuildResult {
    pub target: Target,
    pub ir: PathBuf,
    pub object: PathBuf,
    /// `None` when no runtime library / linker is available for this target.
    pub executable: Option<PathBuf>,
    pub note: Option<String>,
    /// The shared runtime the executable loads (`IncludeDependencies=false`).
    pub shared_runtime: Option<PathBuf>,
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
        let shared = !p.config.include_dependencies;
        let (executable, note) = match find_runtime(t, shared) {
            Some(rt) => match link(&obj, &rt, t, &exe_path, shared, p.config.sdk) {
                Ok(()) => (Some(exe_path), None),
                Err(e) => (None, Some(format!("linking for {} failed: {}", t.name(), e))),
            },
            None => (
                None,
                Some(format!(
                    "object file only: no {} runtime library for {} (build it with `cargo build -p l2-native-rt --release --target {}`, or install an SDK)",
                    if shared { "shared" } else { "static" },
                    t.name(),
                    crate::llvm::triple_for(t)
                )),
            ),
        };
        let shared_runtime = if shared && executable.is_some() { find_runtime(t, true).and_then(|l| shared_runtime_file(&l)) } else { None };
        results.push(BuildResult { target: t, ir: obj.with_extension("ll"), object: obj, executable, note, shared_runtime });
    }
    Ok(results)
}
