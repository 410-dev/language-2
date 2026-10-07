//! Text encodings, hex / base64, numbers as bytes and JSON.

use super::{Args, SysOp};
use crate::bigint::BigInt;
use crate::ops;
use crate::value::*;
use std::rc::Rc;

pub(super) fn call<H: Host>(op: SysOp, a: &Args, h: &mut H) -> Result<Value, H::Err> {
    use SysOp::*;
    Ok(match op {
        strEncode => Value::bytes(encode(&a.str(0), &a.str(1), h)?),
        bytesDecode => {
            let enc = a.str(1);
            a.with_bytes(0, h, |b, h| decode(b, &enc, h).map(Value::str))?
        }
        bytesToHex => {
            let upper = a.bool(1);
            a.with_bytes(0, h, |b, _| Ok(Value::str(to_hex(b, upper))))?
        }
        bytesFromHex => match from_hex(&a.str(0)) {
            Some(b) => Value::bytes(b),
            None => return Err(h.throw(ExcKind::IllegalArgument, "invalid hexadecimal text".into())),
        },
        bytesToBase64 => {
            let url = a.bool(1);
            a.with_bytes(0, h, |b, _| Ok(Value::str(to_base64(b, url))))?
        }
        bytesFromBase64 => match from_base64(&a.str(0)) {
            Some(b) => Value::bytes(b),
            None => return Err(h.throw(ExcKind::IllegalArgument, "invalid base64 text".into())),
        },
        numToBytes => Value::bytes(num_to_bytes(&a.v(0), &a.str(1), h)?),
        numFromBytes => {
            let b = a.bytes(0, h)?;
            num_from_bytes(&b, &a.str(1), &a.str(2), h)?
        }
        jsonStringify => Value::str(json_stringify(&a.v(0), a.int(1), h)?),
        jsonParse => json_parse(&a.str(0)).map_err(|e| h.throw(ExcKind::IllegalArgument, e))?,
        compare => Value::i64(match ops::compare_values(&a.v(0), &a.v(1), h)? {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }),
        _ => unreachable!(),
    })
}

fn norm_encoding(e: &str) -> String {
    e.to_ascii_lowercase().replace(['-', '_', ' '], "")
}

fn unknown_encoding<H: Host>(e: &str, h: &mut H) -> H::Err {
    h.throw(ExcKind::IllegalArgument, format!("unknown text encoding \"{}\" (expected utf-8, utf-16le, utf-16be, ascii or latin-1)", e))
}

pub fn encode<H: Host>(s: &str, enc: &str, h: &mut H) -> Result<Vec<u8>, H::Err> {
    Ok(match norm_encoding(enc).as_str() {
        "" | "utf8" => s.as_bytes().to_vec(),
        "utf16" | "utf16le" => s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect(),
        "utf16be" => s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
        "ascii" | "usascii" => {
            if let Some(c) = s.chars().find(|c| !c.is_ascii()) {
                return Err(h.throw(ExcKind::IllegalArgument, format!("character '{}' cannot be encoded as ASCII", c)));
            }
            s.as_bytes().to_vec()
        }
        "latin1" | "iso88591" => {
            let mut out = Vec::with_capacity(s.len());
            for c in s.chars() {
                if (c as u32) > 0xff {
                    return Err(h.throw(ExcKind::IllegalArgument, format!("character '{}' cannot be encoded as Latin-1", c)));
                }
                out.push(c as u32 as u8);
            }
            out
        }
        _ => return Err(unknown_encoding(enc, h)),
    })
}

pub fn decode<H: Host>(b: &[u8], enc: &str, h: &mut H) -> Result<String, H::Err> {
    let bad = |h: &mut H, what: &str| h.throw(ExcKind::IllegalArgument, format!("bytes are not valid {} text", what));
    Ok(match norm_encoding(enc).as_str() {
        "" | "utf8" => match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            Err(e) => {
                return Err(h.throw(
                    ExcKind::IllegalArgument,
                    format!("bytes are not valid UTF-8 text (invalid byte at offset {}); use toHex() or toBase64() for binary data", e.valid_up_to()),
                ))
            }
        },
        e @ ("utf16" | "utf16le" | "utf16be") => {
            if b.len() % 2 != 0 {
                return Err(bad(h, "UTF-16"));
            }
            let units: Vec<u16> = b.chunks(2).map(|c| if e == "utf16be" { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }).collect();
            match String::from_utf16(&units) {
                Ok(s) => s,
                Err(_) => return Err(bad(h, "UTF-16")),
            }
        }
        "ascii" | "usascii" => {
            if !b.is_ascii() {
                return Err(bad(h, "ASCII"));
            }
            String::from_utf8_lossy(b).into_owned()
        }
        "latin1" | "iso88591" => b.iter().map(|&x| x as char).collect(),
        _ => return Err(unknown_encoding(enc, h)),
    })
}

