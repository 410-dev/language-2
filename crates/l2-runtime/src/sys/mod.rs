//! System services of the standard library (spec 14.5–14.11): encodings, files, processes,
//! networking, time, randomness and cryptography.
//!
//! Every operation is one [`SysOp`] reached through the `Sys` builtin, so all three backends
//! share this implementation. Standard library modules call them as intrinsics
//! (`intr.fsReadBytes(path)`); the compiler type-checks the calls against [`SysOp::sig`].
//!
//! Signature letters: `S` String, `s` String?, `I` Int64, `F` Float64, `B` Boolean,
//! `Y` UInt8[] (bytes), `A` String[], `L` Int64[], `M` Dictionary[String, String],
//! `H` resource handle, `D` DTVariable, `V` void; `(..)` is a multiple return.

mod crypto;
mod encoding;
mod fs;
mod net;
mod proc;
mod time;

use crate::value::*;
use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

pub use encoding::{json_parse, json_stringify};

macro_rules! sys_ops {
    ($($name:ident = $sig:literal),* $(,)?) => {
        #[allow(non_camel_case_types)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum SysOp { $($name),* }
        impl SysOp {
            pub const ALL: &'static [SysOp] = &[$(SysOp::$name),*];
            pub fn name(self) -> &'static str {
                match self { $(SysOp::$name => stringify!($name)),* }
            }
            /// Parameter letters, `>`, result letters (see the module documentation).
            pub fn sig(self) -> &'static str {
                match self { $(SysOp::$name => $sig),* }
            }
            pub fn by_name(n: &str) -> Option<SysOp> {
                Self::ALL.iter().copied().find(|o| o.name() == n)
            }
            pub fn code(self) -> u16 {
                self as u16
            }
            pub fn from_code(c: u16) -> SysOp {
                Self::ALL[c as usize]
            }
        }
    };
}

