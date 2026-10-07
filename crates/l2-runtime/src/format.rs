//! Python's format specification mini-language, used by f-string fields (`f"{x:>10.2f}"`) and
//! `.format()` placeholders (`"%x:.2f%"`):
//!
//! `[[fill]align][sign][z][#][0][width][grouping][.precision][type]`

use crate::bigint::BigInt;
use crate::ops;
use crate::value::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub fill: char,
    pub align: Option<char>,
    pub sign: char,
    pub z: bool,
    pub alt: bool,
    pub zero: bool,
    pub width: usize,
    pub grouping: Option<char>,
    pub precision: Option<usize>,
    pub ty: Option<char>,
}

const TYPES: &str = "bcdeEfFgGnosxX%";

pub fn parse_spec(s: &str) -> Result<Spec, String> {
    let c: Vec<char> = s.chars().collect();
    let bad = || format!("invalid format specifier '{}'", s);
    let mut sp = Spec { fill: ' ', align: None, sign: '-', z: false, alt: false, zero: false, width: 0, grouping: None, precision: None, ty: None };
    let mut i = 0;
    let is_align = |ch: char| matches!(ch, '<' | '>' | '=' | '^');
    if c.len() >= 2 && is_align(c[1]) {
        sp.fill = c[0];
        sp.align = Some(c[1]);
        i = 2;
    } else if !c.is_empty() && is_align(c[0]) {
        sp.align = Some(c[0]);
        i = 1;
    }
    if i < c.len() && matches!(c[i], '+' | '-' | ' ') {
        sp.sign = c[i];
        i += 1;
    }
    if i < c.len() && c[i] == 'z' {
        sp.z = true;
        i += 1;
    }
    if i < c.len() && c[i] == '#' {
        sp.alt = true;
        i += 1;
    }
    if i < c.len() && c[i] == '0' {
        sp.zero = true;
        i += 1;
    }
    let start = i;
    while i < c.len() && c[i].is_ascii_digit() {
        i += 1;
    }
    if i > start {
        sp.width = c[start..i].iter().collect::<String>().parse().map_err(|_| bad())?;
    }
    if i < c.len() && matches!(c[i], ',' | '_') {
        sp.grouping = Some(c[i]);
        i += 1;
    }
    if i < c.len() && c[i] == '.' {
        i += 1;
        let ps = i;
        while i < c.len() && c[i].is_ascii_digit() {
            i += 1;
        }
        if i == ps {
            return Err(format!("format specifier '{}' is missing the precision after '.'", s));
        }
        sp.precision = Some(c[ps..i].iter().collect::<String>().parse().map_err(|_| bad())?);
    }
    if i < c.len() && TYPES.contains(c[i]) {
        sp.ty = Some(c[i]);
        i += 1;
    }
    if i != c.len() {
        return Err(bad());
    }
    Ok(sp)
}

/// The broad kind of a value for checking a specifier against its type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Int,
    Float,
    Bool,
    Text,
}

/// Checks that a specifier can format values of `kind` (also used at compile time).
pub fn check_spec(sp: &Spec, kind: Kind, type_name: &str) -> Result<(), String> {
    let unknown = |t: char| Err(format!("unknown format code '{}' for a value of type {}", t, type_name));
    match (kind, sp.ty) {
        (Kind::Text, Some(t)) if t != 's' => return unknown(t),
        (Kind::Float, Some(t)) if "bcdosxX".contains(t) => return unknown(t),
        (Kind::Int | Kind::Bool, Some('s')) => return unknown('s'),
        _ => {}
    }
    let numeric = match kind {
        Kind::Int | Kind::Float => true,
        Kind::Bool => sp.ty.is_some(),
        Kind::Text => false,
    };
    if !numeric {
        if sp.sign != '-' {
            return Err("sign not allowed in a string format specifier".into());
        }
        if sp.align == Some('=') {
            return Err("'=' alignment not allowed in a string format specifier".into());
        }
        if sp.grouping.is_some() {
            return Err("cannot specify ',' or '_' with a string".into());
        }
        if sp.alt {
            return Err("alternate form (#) not allowed in a string format specifier".into());
        }
    }
    if let (Some(','), Some(t)) = (sp.grouping, sp.ty) {
        if "bcoxXn".contains(t) {
            return Err(format!("cannot specify ',' with '{}'", t));
        }
    }
    if kind == Kind::Int && sp.precision.is_some() && sp.ty.map(|t| "bcdoxXn".contains(t)).unwrap_or(true) {
        return Err("precision not allowed in an integer format specifier".into());
    }
    Ok(())
}

