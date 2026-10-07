//! TCP / UDP sockets and an HTTP(S) client.

use super::{closed, handle, io_err, opt_str, str_dict, tuple, Args, Resource, SysOp};
use crate::value::*;
use std::io::{BufRead, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

fn timeout(ms: i64) -> Option<Duration> {
    (ms > 0).then(|| Duration::from_millis(ms as u64))
}

fn addr(host: &str, port: i64) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{}]:{}", host, port)
    } else {
        format!("{}:{}", host, port)
    }
}

fn check_port<H: Host>(port: i64, h: &mut H) -> Result<(), H::Err> {
    if !(0..=65535).contains(&port) {
        return Err(h.throw(ExcKind::IllegalArgument, format!("port {} out of range 0..65535", port)));
    }
    Ok(())
}

fn with_tcp<H: Host, R>(a: &Args, h: &mut H, f: impl FnOnce(&mut std::io::BufReader<TcpStream>) -> std::io::Result<R>) -> Result<R, H::Err> {
    let x = a.handle(0, h)?;
    let mut r = x.res.borrow_mut();
    match &mut *r {
        Resource::Tcp(s) => f(s).map_err(|e| io_err(e, "socket", h)),
        _ => Err(closed("socket", h)),
    }
}

fn with_udp<H: Host, R>(a: &Args, h: &mut H, f: impl FnOnce(&UdpSocket) -> std::io::Result<R>) -> Result<R, H::Err> {
    let x = a.handle(0, h)?;
    let r = x.res.borrow();
    match &*r {
        Resource::Udp(s) => f(s).map_err(|e| io_err(e, "socket", h)),
        _ => Err(closed("socket", h)),
    }
}

