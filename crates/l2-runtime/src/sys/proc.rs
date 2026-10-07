//! Processes, environment and system information.

use super::{handle, io_err, opt_str, str_dict, tuple, Args, Resource, SysOp};
use crate::value::*;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;

pub struct ProcState {
    child: Child,
    out: Option<JoinHandle<Vec<u8>>>,
    err: Option<JoinHandle<Vec<u8>>>,
    result: Option<(i64, String, String)>,
}

impl ProcState {
    pub fn kill(&mut self) {
        if self.result.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn wait(&mut self) -> std::io::Result<(i64, String, String)> {
        if let Some(r) = &self.result {
            return Ok(r.clone());
        }
        let status = self.child.wait()?;
        let out = self.out.take().and_then(|t| t.join().ok()).unwrap_or_default();
        let err = self.err.take().and_then(|t| t.join().ok()).unwrap_or_default();
        let code = status.code().map(|c| c as i64).unwrap_or(-1);
        let r = (code, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned());
        self.result = Some(r.clone());
        Ok(r)
    }
}

/// Copies a child's output into a buffer, echoing it to our stdout / stderr when `echo`.
fn pump<R: Read + Send + 'static>(mut r: R, echo: bool, to_err: bool) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    all.extend_from_slice(&buf[..n]);
                    if echo {
                        if to_err {
                            let mut e = std::io::stderr().lock();
                            let _ = e.write_all(&buf[..n]);
                            let _ = e.flush();
                        } else {
                            let mut o = std::io::stdout().lock();
                            let _ = o.write_all(&buf[..n]);
                            let _ = o.flush();
                        }
                    }
                }
            }
        }
        all
    })
}

/// `shell("...")`: the platform shell interprets the whole string (pipes, globs, `&&`...).
fn shell_command(cmd: &str) -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/d", "/s", "/c"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.raw_arg(format!("\"{}\"", cmd));
        }
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(cmd);
        c
    }
}

fn spawn<H: Host>(argv: Vec<String>, shell: Option<String>, echo: bool, cwd: Option<String>, h: &mut H) -> Result<Value, H::Err> {
    let mut cmd = match &shell {
        Some(s) => shell_command(s),
        None => {
            let Some(prog) = argv.first() else {
                return Err(h.throw(ExcKind::IllegalArgument, "shell: empty command".into()));
            };
            let mut c = Command::new(prog);
            c.args(&argv[1..]);
            c
        }
    };
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    crate::builtins::flush_out();
    cmd.stdin(if echo { Stdio::inherit() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let what = shell.clone().unwrap_or_else(|| argv.join(" "));
    let mut child = cmd.spawn().map_err(|e| io_err(e, &format!("cannot run '{}'", what), h))?;
    let out = child.stdout.take().map(|o| pump(o, echo, false));
    let err = child.stderr.take().map(|e| pump(e, echo, true));
    Ok(handle("Process", Resource::Process(Box::new(ProcState { child, out, err, result: None }))))
}

pub(super) fn call<H: Host>(op: SysOp, a: &Args, h: &mut H) -> Result<Value, H::Err> {
    use SysOp::*;
    Ok(match op {
        sysSleep => {
            crate::builtins::flush_out();
            let ms = a.int(0);
            if ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(ms as u64));
            }
            Value::Void
        }
        sysExit => {
            if a.bool(1) {
                crate::builtins::flush_out();
                let _ = std::io::stderr().flush();
            }
            std::process::exit(a.int(0) as i32)
        }
        sysEnv => opt_str(std::env::var(a.str(0)).ok()),
        sysSetEnv => {
            let (k, v) = (a.str(0), a.str(1));
            if k.is_empty() || k.contains('=') || k.contains('\0') || v.contains('\0') {
                return Err(h.throw(ExcKind::IllegalArgument, format!("invalid environment variable name \"{}\"", k)));
            }
            // SAFETY-free on the current toolchain edition: single-threaded program state
            std::env::set_var(k, v);
            Value::Void
        }
        sysRemoveEnv => {
            let k = a.str(0);
            if !k.is_empty() && !k.contains('=') && !k.contains('\0') {
                std::env::remove_var(k);
            }
            Value::Void
        }
        sysEnvAll => {
            let mut v: Vec<(String, String)> = std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect();
            v.sort();
            str_dict(v)
        }
        sysInfo => opt_str(match a.str(0).as_str() {
            "os" => Some(match std::env::consts::OS {
                "macos" => "mac".to_string(),
                o => o.to_string(),
            }),
            "family" => Some(std::env::consts::FAMILY.to_string()),
            "arch" => Some(std::env::consts::ARCH.to_string()),
            "hostname" => std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .ok()
                .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty()),
            "user" => std::env::var("USERNAME").or_else(|_| std::env::var("USER")).ok(),
            "exe" => std::env::current_exe().ok().map(|p| p.display().to_string()),
            _ => None,
        }),
        sysCpuCount => Value::i64(std::thread::available_parallelism().map(|n| n.get() as i64).unwrap_or(1)),
        sysPid => Value::i64(std::process::id() as i64),
        sysCwd => Value::str(std::env::current_dir().map(|p| p.display().to_string()).map_err(|e| io_err(e, "current directory", h))?),
        sysSetCwd => {
            let d = a.str(0);
            std::env::set_current_dir(&d).map_err(|e| io_err(e, &d, h))?;
            Value::Void
        }
        procSpawn => spawn(a.strs(0), a.opt_str(1), a.bool(2), a.opt_str(3), h)?,
        procWait => {
            let x = a.handle(0, h)?;
            let r = match &mut *x.res.borrow_mut() {
                Resource::Process(p) => p.wait(),
                _ => return Err(super::closed("process", h)),
            };
            let (code, out, err) = r.map_err(|e| io_err(e, "wait", h))?;
            tuple(vec![Value::i64(code), Value::str(out), Value::str(err)])
        }
        procIsDone => {
            let x = a.handle(0, h)?;
            let mut r = x.res.borrow_mut();
            match &mut *r {
                Resource::Process(p) => Value::Bool(p.result.is_some() || matches!(p.child.try_wait(), Ok(Some(_)))),
                _ => Value::Bool(true),
            }
        }
        procKill => {
            let x = a.handle(0, h)?;
            if let Resource::Process(p) = &mut *x.res.borrow_mut() {
                p.kill();
            }
            Value::Void
        }
        _ => unreachable!(),
    })
}