fn group(digits: &str, sep: char, every: usize) -> String {
    let n = digits.chars().count();
    let mut out = String::with_capacity(n + n / every);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(every) {
            out.push(sep);
        }
        out.push(ch);
    }
    out
}

/// Pads `sign + prefix + body` to the width.
fn pad(sp: &Spec, sign: &str, prefix: &str, body: &str, default_align: char) -> String {
    let (fill, align) = match sp.align {
        Some(a) => (sp.fill, a),
        None if sp.zero => ('0', '='),
        None => (sp.fill, default_align),
    };
    let len = sign.chars().count() + prefix.chars().count() + body.chars().count();
    if len >= sp.width {
        return format!("{}{}{}", sign, prefix, body);
    }
    let n = sp.width - len;
    let fills = |k: usize| std::iter::repeat_n(fill, k).collect::<String>();
    match align {
        '<' => format!("{}{}{}{}", sign, prefix, body, fills(n)),
        '^' => format!("{}{}{}{}{}", fills(n / 2), sign, prefix, body, fills(n - n / 2)),
        '=' => format!("{}{}{}{}", sign, prefix, fills(n), body),
        _ => format!("{}{}{}{}", fills(n), sign, prefix, body),
    }
}

fn sign_str(sp: &Spec, neg: bool) -> &'static str {
    match (neg, sp.sign) {
        (true, _) => "-",
        (false, '+') => "+",
        (false, ' ') => " ",
        _ => "",
    }
}

/// Groups the integer part of a decimal number string (`1234567.25` -> `1,234,567.25`).
fn group_decimal(body: &str, sep: Option<char>) -> String {
    let Some(sep) = sep else { return body.to_string() };
    let end = body.find(|c: char| !c.is_ascii_digit()).unwrap_or(body.len());
    format!("{}{}", group(&body[..end], sep, 3), &body[end..])
}

fn big_radix(b: &BigInt, radix: u32) -> String {
    let neg = b.is_negative();
    let mut m = if neg { b.neg() } else { b.clone() };
    if m.is_zero() {
        return "0".into();
    }
    let base = BigInt::from_i128(radix as i128);
    let mut digits = Vec::new();
    while !m.is_zero() {
        let (q, r) = m.divmod(&base).unwrap();
        let d = r.to_i128().unwrap_or(0) as u32;
        digits.push(std::char::from_digit(d, radix).unwrap_or('0'));
        m = q;
    }
    digits.iter().rev().collect()
}

fn format_int(sp: &Spec, neg: bool, mag_dec: String, mag_radix: &dyn Fn(u32) -> String, as_char: Option<char>) -> Result<String, String> {
    let ty = sp.ty.unwrap_or('d');
    let (prefix, digits, every) = match ty {
        'd' | 'n' => ("", mag_dec, 3),
        'b' => (if sp.alt { "0b" } else { "" }, mag_radix(2), 4),
        'o' => (if sp.alt { "0o" } else { "" }, mag_radix(8), 4),
        'x' => (if sp.alt { "0x" } else { "" }, mag_radix(16), 4),
        'X' => (if sp.alt { "0X" } else { "" }, mag_radix(16).to_uppercase(), 4),
        'c' => {
            let ch = as_char.ok_or_else(|| "%c arg not in range(0x110000)".to_string())?;
            return Ok(pad(sp, "", "", &ch.to_string(), '<'));
        }
        _ => unreachable!(),
    };
    let body = match sp.grouping {
        Some(sep) => group(&digits, sep, every),
        None => digits,
    };
    Ok(pad(sp, sign_str(sp, neg), prefix, &body, '>'))
}

fn exp_python(s: &str, upper: bool) -> String {
    // Rust: 1.5e3 / 1.5e-7  ->  Python: 1.5e+03 / 1.5e-07
    let (m, e) = s.split_once('e').unwrap_or((s, "0"));
    let (sign, digits) = match e.strip_prefix('-') {
        Some(d) => ('-', d),
        None => ('+', e),
    };
    let out = format!("{}e{}{:0>2}", m, sign, digits);
    if upper {
        out.to_uppercase()
    } else {
        out
    }
}