pub fn to_hex(b: &[u8], upper: bool) -> String {
    let digits: &[u8; 16] = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(digits[(x >> 4) as usize] as char);
        s.push(digits[(x & 15) as usize] as char);
    }
    s
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let s = if s.len() >= 2 && s[0] == b'0' && (s[1] == b'x' || s[1] == b'X') { &s[2..] } else { &s[..] };
    if s.len() % 2 != 0 {
        return None;
    }
    let val = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    s.chunks(2).map(|p| Some(val(p[0])? << 4 | val(p[1])?)).collect()
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Standard base64 with padding, or the URL-safe alphabet without padding.
pub fn to_base64(b: &[u8], url: bool) -> String {
    let t = if url { B64_URL } else { B64 };
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(t[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else if !url {
                s.push('=');
            }
        }
    }
    s
}

/// Accepts both alphabets, with or without padding.
pub fn from_base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut pad = false;
    for c in s.bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            pad = true;
            continue;
        }
        if pad {
            return None;
        }
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if bits >= 6 {
        return None;
    }
    Some(out)
}

fn big_endian<H: Host>(endian: &str, h: &mut H) -> Result<bool, H::Err> {
    match endian.to_ascii_lowercase().as_str() {
        "big" | "be" | "network" => Ok(true),
        "little" | "le" | "" => Ok(false),
        _ => Err(h.throw(ExcKind::IllegalArgument, format!("unknown byte order \"{}\" (expected \"big\" or \"little\")", endian))),
    }
}

fn num_to_bytes<H: Host>(v: &Value, endian: &str, h: &mut H) -> Result<Vec<u8>, H::Err> {
    let big = big_endian(endian, h)?;
    let mut b = match v {
        Value::Int(t, x) => {
            let n = (t.bits() / 8) as usize;
            let le = (*x as u128).to_le_bytes();
            le[..n].to_vec()
        }
        Value::Float(FloatTy::F64, f) => f.to_le_bytes().to_vec(),
        Value::Float(FloatTy::F32, f) => (*f as f32).to_le_bytes().to_vec(),
        Value::Float(FloatTy::F16, f) => f32_to_f16_bits(*f as f32).to_le_bytes().to_vec(),
        Value::Big(x) => {
            // minimal two's complement: the smallest k with -2^(8k-1) <= x < 2^(8k-1)
            let base = BigInt::from_i128(256);
            let mut k = 1usize;
            let mut half = BigInt::from_i128(128);
            while !(x.cmp(&half.neg()) != std::cmp::Ordering::Less && x.cmp(&half) == std::cmp::Ordering::Less) {
                k += 1;
                half = half.mul(&base);
            }
            let mut y = if x.is_negative() { x.add(&half.mul(&BigInt::from_i128(2))) } else { (**x).clone() };
            let mut out = Vec::with_capacity(k);
            for _ in 0..k {
                let (q, r) = y.divmod(&base).unwrap_or((BigInt::zero(), BigInt::zero()));
                out.push(r.to_i128().unwrap_or(0) as u8);
                y = q;
            }
            out
        }
        other => return Err(h.throw(ExcKind::IllegalArgument, format!("toBytes needs a number, got {}", other.type_name()))),
    };
    if big {
        b.reverse();
    }
    Ok(b)
}