sys_ops! {
    // ---------------- text encodings, bytes, JSON
    strEncode = "SS>Y",
    bytesDecode = "YS>S",
    bytesToHex = "YB>S",
    bytesFromHex = "S>Y",
    bytesToBase64 = "YB>S",
    bytesFromBase64 = "S>Y",
    numToBytes = "DS>Y",
    numFromBytes = "YSS>D",
    jsonStringify = "DI>S",
    jsonParse = "S>D",
    compare = "DD>I",
    // a shared mutable slot (state changed through read-only references)
    cellNew = "D>H",
    cellGet = "H>D",
    cellSet = "HD>V",

    // ---------------- system
    sysSleep = "I>V",
    sysExit = "IB>V",
    sysEnv = "S>s",
    sysSetEnv = "SS>V",
    sysRemoveEnv = "S>V",
    sysEnvAll = ">M",
    sysInfo = "S>s",
    sysCpuCount = ">I",
    sysPid = ">I",
    sysCwd = ">S",
    sysSetCwd = "S>V",

    // ---------------- processes
    procSpawn = "AsBs>H",
    procWait = "H>(ISS)",
    procIsDone = "H>B",
    procKill = "H>V",

    // ---------------- files and paths
    fsReadBytes = "S>Y",
    fsReadText = "SS>S",
    fsWriteBytes = "SYB>V",
    fsWriteText = "SSSB>V",
    fsExists = "S>B",
    fsIsFile = "S>B",
    fsIsDir = "S>B",
    fsSize = "S>I",
    fsModified = "S>I",
    fsDelete = "S>V",
    fsDeleteDir = "SB>V",
    fsMakeDir = "SB>V",
    fsList = "S>A",
    fsCopy = "SS>V",
    fsMove = "SS>V",
    fsTempDir = ">S",
    fsHomeDir = ">s",
    pathJoin = "A>S",
    pathParent = "S>s",
    pathFileName = "S>s",
    pathStem = "S>s",
    pathExtension = "S>s",
    pathAbsolute = "S>S",
    pathNormalize = "S>S",
    pathSeparator = ">S",
    fileOpen = "SS>H",
    fileRead = "HI>Y",
    fileReadLine = "H>s",
    fileWrite = "HY>V",
    fileSeek = "HI>V",
    filePosition = "H>I",
    fileLength = "H>I",
    fileFlush = "H>V",
    close = "H>V",
    isOpen = "H>B",

    // ---------------- networking
    tcpConnect = "SII>H",
    tcpListen = "SI>H",
    tcpAccept = "H>H",
    tcpRead = "HI>Y",
    tcpReadLine = "H>s",
    tcpWrite = "HY>V",
    tcpSetTimeout = "HI>V",
    tcpLocalAddress = "H>S",
    tcpPeerAddress = "H>S",
    tcpShutdown = "H>V",
    udpBind = "SI>H",
    udpSend = "HYSI>I",
    udpReceive = "HI>(YSI)",
    udpSetTimeout = "HI>V",
    httpRequest = "SSMYI>(IMY)",

    // ---------------- time
    timeNowMillis = ">I",
    timeMonotonicNanos = ">I",
    timeLocalOffset = "I>I",
    timeLocalOffsetOf = "L>I",
    timeFields = "II>L",
    timeFromFields = "LI>I",
    timeAddMonths = "III>I",
    timeFormat = "IIS>S",
    timeParse = "SS>L",
    durationString = "I>S",

    // ---------------- randomness
    rngNew = "IB>H",
    rngInt = "HII>I",
    rngLong = "H>I",
    rngFloat = "H>F",
    rngBytes = "HI>Y",
    rngGaussian = "H>F",
    rngIsSeeded = "H>B",
    secureBytes = "I>Y",

    // ---------------- cryptography
    hashAlgorithms = ">A",
    hashDigest = "SY>Y",
    hashNew = "S>H",
    hashUpdate = "HY>V",
    hashFinish = "H>Y",
    hmac = "SYY>Y",
    pbkdf2 = "SYYII>Y",
    hkdf = "SYYYI>Y",
    constantTimeEquals = "YY>B",
    symAlgorithms = ">A",
    symKeySize = "S>I",
    symEncrypt = "SYYBSIY>Y",
    symDecrypt = "SYYBY>Y",
    rsaGenerate = "IH>(HH)",
    rsaGenerateOs = "I>(HH)",
    rsaPublicOf = "H>H",
    rsaToPem = "H>S",
    rsaFromPem = "S>H",
    rsaIsPrivate = "H>B",
    rsaBits = "H>I",
    rsaEncrypt = "HYH>Y",
    rsaEncryptOs = "HY>Y",
    rsaDecrypt = "HY>Y",
    rsaSign = "HYH>Y",
    rsaSignOs = "HY>Y",
    rsaVerify = "HYY>B",
}

// ---------------------------------------------------------------------------------------------
// resource handles

/// An operating-system resource shared by the standard library objects that refer to it.
pub struct Handle {
    kind: &'static str,
    pub res: RefCell<Resource>,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<{}>", self.kind)
    }
}

impl Handle {
    pub fn kind(&self) -> &'static str {
        self.kind
    }
}

pub enum Resource {
    Closed,
    File(std::io::BufReader<std::fs::File>),
    Tcp(std::io::BufReader<std::net::TcpStream>),
    Listener(std::net::TcpListener),
    Udp(std::net::UdpSocket),
    Process(Box<proc::ProcState>),
    Rng(Box<crypto::Rng>),
    Hasher(Box<dyn digest::DynDigest>),
    RsaPublic(Box<rsa::RsaPublicKey>),
    RsaPrivate(Box<rsa::RsaPrivateKey>),
    Cell(Value),
}

fn handle(kind: &'static str, r: Resource) -> Value {
    Value::Handle(Rc::new(Handle { kind, res: RefCell::new(r) }))
}

// ---------------------------------------------------------------------------------------------
// argument helpers

pub(crate) struct Args<'a>(&'a [Value]);