fn strip_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    let (m, rest) = match s.find('e') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let m = m.trim_end_matches('0').trim_end_matches('.');
    format!("{}{}", m, rest)
}

/// Python's `g` rules; `keep_point` is the "None" presentation (at least one fraction digit).
fn general(a: f64, p: usize, alt: bool, upper: bool, keep_point: bool) -> String {
    let p = p.max(1);
    let sci = format!("{:.*e}", p - 1, a);
    let exp: i32 = sci.split_once('e').map(|(_, e)| e.parse().unwrap_or(0)).unwrap_or(0);
    let limit = if keep_point { p as i32 - 1 } else { p as i32 };
    let mut s = if (-4..limit).contains(&exp) {
        let decimals = (p as i32 - 1 - exp).max(0) as usize;
        let f = format!("{:.*}", decimals, a);
        if alt {
            f
        } else {
            strip_zeros(&f)
        }
    } else {
        let e = exp_python(&sci, upper);
        if alt {
            e
        } else {
            strip_zeros(&e)
        }
    };
    if keep_point && !s.contains('.') && !s.contains('e') && !s.contains('E') {
        s.push_str(".0");
    }
    if alt && !s.contains('.') {
        match s.find(['e', 'E']) {
            Some(i) => s.insert(i, '.'),
            None => s.push('.'),
        }
    }
    s
}

fn format_float(sp: &Spec, ty: FloatTy, v: f64) -> String {
    let neg = v.is_sign_negative() && !v.is_nan();
    let a = v.abs();
    let upper = matches!(sp.ty, Some('E' | 'F' | 'G'));
    let mut body = if !v.is_finite() {
        let s = if v.is_nan() { "nan" } else { "inf" };
        match sp.ty {
            None if sp.precision.is_none() => (if v.is_nan() { "NaN" } else { "Infinity" }).to_string(),
            _ if upper => s.to_uppercase(),
            _ => s.to_string(),
        }
    } else {
        match sp.ty {
            Some('f' | 'F') => {
                let s = format!("{:.*}", sp.precision.unwrap_or(6), a);
                if sp.alt && !s.contains('.') {
                    format!("{}.", s)
                } else {
                    s
                }
            }
            Some('e' | 'E') => {
                let s = exp_python(&format!("{:.*e}", sp.precision.unwrap_or(6), a), upper);
                if sp.alt && !s.contains('.') {
                    s.replacen('e', ".e", 1).replacen('E', ".E", 1)
                } else {
                    s
                }
            }
            Some('g' | 'G' | 'n') => general(a, sp.precision.unwrap_or(6), sp.alt, upper, false),
            Some('%') => {
                let s = format!("{:.*}%", sp.precision.unwrap_or(6), a * 100.0);
                if sp.alt && !s.contains('.') {
                    s.replace('%', ".%")
                } else {
                    s
                }
            }
            _ => match sp.precision {
                Some(p) => general(a, p, sp.alt, false, true),
                None => ops::fmt_float(ty, a),
            },
        }
    };
    let mut neg = neg;
    if sp.z && neg && body.chars().all(|c| matches!(c, '0' | '.' | '%' | 'e' | 'E' | '+' | '-')) {
        neg = false;
    }
    if let Some(sep) = sp.grouping {
        body = group_decimal(&body, Some(sep));
    }
    pad(sp, sign_str(sp, neg), "", &body, '>')
}

fn format_text(sp: &Spec, s: &str) -> String {
    let s: String = match sp.precision {
        Some(p) => s.chars().take(p).collect(),
        None => s.to_string(),
    };
    pad(sp, "", "", &s, '<')
}