fn num_from_bytes<H: Host>(b: &[u8], code: &str, endian: &str, h: &mut H) -> Result<Value, H::Err> {
    let big = big_endian(endian, h)?;
    let mut le = b.to_vec();
    if big {
        le.reverse();
    }
    let ty = RtType::decode(code).unwrap_or(RtType::Any);
    let need = |n: usize, h: &mut H| -> Result<(), H::Err> {
        if le.len() != n {
            return Err(h.throw(ExcKind::IllegalArgument, format!("{} needs exactly {} byte(s), got {}", code, n, le.len())));
        }
        Ok(())
    };
    Ok(match ty {
        RtType::Int(t) => {
            let n = (t.bits() / 8) as usize;
            need(n, h)?;
            let mut buf = [0u8; 16];
            buf[..n].copy_from_slice(&le);
            let raw = u128::from_le_bytes(buf);
            let v = if t.signed() { t.wrap(raw as i128) } else { raw as i128 };
            Value::Int(t, v)
        }
        RtType::Float(FloatTy::F64) => {
            need(8, h)?;
            Value::Float(FloatTy::F64, f64::from_le_bytes(le[..8].try_into().unwrap()))
        }
        RtType::Float(FloatTy::F32) => {
            need(4, h)?;
            Value::Float(FloatTy::F32, f32::from_le_bytes(le[..4].try_into().unwrap()) as f64)
        }
        RtType::Float(FloatTy::F16) => {
            need(2, h)?;
            Value::Float(FloatTy::F16, f16_bits_to_f32(u16::from_le_bytes([le[0], le[1]])) as f64)
        }
        RtType::Big => {
            if le.is_empty() {
                return Ok(Value::Big(Rc::new(BigInt::zero())));
            }
            let neg = le[le.len() - 1] & 0x80 != 0;
            let mut n = BigInt::zero();
            let base = BigInt::from_i128(256);
            for &x in le.iter().rev() {
                n = n.mul(&base).add(&BigInt::from_i128(x as i128));
            }
            if neg {
                let mut m = BigInt::from_i128(1);
                for _ in 0..le.len() {
                    m = m.mul(&base);
                }
                n = n.sub(&m);
            }
            Value::Big(Rc::new(n))
        }
        _ => return Err(h.throw(ExcKind::IllegalArgument, format!("fromBytes: not a numeric type ({})", code))),
    })
}

// ---------------------------------------------------------------------------------------------
// JSON

fn json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// JSON text of a value; `indent > 0` pretty-prints.
pub fn json_stringify<H: Host>(v: &Value, indent: i64, h: &mut H) -> Result<String, H::Err> {
    let mut out = String::new();
    json_write(v, indent.max(0) as usize, 0, &mut out, h)?;
    Ok(out)
}

fn json_write<H: Host>(v: &Value, ind: usize, depth: usize, out: &mut String, h: &mut H) -> Result<(), H::Err> {
    let nl = |out: &mut String, d: usize| {
        if ind > 0 {
            out.push('\n');
            out.push_str(&" ".repeat(ind * d));
        }
    };
    match v.deref() {
        Value::Null | Value::Void => out.push_str("null"),
        Value::Bool(b) => out.push_str(if b { "true" } else { "false" }),
        Value::Int(_, x) => out.push_str(&x.to_string()),
        Value::Big(b) => out.push_str(&b.to_string()),
        Value::Float(t, f) => {
            if !f.is_finite() {
                return Err(h.throw(ExcKind::IllegalArgument, format!("JSON cannot represent {}", ops::fmt_float(t, f))));
            }
            out.push_str(&ops::fmt_float(t, f));
        }
        Value::Str(s) => json_string(&s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, depth + 1);
                json_write(&x, ind, depth + 1, out, h)?;
            }
            if !a.items.is_empty() {
                nl(out, depth);
            }
            out.push(']');
        }
        Value::Tuple(t) => {
            let arr = Value::array(t.to_vec(), false);
            json_write(&arr, ind, depth, out, h)?;
        }
        Value::Dict(d) => {
            out.push('{');
            for (i, (k, x)) in d.entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, depth + 1);
                let key = match k.deref() {
                    Value::Str(s) => s.to_string(),
                    other => ops::to_display(&other, h)?,
                };
                json_string(&key, out);
                out.push(':');
                if ind > 0 {
                    out.push(' ');
                }
                json_write(x, ind, depth + 1, out, h)?;
            }
            if !d.entries.is_empty() {
                nl(out, depth);
            }
            out.push('}');
        }
        other => return Err(h.throw(ExcKind::IllegalArgument, format!("JSON cannot represent a value of type {}", other.type_name()))),
    }
    Ok(())
}

/// Parses JSON: objects become `Dictionary[String, DTVariable]`, integers `Int64` (or
/// `IntLarge`), other numbers `Float64`.
pub fn json_parse(s: &str) -> Result<Value, String> {
    let mut p = Json { s: s.as_bytes(), i: 0, text: s };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("unexpected text after the JSON value"));
    }
    Ok(v)
}

struct Json<'a> {
    s: &'a [u8],
    i: usize,
    text: &'a str,
}