impl<'a> Args<'a> {
    fn v(&self, i: usize) -> Value {
        self.0.get(i).map(|v| v.deref()).unwrap_or(Value::Null)
    }
    fn str(&self, i: usize) -> String {
        match self.v(i) {
            Value::Str(s) => s.to_string(),
            _ => String::new(),
        }
    }
    fn opt_str(&self, i: usize) -> Option<String> {
        match self.v(i) {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        }
    }
    fn int(&self, i: usize) -> i64 {
        match self.v(i) {
            Value::Int(_, x) => x as i64,
            Value::Float(_, f) => f as i64,
            _ => 0,
        }
    }
    fn bool(&self, i: usize) -> bool {
        matches!(self.v(i), Value::Bool(true))
    }
    fn bytes<H: Host>(&self, i: usize, h: &mut H) -> Result<Vec<u8>, H::Err> {
        match self.0.get(i).map(|v| v.deref()) {
            Some(Value::Array(a)) => match a.items.bytes() {
                Some(b) => Ok(b.into_owned()),
                None => Err(h.throw(ExcKind::IllegalArgument, "expected an array of UInt8".into())),
            },
            Some(Value::Null) | None => Err(h.throw(ExcKind::NullPointer, "bytes are Null".into())),
            Some(other) => Err(h.throw(ExcKind::ClassCast, format!("expected UInt8[], got {}", other.type_name()))),
        }
    }
    /// Borrows the bytes without copying when possible (calls `f`).
    fn with_bytes<H: Host, R>(&self, i: usize, h: &mut H, f: impl FnOnce(&[u8], &mut H) -> Result<R, H::Err>) -> Result<R, H::Err> {
        let v = self.v(i);
        if let Value::Array(a) = &v {
            if let Some(b) = a.items.bytes() {
                let b: Cow<'_, [u8]> = b;
                return f(&b, h);
            }
        }
        let b = self.bytes(i, h)?;
        f(&b, h)
    }
    fn strs(&self, i: usize) -> Vec<String> {
        match self.v(i) {
            Value::Array(a) => a.items.iter().map(|v| if let Value::Str(s) = v.deref() { s.to_string() } else { String::new() }).collect(),
            _ => Vec::new(),
        }
    }
    fn ints(&self, i: usize) -> Vec<i64> {
        match self.v(i) {
            Value::Array(a) => a.items.iter().map(|v| v.deref().as_int() as i64).collect(),
            _ => Vec::new(),
        }
    }
    fn dict(&self, i: usize) -> Vec<(String, String)> {
        match self.v(i) {
            Value::Dict(d) => d
                .entries
                .iter()
                .map(|(k, v)| {
                    let s = |v: &Value| match v.deref() {
                        Value::Str(s) => s.to_string(),
                        other => format!("{:?}", other),
                    };
                    (s(k), s(v))
                })
                .collect(),
            _ => Vec::new(),
        }
    }
    fn handle<H: Host>(&self, i: usize, h: &mut H) -> Result<Rc<Handle>, H::Err> {
        match self.v(i) {
            Value::Handle(x) => Ok(x),
            Value::Null => Err(h.throw(ExcKind::NullPointer, "resource handle is Null".into())),
            other => Err(h.throw(ExcKind::ClassCast, format!("expected a resource handle, got {}", other.type_name()))),
        }
    }
}

pub(crate) fn tuple(v: Vec<Value>) -> Value {
    Value::Tuple(Rc::new(v))
}

pub(crate) fn opt_str(s: Option<String>) -> Value {
    s.map(Value::str).unwrap_or(Value::Null)
}

pub(crate) fn str_array(v: Vec<String>) -> Value {
    Value::array(v.into_iter().map(Value::str).collect(), false)
}

pub(crate) fn int_array(v: Vec<i64>) -> Value {
    Value::packed(Items::Int(IntTy::I64, v), false)
}