/// `format(value, spec)` with an optional conversion (`!r` / `!s`).
pub fn format_value<H: Host>(v: &Value, spec: &str, conv: &str, h: &mut H) -> Result<String, H::Err> {
    let v = v.deref();
    let v = match conv {
        "r" => Value::str(ops::to_display_nested(&v, h)?),
        "s" => Value::str(ops::to_display(&v, h)?),
        _ => v,
    };
    if spec.is_empty() {
        return ops::to_display(&v, h);
    }
    let sp = parse_spec(spec).map_err(|e| h.throw(ExcKind::IllegalArgument, e))?;
    let checked = |kind: Kind, h: &mut H, name: &str| check_spec(&sp, kind, name).map_err(|e| h.throw(ExcKind::IllegalArgument, e));
    match &v {
        Value::Int(t, x) => {
            checked(Kind::Int, h, t.name())?;
            if matches!(sp.ty, Some('e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%')) {
                return Ok(format_float(&sp, FloatTy::F64, *x as f64));
            }
            let m = x.unsigned_abs();
            let ch = u32::try_from(*x).ok().and_then(char::from_u32);
            format_int(&sp, *x < 0, m.to_string(), &|r| radix_u128(m, r), ch).map_err(|e| h.throw(ExcKind::IllegalArgument, e))
        }
        Value::Big(b) => {
            checked(Kind::Int, h, "IntLarge")?;
            if matches!(sp.ty, Some('e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%')) {
                return Ok(format_float(&sp, FloatTy::F64, b.to_f64()));
            }
            let mag = if b.is_negative() { b.neg() } else { (**b).clone() };
            let ch = b.to_i128().and_then(|x| u32::try_from(x).ok()).and_then(char::from_u32);
            format_int(&sp, b.is_negative(), mag.to_string(), &|r| big_radix(&mag, r), ch).map_err(|e| h.throw(ExcKind::IllegalArgument, e))
        }
        Value::Float(t, f) => {
            checked(Kind::Float, h, t.name())?;
            Ok(format_float(&sp, *t, *f))
        }
        Value::Bool(b) => {
            checked(Kind::Bool, h, "Boolean")?;
            if sp.ty.is_some() {
                let x = *b as u128;
                return format_int(&sp, false, x.to_string(), &|r| radix_u128(x, r), None).map_err(|e| h.throw(ExcKind::IllegalArgument, e));
            }
            Ok(format_text(&sp, if *b { "true" } else { "false" }))
        }
        other => {
            let name = match other {
                Value::Str(_) => "String".to_string(),
                Value::Object(o) => h.class_name(o.class),
                other => other.type_name(),
            };
            checked(Kind::Text, h, &name)?;
            let s = ops::to_display(other, h)?;
            Ok(format_text(&sp, &s))
        }
    }
}

fn radix_u128(m: u128, r: u32) -> String {
    match r {
        2 => format!("{:b}", m),
        8 => format!("{:o}", m),
        16 => format!("{:x}", m),
        _ => m.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: f64, s: &str) -> String {
        format_float(&parse_spec(s).unwrap(), FloatTy::F64, v)
    }

    fn i(v: i128, s: &str) -> String {
        let sp = parse_spec(s).unwrap();
        let m = v.unsigned_abs();
        format_int(&sp, v < 0, m.to_string(), &|r| radix_u128(m, r), None).unwrap()
    }

    #[test]
    fn python_compat() {
        assert_eq!(f(1.23456, ".2f"), "1.23");
        assert_eq!(f(1.23456, "8.3f"), "   1.235");
        assert_eq!(f(1.23456, "<8.1f"), "1.2     ");
        assert_eq!(f(-3.5, "08.2f"), "-0003.50");
        assert_eq!(f(1234567.891, ",.2f"), "1,234,567.89");
        assert_eq!(f(0.25, ".1%"), "25.0%");
        assert_eq!(f(12345.678, ".3e"), "1.235e+04");
        assert_eq!(f(0.0001234, "g"), "0.0001234");
        assert_eq!(f(123456789.0, "g"), "1.23457e+08");
        assert_eq!(f(2.0, ".3"), "2.0");
        assert_eq!(f(-0.0001, "z.2f"), "0.00");
        assert_eq!(f(1.5, "+"), "+1.5");
        assert_eq!(f(1.5, "*^9"), "***1.5***");
        assert_eq!(i(255, "#x"), "0xff");
        assert_eq!(i(255, "08b"), "11111111");
        assert_eq!(i(1234567, "_"), "1_234_567");
        assert_eq!(i(-42, "=+6"), "-   42");
        assert_eq!(i(42, "^7"), "  42   ");
        assert!(parse_spec(".f").is_err());
        assert!(parse_spec("10q").is_err());
    }
}
