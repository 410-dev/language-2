//! Installed SDK versions (spec 2.3, 2.5).
//!
//! Several SDK versions can be installed side by side under the SDK home (`$L2_HOME`, default
//! `%LOCALAPPDATA%\language-2` on Windows and `~/.language-2` elsewhere):
//!
//! ```text
//! <home>/sdk/<version>/bin/language-2[.exe]        the toolchain of that SDK version
//! <home>/sdk/<version>/lib/<target triple>/        its runtime libraries (static and shared)
//! ```
//!
//! `language-2 run` / `build` / `check` read the program's `@using sdk N`; when N is not the
//! running toolchain's own version they hand the command to the installed toolchain of SDK N.
//! Native programs built with `IncludeDependencies=false` load the shared runtime of their SDK
//! version from `lib/<triple>/`.

use crate::hir::Target;
use std::path::{Path, PathBuf};

/// The SDK version implemented by this toolchain.
pub const SDK_VERSION: u32 = 1;

pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("L2_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(h);
    }
    if cfg!(windows) {
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(l).join(crate::LANG_NAME);
        }
    }
    let user_home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    user_home.join(format!(".{}", crate::LANG_NAME))
}

pub fn sdk_dir(version: u32) -> PathBuf {
    home().join("sdk").join(version.to_string())
}

/// Runtime libraries of SDK `version` for `target`.
pub fn lib_dir(version: u32, target: Target) -> PathBuf {
    sdk_dir(version).join("lib").join(crate::llvm::triple_for(target))
}

fn exe_name() -> String {
    if cfg!(windows) {
        format!("{}.exe", crate::LANG_NAME)
    } else {
        crate::LANG_NAME.to_string()
    }
}

/// The toolchain executable of an installed SDK version.
pub fn toolchain(version: u32) -> Option<PathBuf> {
    let p = sdk_dir(version).join("bin").join(exe_name());
    p.is_file().then_some(p)
}

/// Installed SDK versions, ascending.
pub fn installed() -> Vec<u32> {
    let mut v: Vec<u32> = std::fs::read_dir(home().join("sdk"))
        .map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| e.file_name().to_str().and_then(|n| n.parse().ok())).filter(|v| toolchain(*v).is_some()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// The version in a source file's `@using sdk N` directive.
pub fn requested_version(src: &str) -> Option<u32> {
    for line in src.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("@using") {
            let mut words = rest.split_whitespace();
            if words.next() == Some("sdk") {
                return words.next().and_then(|w| w.parse().ok());
            }
        }
    }
    None
}

fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {}", d.display(), e))?;
    }
    std::fs::copy(from, to).map_err(|e| format!("cannot copy {} to {}: {}", from.display(), to.display(), e))?;
    Ok(())
}

/// Installs this toolchain and the runtime libraries it can find as SDK `SDK_VERSION`.
/// Returns a description of what was installed.
pub fn install() -> Result<Vec<String>, String> {
    let me = std::env::current_exe().map_err(|e| format!("cannot locate the running toolchain: {}", e))?;
    let dir = sdk_dir(SDK_VERSION);
    let mut done = Vec::new();
    let bin = dir.join("bin").join(exe_name());
    if me != bin {
        copy_file(&me, &bin)?;
    }
    done.push(format!("toolchain: {}", bin.display()));
    for t in [Target::Amd64, Target::I386, Target::Arm64] {
        let out = lib_dir(SDK_VERSION, t);
        let mut files = Vec::new();
        for shared in [false, true] {
            if let Some(found) = crate::native::find_runtime(t, shared) {
                if found.starts_with(&out) {
                    continue;
                }
                let mut parts = vec![found.clone()];
                if shared {
                    // the import library sits next to the DLL / shared object
                    if let Some(lib) = crate::native::shared_runtime_file(&found) {
                        if lib != found {
                            parts.push(lib);
                        }
                    }
                }
                for f in parts {
                    let name = f.file_name().unwrap();
                    copy_file(&f, &out.join(name))?;
                    files.push(name.to_string_lossy().to_string());
                }
            }
        }
        if !files.is_empty() {
            done.push(format!("runtime [{}]: {} ({})", t.name(), out.display(), files.join(", ")));
        }
    }
    Ok(done)
}
