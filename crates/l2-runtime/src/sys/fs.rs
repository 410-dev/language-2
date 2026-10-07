//! Files, directories, paths and file streams.

use super::encoding::{decode, encode};
use super::{closed, handle, io_err, opt_str, str_array, Args, Resource, SysOp};
use crate::value::*;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

fn path_str(p: &Path) -> String {
    p.display().to_string()
}

fn opt_path(p: Option<&std::ffi::OsStr>) -> Value {
    opt_str(p.map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty()))
}

/// Removes `.` and resolves `..` lexically.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() || out.as_os_str().is_empty() {
                    out.push("..");
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn millis(t: std::time::SystemTime) -> i64 {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

fn with_file<H: Host, R>(a: &Args, h: &mut H, f: impl FnOnce(&mut std::io::BufReader<std::fs::File>) -> std::io::Result<R>) -> Result<R, H::Err> {
    let x = a.handle(0, h)?;
    let mut r = x.res.borrow_mut();
    match &mut *r {
        Resource::File(file) => f(file).map_err(|e| io_err(e, "file", h)),
        _ => Err(closed("file", h)),
    }
}

pub(super) fn call<H: Host>(op: SysOp, a: &Args, h: &mut H) -> Result<Value, H::Err> {
    use SysOp::*;
    let p = a.str(0);
    Ok(match op {
        fsReadBytes => Value::bytes(std::fs::read(&p).map_err(|e| io_err(e, &p, h))?),
        fsReadText => {
            let b = std::fs::read(&p).map_err(|e| io_err(e, &p, h))?;
            let b = if b.starts_with(&[0xef, 0xbb, 0xbf]) { &b[3..] } else { &b[..] };
            Value::str(decode(b, &a.str(1), h)?)
        }
        fsWriteBytes | fsWriteText => {
            let (data, append) = if op == fsWriteBytes { (a.bytes(1, h)?, a.bool(2)) } else { (encode(&a.str(1), &a.str(2), h)?, a.bool(3)) };
            let r = if append {
                std::fs::OpenOptions::new().create(true).append(true).open(&p).and_then(|mut f| f.write_all(&data))
            } else {
                std::fs::write(&p, &data)
            };
            r.map_err(|e| io_err(e, &p, h))?;
            Value::Void
        }
        fsExists => Value::Bool(Path::new(&p).exists()),
        fsIsFile => Value::Bool(Path::new(&p).is_file()),
        fsIsDir => Value::Bool(Path::new(&p).is_dir()),
        fsSize => Value::i64(std::fs::metadata(&p).map_err(|e| io_err(e, &p, h))?.len() as i64),
        fsModified => Value::i64(millis(std::fs::metadata(&p).and_then(|m| m.modified()).map_err(|e| io_err(e, &p, h))?)),
        fsDelete => {
            std::fs::remove_file(&p).map_err(|e| io_err(e, &p, h))?;
            Value::Void
        }
        fsDeleteDir => {
            let r = if a.bool(1) { std::fs::remove_dir_all(&p) } else { std::fs::remove_dir(&p) };
            r.map_err(|e| io_err(e, &p, h))?;
            Value::Void
        }
        fsMakeDir => {
            let r = if a.bool(1) { std::fs::create_dir_all(&p) } else { std::fs::create_dir(&p) };
            r.map_err(|e| io_err(e, &p, h))?;
            Value::Void
        }
        fsList => {
            let mut names = Vec::new();
            for e in std::fs::read_dir(&p).map_err(|e| io_err(e, &p, h))? {
                let e = e.map_err(|e| io_err(e, &p, h))?;
                names.push(e.file_name().to_string_lossy().into_owned());
            }
            names.sort();
            str_array(names)
        }
        fsCopy => {
            let to = a.str(1);
            let r = if Path::new(&p).is_dir() { copy_dir(Path::new(&p), Path::new(&to)) } else { std::fs::copy(&p, &to).map(|_| ()) };
            r.map_err(|e| io_err(e, &format!("copy {} -> {}", p, to), h))?;
            Value::Void
        }
        fsMove => {
            let to = a.str(1);
            std::fs::rename(&p, &to).map_err(|e| io_err(e, &format!("move {} -> {}", p, to), h))?;
            Value::Void
        }
        fsTempDir => Value::str(path_str(&std::env::temp_dir())),
        fsHomeDir => opt_str(std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok()),
        pathJoin => {
            let mut out = PathBuf::new();
            for part in a.strs(0) {
                out.push(part);
            }
            Value::str(path_str(&out))
        }
        pathParent => opt_path(Path::new(&p).parent().map(|x| x.as_os_str())),
        pathFileName => opt_path(Path::new(&p).file_name()),
        pathStem => opt_path(Path::new(&p).file_stem()),
        pathExtension => opt_path(Path::new(&p).extension()),
        pathAbsolute => {
            let abs = std::path::absolute(&p).map_err(|e| io_err(e, &p, h))?;
            Value::str(path_str(&normalize(&abs)))
        }
        pathNormalize => Value::str(path_str(&normalize(Path::new(&p)))),
        pathSeparator => Value::str(std::path::MAIN_SEPARATOR.to_string()),
        fileOpen => {
            let mode = a.str(1);
            let mut o = std::fs::OpenOptions::new();
            match mode.as_str() {
                "r" => o.read(true),
                "w" => o.write(true).create(true).truncate(true),
                "a" => o.append(true).create(true),
                "rw" | "r+" => o.read(true).write(true).create(true),
                _ => return Err(h.throw(ExcKind::IllegalArgument, format!("unknown file mode \"{}\" (expected \"r\", \"w\", \"a\" or \"rw\")", mode))),
            };
            let f = o.open(&p).map_err(|e| io_err(e, &p, h))?;
            handle("File", Resource::File(std::io::BufReader::new(f)))
        }
        fileRead => {
            let n = a.int(1);
            Value::bytes(with_file(a, h, |f| {
                let mut out = Vec::new();
                if n < 0 {
                    f.read_to_end(&mut out)?;
                } else {
                    f.by_ref().take(n as u64).read_to_end(&mut out)?;
                }
                Ok(out)
            })?)
        }
        fileReadLine => {
            let line = with_file(a, h, |f| {
                let mut buf = Vec::new();
                let n = f.read_until(b'\n', &mut buf)?;
                Ok((n > 0).then(|| {
                    while buf.last() == Some(&b'\n') || buf.last() == Some(&b'\r') {
                        buf.pop();
                    }
                    String::from_utf8_lossy(&buf).into_owned()
                }))
            })?;
            opt_str(line)
        }
        fileWrite => {
            let data = a.bytes(1, h)?;
            with_file(a, h, |f| {
                // drop read-ahead so the write lands at the logical position
                let pos = f.stream_position()?;
                f.seek(SeekFrom::Start(pos))?;
                f.get_mut().write_all(&data)
            })?;
            Value::Void
        }
        fileSeek => {
            let pos = a.int(1);
            with_file(a, h, |f| if pos < 0 { f.seek(SeekFrom::End(pos + 1)) } else { f.seek(SeekFrom::Start(pos as u64)) })?;
            Value::Void
        }
        filePosition => Value::i64(with_file(a, h, |f| f.stream_position())? as i64),
        fileLength => Value::i64(with_file(a, h, |f| f.get_ref().metadata().map(|m| m.len()))? as i64),
        fileFlush => {
            with_file(a, h, |f| f.get_mut().flush())?;
            Value::Void
        }
        _ => unreachable!(),
    })
}