impl Json<'_> {
    fn err(&self, msg: &str) -> String {
        let line = self.text[..self.i.min(self.text.len())].matches('\n').count() + 1;
        format!("invalid JSON at line {}, offset {}: {}", line, self.i, msg)
    }
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn lit(&mut self, w: &str, v: Value) -> Result<Value, String> {
        if self.s[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(self.err("unexpected token"))
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > 512 {
            return Err(self.err("nesting too deep"));
        }
        match self.s.get(self.i) {
            None => Err(self.err("unexpected end of input")),
            Some(b'{') => {
                self.i += 1;
                let mut d = DictVal::default();
                self.ws();
                if self.eat(b'}') {
                    return Ok(Value::Dict(Rc::new(d)));
                }
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&b'"') {
                        return Err(self.err("expected a string key"));
                    }
                    let k = self.string()?;
                    self.ws();
                    if !self.eat(b':') {
                        return Err(self.err("expected ':'"));
                    }
                    self.ws();
                    let v = self.value(depth + 1)?;
                    d.insert(Value::str(k), v);
                    self.ws();
                    if self.eat(b'}') {
                        return Ok(Value::Dict(Rc::new(d)));
                    }
                    if !self.eat(b',') {
                        return Err(self.err("expected ',' or '}'"));
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat(b']') {
                    return Ok(Value::array(items, false));
                }
                loop {
                    self.ws();
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat(b']') {
                        return Ok(Value::array(items, false));
                    }
                    if !self.eat(b',') {
                        return Err(self.err("expected ',' or ']'"));
                    }
                }
            }
            Some(b'"') => Ok(Value::str(self.string()?)),
            Some(b't') => self.lit("true", Value::Bool(true)),
            Some(b'f') => self.lit("false", Value::Bool(false)),
            Some(b'n') => self.lit("null", Value::Null),
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
            Some(_) => Err(self.err("unexpected character")),
        }
    }
    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.i < self.s.len() && self.s[self.i] != b'"' && self.s[self.i] != b'\\' && self.s[self.i] >= 0x20 {
                self.i += 1;
            }
            out.push_str(&self.text[start..self.i]);
            match self.s.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = *self.s.get(self.i).ok_or_else(|| self.err("unterminated string"))?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xd800..0xdc00).contains(&cp) && self.s[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xd800) << 10) + (lo.wrapping_sub(0xdc00) & 0x3ff);
                            }
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(self.err("invalid escape")),
                    }
                }
                _ => return Err(self.err("unterminated string")),
            }
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.text.get(self.i..self.i + 4).ok_or_else(|| self.err("bad \\u escape"))?;
        let v = u32::from_str_radix(h, 16).map_err(|_| self.err("bad \\u escape"))?;
        self.i += 4;
        Ok(v)
    }
    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        self.eat(b'-');
        let mut float = false;
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'0'..=b'9' => {}
                b'.' | b'e' | b'E' | b'+' | b'-' => float = true,
                _ => break,
            }
            self.i += 1;
        }
        let t = &self.text[start..self.i];
        if !float {
            if let Ok(v) = t.parse::<i64>() {
                return Ok(Value::i64(v));
            }
            if let Some(b) = BigInt::parse(t) {
                return Ok(Value::Big(Rc::new(b)));
            }
        }
        t.parse::<f64>().map(|f| Value::Float(FloatTy::F64, f)).map_err(|_| self.err("invalid number"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_hex() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            let e = to_base64(s.as_bytes(), false);
            assert_eq!(from_base64(&e).unwrap(), s.as_bytes());
            assert_eq!(from_base64(&to_base64(s.as_bytes(), true)).unwrap(), s.as_bytes());
        }
        assert_eq!(to_base64(b"foobar", false), "Zm9vYmFy");
        assert_eq!(to_base64(b"fo", false), "Zm8=");
        assert_eq!(to_hex(&[0, 255, 16], false), "00ff10");
        assert_eq!(from_hex("00FF10").unwrap(), vec![0, 255, 16]);
        assert!(from_hex("abc").is_none());
    }

    #[test]
    fn json_round_trip() {
        let v = json_parse(r#"{"a": [1, 2.5, "x\né"], "b": {"c": null, "d": true}, "big": 123456789012345678901234567890}"#).unwrap();
        let Value::Dict(d) = &v else { panic!() };
        assert_eq!(d.entries.len(), 3);
        assert!(json_parse("[1,]").is_err());
        assert!(json_parse("{} x").is_err());
    }
}