pub(crate) fn str_dict(v: Vec<(String, String)>) -> Value {
    let mut d = DictVal::default();
    for (k, x) in v {
        d.insert(Value::str(k), Value::str(x));
    }
    Value::Dict(Rc::new(d))
}

/// Maps an I/O error to the language's exception classes.
pub(crate) fn io_err<H: Host>(e: std::io::Error, what: &str, h: &mut H) -> H::Err {
    use std::io::ErrorKind as K;
    let kind = match e.kind() {
        K::NotFound => ExcKind::FileNotFound,
        K::TimedOut | K::WouldBlock => ExcKind::Timeout,
        _ => ExcKind::IO,
    };
    h.throw(kind, format!("{}: {}", what, e))
}

pub(crate) fn closed<H: Host>(what: &str, h: &mut H) -> H::Err {
    h.throw(ExcKind::IllegalState, format!("{} is closed", what))
}

// ---------------------------------------------------------------------------------------------
// dispatch

/// Runs system operation `code` (the first argument of the `Sys` builtin).
pub fn call<H: Host>(code: u16, args: &[Value], h: &mut H) -> Result<Value, H::Err> {
    let op = SysOp::from_code(code);
    let a = Args(args);
    use SysOp::*;
    match op {
        strEncode | bytesDecode | bytesToHex | bytesFromHex | bytesToBase64 | bytesFromBase64 | numToBytes | numFromBytes | jsonStringify | jsonParse | compare => encoding::call(op, &a, h),
        sysSleep | sysExit | sysEnv | sysSetEnv | sysRemoveEnv | sysEnvAll | sysInfo | sysCpuCount | sysPid | sysCwd | sysSetCwd | procSpawn | procWait | procIsDone | procKill => proc::call(op, &a, h),
        fsReadBytes | fsReadText | fsWriteBytes | fsWriteText | fsExists | fsIsFile | fsIsDir | fsSize | fsModified | fsDelete | fsDeleteDir | fsMakeDir | fsList | fsCopy | fsMove | fsTempDir | fsHomeDir | pathJoin | pathParent
        | pathFileName | pathStem | pathExtension | pathAbsolute | pathNormalize | pathSeparator | fileOpen | fileRead | fileReadLine | fileWrite | fileSeek | filePosition | fileLength | fileFlush => fs::call(op, &a, h),
        close => {
            let x = a.handle(0, h)?;
            let old = std::mem::replace(&mut *x.res.borrow_mut(), Resource::Closed);
            match old {
                Resource::File(mut f) => {
                    use std::io::Write;
                    f.get_mut().flush().map_err(|e| io_err(e, "close", h))?;
                }
                Resource::Tcp(s) => {
                    let _ = s.get_ref().shutdown(std::net::Shutdown::Both);
                }
                Resource::Process(mut p) => p.kill(),
                _ => {}
            }
            Ok(Value::Void)
        }
        isOpen => Ok(Value::Bool(!matches!(*a.handle(0, h)?.res.borrow(), Resource::Closed))),
        cellNew => Ok(handle("Cell", Resource::Cell(a.v(0)))),
        cellGet => match &*a.handle(0, h)?.res.borrow() {
            Resource::Cell(v) => Ok(v.clone()),
            _ => Err(closed("cell", h)),
        },
        cellSet => {
            let x = a.handle(0, h)?;
            *x.res.borrow_mut() = Resource::Cell(a.v(1));
            Ok(Value::Void)
        }
        tcpConnect | tcpListen | tcpAccept | tcpRead | tcpReadLine | tcpWrite | tcpSetTimeout | tcpLocalAddress | tcpPeerAddress | tcpShutdown | udpBind | udpSend | udpReceive | udpSetTimeout | httpRequest => net::call(op, &a, h),
        timeNowMillis | timeMonotonicNanos | timeLocalOffset | timeLocalOffsetOf | timeFields | timeFromFields | timeAddMonths | timeFormat | timeParse | durationString => time::call(op, &a, h),
        _ => crypto::call(op, &a, h),
    }
}