pub(super) fn call<H: Host>(op: SysOp, a: &Args, h: &mut H) -> Result<Value, H::Err> {
    use SysOp::*;
    Ok(match op {
        tcpConnect => {
            let (host, port) = (a.str(0), a.int(1));
            check_port(port, h)?;
            let target = addr(&host, port);
            let addrs: Vec<_> = target.to_socket_addrs().map_err(|e| io_err(e, &target, h))?.collect();
            let mut last = None;
            let mut stream = None;
            for sa in addrs {
                let r = match timeout(a.int(2)) {
                    Some(t) => TcpStream::connect_timeout(&sa, t),
                    None => TcpStream::connect(sa),
                };
                match r {
                    Ok(s) => {
                        stream = Some(s);
                        break;
                    }
                    Err(e) => last = Some(e),
                }
            }
            match stream {
                Some(s) => handle("TcpSocket", Resource::Tcp(std::io::BufReader::new(s))),
                None => return Err(io_err(last.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no address")), &format!("connect {}", target), h)),
            }
        }
        tcpListen => {
            let (host, port) = (a.str(0), a.int(1));
            check_port(port, h)?;
            let target = addr(&host, port);
            let l = TcpListener::bind(&target).map_err(|e| io_err(e, &format!("listen {}", target), h))?;
            handle("TcpServer", Resource::Listener(l))
        }
        tcpAccept => {
            let x = a.handle(0, h)?;
            let r = x.res.borrow();
            let Resource::Listener(l) = &*r else { return Err(closed("server", h)) };
            let (s, _) = l.accept().map_err(|e| io_err(e, "accept", h))?;
            handle("TcpSocket", Resource::Tcp(std::io::BufReader::new(s)))
        }
        tcpRead => {
            let n = a.int(1).max(0) as usize;
            Value::bytes(with_tcp(a, h, |s| {
                let mut buf = vec![0u8; n.max(1)];
                let k = s.read(&mut buf)?;
                buf.truncate(k);
                Ok(buf)
            })?)
        }
        tcpReadLine => opt_str(with_tcp(a, h, |s| {
            let mut buf = Vec::new();
            let n = s.read_until(b'\n', &mut buf)?;
            Ok((n > 0).then(|| {
                while buf.last() == Some(&b'\n') || buf.last() == Some(&b'\r') {
                    buf.pop();
                }
                String::from_utf8_lossy(&buf).into_owned()
            }))
        })?),
        tcpWrite => {
            let data = a.bytes(1, h)?;
            with_tcp(a, h, |s| {
                s.get_mut().write_all(&data)?;
                s.get_mut().flush()
            })?;
            Value::Void
        }
        tcpSetTimeout => {
            let t = timeout(a.int(1));
            let x = a.handle(0, h)?;
            let r = x.res.borrow();
            let res = match &*r {
                Resource::Tcp(s) => s.get_ref().set_read_timeout(t).and_then(|_| s.get_ref().set_write_timeout(t)),
                Resource::Udp(s) => s.set_read_timeout(t).and_then(|_| s.set_write_timeout(t)),
                Resource::Listener(_) => Ok(()),
                _ => return Err(closed("socket", h)),
            };
            res.map_err(|e| io_err(e, "socket", h))?;
            Value::Void
        }
        tcpLocalAddress | tcpPeerAddress => {
            let x = a.handle(0, h)?;
            let r = x.res.borrow();
            let res = match (&*r, op) {
                (Resource::Tcp(s), tcpLocalAddress) => s.get_ref().local_addr(),
                (Resource::Tcp(s), _) => s.get_ref().peer_addr(),
                (Resource::Listener(l), _) => l.local_addr(),
                (Resource::Udp(s), _) => s.local_addr(),
                _ => return Err(closed("socket", h)),
            };
            Value::str(res.map_err(|e| io_err(e, "socket", h))?.to_string())
        }
        tcpShutdown => {
            with_tcp(a, h, |s| s.get_ref().shutdown(Shutdown::Write))?;
            Value::Void
        }
        udpBind => {
            let (host, port) = (a.str(0), a.int(1));
            check_port(port, h)?;
            let target = addr(&host, port);
            let s = UdpSocket::bind(&target).map_err(|e| io_err(e, &format!("bind {}", target), h))?;
            handle("UdpSocket", Resource::Udp(s))
        }
        udpSend => {
            let data = a.bytes(1, h)?;
            let (host, port) = (a.str(2), a.int(3));
            check_port(port, h)?;
            let target = addr(&host, port);
            Value::i64(with_udp(a, h, |s| s.send_to(&data, &target))? as i64)
        }
        udpReceive => {
            let n = a.int(1).max(1) as usize;
            let (buf, from) = with_udp(a, h, |s| {
                let mut buf = vec![0u8; n];
                let (k, from) = s.recv_from(&mut buf)?;
                buf.truncate(k);
                Ok((buf, from))
            })?;
            tuple(vec![Value::bytes(buf), Value::str(from.ip().to_string()), Value::i64(from.port() as i64)])
        }
        udpSetTimeout => {
            let t = timeout(a.int(1));
            with_udp(a, h, |s| s.set_read_timeout(t).and_then(|_| s.set_write_timeout(t)))?;
            Value::Void
        }
        httpRequest => http(a, h)?,
        _ => unreachable!(),
    })
}

fn http<H: Host>(a: &Args, h: &mut H) -> Result<Value, H::Err> {
    let (method, url) = (a.str(0).to_ascii_uppercase(), a.str(1));
    let headers = a.dict(2);
    let body = a.bytes(3, h)?;
    let mut b = ureq::AgentBuilder::new().redirects(5);
    if let Some(t) = timeout(a.int(4)) {
        b = b.timeout(t);
    }
    let agent = b.build();
    let mut req = agent.request(&method, &url);
    for (k, v) in &headers {
        req = req.set(k, v);
    }
    let r = if body.is_empty() && matches!(method.as_str(), "GET" | "HEAD" | "DELETE" | "OPTIONS") { req.call() } else { req.send_bytes(&body) };
    let resp = match r {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(ureq::Error::Transport(t)) => {
            let kind = match t.kind() {
                ureq::ErrorKind::InvalidUrl | ureq::ErrorKind::UnknownScheme => ExcKind::IllegalArgument,
                ureq::ErrorKind::Io if t.to_string().contains("timed out") => ExcKind::Timeout,
                _ => ExcKind::IO,
            };
            return Err(h.throw(kind, format!("{} {}: {}", method, url, t)));
        }
    };
    let status = resp.status() as i64;
    let mut hs = Vec::new();
    for name in resp.headers_names() {
        if let Some(v) = resp.header(&name) {
            hs.push((name.to_ascii_lowercase(), v.to_string()));
        }
    }
    let mut data = Vec::new();
    resp.into_reader().take(1 << 30).read_to_end(&mut data).map_err(|e| io_err(e, &url, h))?;
    Ok(tuple(vec![Value::i64(status), str_dict(hs), Value::bytes(data)]))
}
