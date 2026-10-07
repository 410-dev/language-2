//! Library functions shared by every backend (strings, numbers, arrays, dictionaries, stdio).

use crate::numeric::{self, RoundMode, RoundSpec};
use crate::ops::*;
use crate::value::*;
use std::cell::Cell;
use std::io::{BufRead, Write};
use std::rc::Rc;

macro_rules! builtins {
    ($($name:ident),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u16)]
        pub enum Builtin { $($name),* }
        impl Builtin {
            pub const ALL: &'static [Builtin] = &[$(Builtin::$name),*];
            pub fn from_u16(v: u16) -> Builtin { Self::ALL[v as usize] }
            pub fn name(self) -> &'static str {
                match self { $(Builtin::$name => stringify!($name)),* }
            }
        }
    };
}

builtins! {
    // generic
    ToString, Clone, Equals, SameRef,
    // stdio
    Println, Print, Read, ReplaceLine,
    // String
    StrLength, StrIsEmpty, StrSubstring, StrStartsWith, StrEndsWith, StrContains, StrReplace,
    StrSplit, StrCharacters, StrFormat, StrFormatInj, StrParse, StrIndexOf, StrToUpper,
    StrToLower, StrTrim, StrAppend, StrPrepend, StrRandomize, StrRandom,
    // numbers
    NumFormat, NumRandomize, NumRandom, NumRange, NumMax, NumMin,
    // arrays
    ArrNew, ArrLength, ArrIsEmpty, ArrAt, ArrFirst, ArrLast, ArrKv, ArrSet, ArrFill, ArrReverse,
    ArrSort, ArrShuffle, ArrInsert, ArrRemove, ArrContains,
    // dictionaries
    DictGet, DictSet, DictKv, DictMerge, DictLength, DictIsEmpty, DictContainsKey, DictRemove,
    DictKeys, DictValues,
    // rounding, formatting, default toString
    NumRound, NumCeil, NumFloor, NumAbs, FormatValue, DefaultToString,
    // bulk numeric kernels (math.linear intrinsics)
    TensorZip, TensorScalar, TensorMatMul, TensorRound, TensorMigrate, TensorTranspose,
}

impl Builtin {
    /// Mutating builtins operate on a storage location (the receiver) instead of a value.
    pub fn is_mutating(self) -> bool {
        use Builtin::*;
        matches!(
            self,
            StrAppend
                | StrPrepend
                | StrRandomize
                | NumRandomize
                | ArrSet
                | ArrFill
                | ArrReverse
                | ArrSort
                | ArrShuffle
                | ArrInsert
                | ArrRemove
                | DictSet
                | DictMerge
                | DictRemove
        )
    }
}

// ---------------------------------------------------------------------------------------------
// random numbers (xorshift64*)

thread_local! {
    static RNG: Cell<u64> = Cell::new(initial_seed());
}

fn initial_seed() -> u64 {
    if let Ok(s) = std::env::var("L2_SEED") {
        if let Ok(v) = s.parse::<u64>() {
            return v.max(1);
        }
    }
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(42);
    t | 1
}

pub fn next_random() -> u64 {
    RNG.with(|r| {
        let mut x = r.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        r.set(x);
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    })
}

fn random_below(n: u128) -> u128 {
    if n == 0 {
        return 0;
    }
    let r = ((next_random() as u128) << 64) | next_random() as u128;
    r % n
}

fn random_f64() -> f64 {
    (next_random() >> 11) as f64 / (1u64 << 53) as f64
}

/// Generates a random string whose characters come from the character set described by the
/// first atom of `regex` (`[a-z0-9]`, `\d`, `\w`, `.`), or from its literal characters.
pub fn random_string(regex: &str, min: i64, max: i64) -> String {
    let set = regex_charset(regex);
    let (lo, hi) = (min.max(0), max.max(min.max(0)));
    let len = lo + random_below((hi - lo + 1) as u128) as i64;
    let mut s = String::new();
    if set.is_empty() {
        return s;
    }
    for _ in 0..len {
        s.push(set[random_below(set.len() as u128) as usize]);
    }
    s
}

fn regex_charset(re: &str) -> Vec<char> {
    let chars: Vec<char> = re.chars().collect();
    let mut i = 0;
    let mut set = Vec::new();
    let push_range = |set: &mut Vec<char>, a: char, b: char| {
        for c in a..=b {
            set.push(c);
        }
    };
    while i < chars.len() {
        match chars[i] {
            '[' => {
                i += 1;
                let negate = i < chars.len() && chars[i] == '^';
                if negate {
                    i += 1;
                }
                let mut cls = Vec::new();
                while i < chars.len() && chars[i] != ']' {
                    let c = if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                        match chars[i] {
                            'd' => {
                                push_range(&mut cls, '0', '9');
                                i += 1;
                                continue;
                            }
                            'w' => {
                                push_range(&mut cls, 'a', 'z');
                                push_range(&mut cls, 'A', 'Z');
                                push_range(&mut cls, '0', '9');
                                cls.push('_');
                                i += 1;
                                continue;
                            }
                            c => c,
                        }
                    } else {
                        chars[i]
                    };
                    if i + 2 < chars.len() && chars[i + 1] == '-' && chars[i + 2] != ']' {
                        push_range(&mut cls, c, chars[i + 2]);
                        i += 3;
                    } else {
                        cls.push(c);
                        i += 1;
                    }
                }
                if negate {
                    let all: Vec<char> = (' '..='~').collect();
                    cls = all.into_iter().filter(|c| !cls.contains(c)).collect();
                }
                return cls;
            }
            '\\' if i + 1 < chars.len() => {
                match chars[i + 1] {
                    'd' => push_range(&mut set, '0', '9'),
                    'w' => {
                        push_range(&mut set, 'a', 'z');
                        push_range(&mut set, 'A', 'Z');
                        push_range(&mut set, '0', '9');
                        set.push('_');
                    }
                    c => set.push(c),
                }
                return set;
            }
            '.' => return (' '..='~').collect(),
            '*' | '+' | '?' | '{' | '}' | '(' | ')' | '|' | '^' | '$' => {}
            c => set.push(c),
        }
        i += 1;
    }
    set.sort();
    set.dedup();
    set
}

// ---------------------------------------------------------------------------------------------
// stdio

pub fn write_out(s: &str) {
    let out = std::io::stdout();
    let mut l = out.lock();
    let _ = l.write_all(s.as_bytes());
}

pub fn flush_out() {
    let _ = std::io::stdout().flush();
}

pub fn read_line(prompt: &str) -> String {
    write_out(prompt);
    flush_out();
    let mut s = String::new();
    let _ = std::io::stdin().lock().read_line(&mut s);
    while s.ends_with('\n') || s.ends_with('\r') {
        s.pop();
    }
    s
}

// ---------------------------------------------------------------------------------------------
// helpers

fn norm_index<H: Host>(i: i128, len: usize, h: &mut H) -> Result<usize, H::Err> {
    let l = len as i128;
    let j = if i < 0 { i + l } else { i };
    if j < 0 || j >= l {
        return Err(h.throw(ExcKind::IndexOutOfBounds, format!("Index {} out of bounds for length {}", i, len)));
    }
    Ok(j as usize)
}

fn arg_int(args: &[Value], i: usize, default: i128) -> i128 {
    match args.get(i) {
        Some(v) => v.deref().as_int(),
        None => default,
    }
}

fn arg_str(args: &[Value], i: usize) -> String {
    match args.get(i).map(|v| v.deref()) {
        Some(Value::Str(s)) => s.to_string(),
        _ => String::new(),
    }
}

fn is_placeholder_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Finds `%name%` and `%name:spec%` placeholders; returns byte ranges.
pub fn find_placeholders(s: &str) -> Vec<(usize, usize)> {
    find_placeholders_spec(s).into_iter().map(|(a, b, _)| (a, b)).collect()
}

/// Like [`find_placeholders`], also returning the format specifier of `%name:spec%`.
pub fn find_placeholders_spec(s: &str) -> Vec<(usize, usize, Option<String>)> {
    let mut out = Vec::new();
    let b: Vec<(usize, char)> = s.char_indices().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i].1 == '%' {
            let mut j = i + 1;
            while j < b.len() && is_placeholder_char(b[j].1) {
                j += 1;
            }
            if j > i + 1 && j < b.len() {
                if b[j].1 == '%' {
                    out.push((b[i].0, b[j].0 + 1, None));
                    i = j + 1;
                    continue;
                }
                if b[j].1 == ':' {
                    let mut k = j + 1;
                    while k < b.len() && b[k].1 != '%' && b[k].1 != '\n' {
                        k += 1;
                    }
                    if k < b.len() && b[k].1 == '%' {
                        let spec = s[b[j].0 + 1..b[k].0].to_string();
                        out.push((b[i].0, b[k].0 + 1, Some(spec)));
                        i = k + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

pub fn substitute_placeholders<H: Host>(s: &str, args: &[Value], h: &mut H) -> Result<String, H::Err> {
    let ph = find_placeholders_spec(s);
    if ph.len() != args.len() {
        return Err(h.throw(
            ExcKind::IllegalArgument,
            format!("format expects {} argument(s) but {} were given", ph.len(), args.len()),
        ));
    }
    let mut out = String::new();
    let mut last = 0;
    for ((a, b, spec), v) in ph.iter().zip(args.iter()) {
        out.push_str(&s[last..*a]);
        match spec {
            Some(sp) => out.push_str(&crate::format::format_value(v, sp, "", h)?),
            None => out.push_str(&to_display(v, h)?),
        }
        last = *b;
    }
    out.push_str(&s[last..]);
    Ok(out)
}

fn parse_as<H: Host>(s: &str, ty: &RtType, h: &mut H) -> Result<Value, H::Err> {
    let bad = |h: &mut H| h.throw(ExcKind::IllegalArgument, format!("cannot parse \"{}\" as {}", s, ty.encode()));
    match ty {
        RtType::Int(t) => match s.parse::<i128>() {
            Ok(v) if t.fits(v) => Ok(Value::Int(*t, v)),
            _ => Err(bad(h)),
        },
        RtType::Big => match crate::bigint::BigInt::parse(s) {
            Some(b) => Ok(Value::Big(Rc::new(b))),
            None => Err(bad(h)),
        },
        RtType::Float(t) => match s.trim().parse::<f64>() {
            Ok(v) => Ok(Value::Float(*t, t.round(v))),
            Err(_) => Err(bad(h)),
        },
        RtType::Bool => match s {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(bad(h)),
        },
        RtType::Str => Ok(Value::str(s)),
        _ => Err(bad(h)),
    }
}

fn merge_sort<H: Host>(v: Vec<Value>, h: &mut H) -> Result<Vec<Value>, H::Err> {
    if v.len() <= 1 {
        return Ok(v);
    }
    let mut v = v;
    let right = v.split_off(v.len() / 2);
    let left = merge_sort(v, h)?;
    let right = merge_sort(right, h)?;
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut li, mut ri) = (left.into_iter().peekable(), right.into_iter().peekable());
    while li.peek().is_some() && ri.peek().is_some() {
        let o = compare_values(li.peek().unwrap(), ri.peek().unwrap(), h)?;
        if o == std::cmp::Ordering::Greater {
            out.push(ri.next().unwrap());
        } else {
            out.push(li.next().unwrap());
        }
    }
    out.extend(li);
    out.extend(ri);
    Ok(out)
}

fn ensure_growable<H: Host>(a: &ArrayVal, what: &str, h: &mut H) -> Result<(), H::Err> {
    if a.fixed {
        return Err(h.throw(ExcKind::UnsupportedOperation, format!("{} on a length-immutable array", what)));
    }
    Ok(())
}

fn str_arg<H: Host>(v: &Value, h: &mut H) -> Result<Rc<String>, H::Err> {
    match v.deref() {
        Value::Str(s) => Ok(s),
        Value::Null => Err(h.throw(ExcKind::NullPointer, "String receiver is Null".into())),
        other => Err(h.throw(ExcKind::ClassCast, format!("expected String, got {}", other.type_name()))),
    }
}

fn arr_arg<H: Host>(v: &Value, h: &mut H) -> Result<Rc<ArrayVal>, H::Err> {
    match v.deref() {
        Value::Array(a) => Ok(a),
        Value::Null => Err(h.throw(ExcKind::NullPointer, "array receiver is Null".into())),
        other => Err(h.throw(ExcKind::ClassCast, format!("expected array, got {}", other.type_name()))),
    }
}

fn dict_arg<H: Host>(v: &Value, h: &mut H) -> Result<Rc<DictVal>, H::Err> {
    match v.deref() {
        Value::Dict(d) => Ok(d),
        Value::Null => Err(h.throw(ExcKind::NullPointer, "Dictionary receiver is Null".into())),
        other => Err(h.throw(ExcKind::ClassCast, format!("expected Dictionary, got {}", other.type_name()))),
    }
}

// ---------------------------------------------------------------------------------------------
// dispatch

/// Calls a non-mutating builtin. For methods the receiver is `args[0]`.
pub fn call<H: Host>(b: Builtin, args: Vec<Value>, h: &mut H) -> Result<Value, H::Err> {
    use Builtin::*;
    let a0 = || args.first().cloned().unwrap_or(Value::Void).deref();
    Ok(match b {
        ToString => Value::str(to_display(&a0(), h)?),
        Clone => deep_clone(&a0(), h)?,
        Equals => Value::Bool(values_equal(&a0(), &args[1].deref(), h)?),
        SameRef => {
            let (x, y) = (a0(), args[1].deref());
            Value::Bool(match (&x, &y) {
                (Value::Object(p), Value::Object(q)) => Rc::ptr_eq(p, q),
                (Value::Str(p), Value::Str(q)) => Rc::ptr_eq(p, q),
                (Value::Array(p), Value::Array(q)) => Rc::ptr_eq(p, q),
                (Value::Dict(p), Value::Dict(q)) => Rc::ptr_eq(p, q),
                (Value::Closure(p), Value::Closure(q)) => Rc::ptr_eq(p, q),
                (Value::Null, Value::Null) => true,
                _ => values_equal(&x, &y, h)?,
            })
        }
        Println => {
            let mut s = to_display(&a0(), h)?;
            s.push('\n');
            write_out(&s);
            Value::Void
        }
        Print => {
            let s = to_display(&a0(), h)?;
            write_out(&s);
            Value::Void
        }
        Read => {
            let p = if args.is_empty() { String::new() } else { to_display(&a0(), h)? };
            Value::str(read_line(&p))
        }
        ReplaceLine => {
            let s = to_display(&a0(), h)?;
            let k = arg_int(&args, 1, 0).max(0) + 1;
            write_out(&format!("\x1b[{k}F\x1b[2K{s}\x1b[{k}E"));
            Value::Void
        }

        // ---------------- String
        StrLength => Value::i64(str_arg(&args[0], h)?.chars().count() as i64),
        StrIsEmpty => Value::Bool(str_arg(&args[0], h)?.is_empty()),
        StrSubstring => {
            let s = str_arg(&args[0], h)?;
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len() as i128;
            let a = arg_int(&args, 1, 0);
            let e = arg_int(&args, 2, len);
            if a < 0 || e > len || a > e {
                return Err(h.throw(ExcKind::IndexOutOfBounds, format!("begin {}, end {}, length {}", a, e, len)));
            }
            Value::str(chars[a as usize..e as usize].iter().collect::<String>())
        }
        StrStartsWith => Value::Bool(str_arg(&args[0], h)?.starts_with(str_arg(&args[1], h)?.as_str())),
        StrEndsWith => Value::Bool(str_arg(&args[0], h)?.ends_with(str_arg(&args[1], h)?.as_str())),
        StrContains => Value::Bool(str_arg(&args[0], h)?.contains(str_arg(&args[1], h)?.as_str())),
        StrIndexOf => {
            let s = str_arg(&args[0], h)?;
            let pat = str_arg(&args[1], h)?;
            Value::i64(match s.find(pat.as_str()) {
                Some(bi) => s[..bi].chars().count() as i64,
                None => -1,
            })
        }
        StrToUpper => Value::str(str_arg(&args[0], h)?.to_uppercase()),
        StrToLower => Value::str(str_arg(&args[0], h)?.to_lowercase()),
        StrTrim => Value::str(str_arg(&args[0], h)?.trim()),
        StrReplace => {
            let s = str_arg(&args[0], h)?;
            let from = str_arg(&args[1], h)?;
            let to = str_arg(&args[2], h)?;
            let limit = arg_int(&args, 3, -1);
            let reverse = args.get(4).map(|v| v.deref().as_bool()).unwrap_or(false);
            if from.is_empty() {
                return Ok(Value::Str(s));
            }
            let mut idx: Vec<usize> = s.match_indices(from.as_str()).map(|(i, _)| i).collect();
            if reverse {
                // non-overlapping matches scanning from the right
                idx = s.rmatch_indices(from.as_str()).map(|(i, _)| i).collect();
            }
            if limit > 0 {
                idx.truncate(limit as usize);
            }
            idx.sort();
            let mut out = String::new();
            let mut last = 0;
            for i in idx {
                if i < last {
                    continue;
                }
                out.push_str(&s[last..i]);
                out.push_str(&to);
                last = i + from.len();
            }
            out.push_str(&s[last..]);
            Value::str(out)
        }
        StrSplit => {
            let s = str_arg(&args[0], h)?;
            let sep = str_arg(&args[1], h)?;
            let max = arg_int(&args, 2, -1);
            let parts: Vec<String> = if sep.is_empty() {
                s.chars().map(|c| c.to_string()).collect()
            } else if max > 0 {
                s.splitn(max as usize, sep.as_str()).map(|x| x.to_string()).collect()
            } else {
                s.split(sep.as_str()).map(|x| x.to_string()).collect()
            };
            Value::array(parts.into_iter().map(Value::str).collect(), false)
        }
        StrCharacters => {
            let s = str_arg(&args[0], h)?;
            Value::array(s.chars().map(|c| Value::str(c.to_string())).collect(), false)
        }
        StrFormat | StrFormatInj => {
            let s = str_arg(&args[0], h)?;
            Value::str(substitute_placeholders(&s, &args[1..], h)?)
        }
        StrParse => {
            let s = str_arg(&args[0], h)?;
            let ty = RtType::decode(&arg_str(&args, 1)).unwrap_or(RtType::Str);
            parse_as(&s, &ty, h)?
        }
        StrRandom => Value::str(random_string(&arg_str(&args, 0), arg_int(&args, 1, 0) as i64, arg_int(&args, 2, 16) as i64)),

        // ---------------- numbers
        NumFormat => Value::str(format_number(&a0(), arg_int(&args, 1, -1) as i64, arg_int(&args, 2, -1) as i64)),
        NumRandom => random_between(&args[0].deref(), &args[1].deref(), h)?,
        NumRange => {
            let start = args[0].deref();
            let end = args[1].deref();
            let step = args.get(2).map(|v| v.deref());
            let mut out = Vec::new();
            match (&start, &end) {
                (Value::Int(t, s), Value::Int(_, e)) => {
                    let st = step.map(|v| v.as_int()).unwrap_or(1);
                    if st == 0 {
                        return Err(h.throw(ExcKind::IllegalArgument, "range step must not be 0".into()));
                    }
                    let mut i = *s;
                    while (st > 0 && i < *e) || (st < 0 && i > *e) {
                        out.push(Value::Int(*t, i));
                        i += st;
                    }
                }
                (Value::Float(t, s), Value::Float(_, e)) => {
                    let st = step.map(|v| v.as_f64()).unwrap_or(1.0);
                    if st == 0.0 {
                        return Err(h.throw(ExcKind::IllegalArgument, "range step must not be 0".into()));
                    }
                    let mut k = 0f64;
                    loop {
                        let x = t.round(s + st * k);
                        if !((st > 0.0 && x < *e) || (st < 0.0 && x > *e)) {
                            break;
                        }
                        out.push(Value::Float(*t, x));
                        k += 1.0;
                    }
                }
                _ => return Err(h.throw(ExcKind::IllegalArgument, "range bounds must be numbers".into())),
            }
            Value::array(out, false)
        }
        NumMax | NumMin => {
            let mut best = a0();
            for v in &args[1..] {
                let v = v.deref();
                let o = compare_values(&v, &best, h)?;
                if (b == NumMax && o == std::cmp::Ordering::Greater) || (b == NumMin && o == std::cmp::Ordering::Less) {
                    best = v;
                }
            }
            best
        }

        // ---------------- arrays
        ArrNew => {
            // args: default value, [length, fixed]
            let def = args[0].clone();
            let len = arg_int(&args, 1, 0);
            if len < 0 {
                return Err(h.throw(ExcKind::IllegalArgument, format!("negative array length {}", len)));
            }
            let fixed = args.get(2).map(|v| v.deref().as_bool()).unwrap_or(false);
            Value::array(vec![def; len as usize], fixed)
        }
        ArrLength => Value::i64(arr_arg(&args[0], h)?.items.len() as i64),
        ArrIsEmpty => Value::Bool(arr_arg(&args[0], h)?.items.is_empty()),
        ArrAt => {
            let a = arr_arg(&args[0], h)?;
            let i = norm_index(arg_int(&args, 1, 0), a.items.len(), h)?;
            a.items[i].clone()
        }
        ArrFirst | ArrLast => {
            let a = arr_arg(&args[0], h)?;
            let off = arg_int(&args, 1, 0);
            if off < 0 {
                return Err(h.throw(ExcKind::IndexOutOfBounds, format!("negative offset {}", off)));
            }
            let idx = if b == ArrFirst { off } else { -1 - off };
            let i = norm_index(idx, a.items.len(), h)?;
            a.items[i].clone()
        }
        ArrKv => {
            let a = arr_arg(&args[0], h)?;
            Value::array(
                a.items.iter().enumerate().map(|(i, v)| Value::Tuple(Rc::new(vec![Value::i64(i as i64), v.clone()]))).collect(),
                false,
            )
        }
        ArrContains => {
            let a = arr_arg(&args[0], h)?;
            let x = args[1].deref();
            let mut found = false;
            for it in &a.items {
                if values_equal(it, &x, h)? {
                    found = true;
                    break;
                }
            }
            Value::Bool(found)
        }

        // ---------------- dictionaries
        DictGet => {
            let d = dict_arg(&args[0], h)?;
            let k = args[1].deref();
            match d.get(&k) {
                Some(v) => v.clone(),
                None => {
                    let ks = to_display_nested(&k, h)?;
                    return Err(h.throw(ExcKind::IllegalArgument, format!("key not found: {}", ks)));
                }
            }
        }
        DictKv => {
            let d = dict_arg(&args[0], h)?;
            Value::array(d.entries.iter().map(|(k, v)| Value::Tuple(Rc::new(vec![k.clone(), v.clone()]))).collect(), false)
        }
        DictLength => Value::i64(dict_arg(&args[0], h)?.entries.len() as i64),
        DictIsEmpty => Value::Bool(dict_arg(&args[0], h)?.entries.is_empty()),
        DictContainsKey => Value::Bool(dict_arg(&args[0], h)?.get(&args[1].deref()).is_some()),
        DictKeys => Value::array(dict_arg(&args[0], h)?.entries.iter().map(|(k, _)| k.clone()).collect(), false),
        DictValues => Value::array(dict_arg(&args[0], h)?.entries.iter().map(|(_, v)| v.clone()).collect(), false),

        // ---------------- rounding / formatting
        NumRound | NumCeil | NumFloor => {
            let mode = match b {
                NumCeil => RoundMode::Ceil,
                NumFloor => RoundMode::Floor,
                _ => RoundMode::Round,
            };
            // args: receiver, wrap, [digits...]
            let wrap = args[1].deref().as_bool();
            let digits: Vec<i128> = args[2..].iter().map(|v| v.deref().as_int()).collect();
            let spec = RoundSpec::from_args(&digits).map_err(|e| h.throw(ExcKind::IllegalArgument, e))?;
            numeric::round_value(&a0(), spec, mode, wrap, h)?
        }
        NumAbs => numeric::abs_value(&a0(), args[1].deref().as_bool(), h)?,
        FormatValue => Value::str(crate::format::format_value(&a0(), &arg_str(&args, 1), &arg_str(&args, 2), h)?),
        DefaultToString => match a0() {
            Value::Object(o) => Value::str(h.obj_default_string(&o)?),
            other => Value::str(to_display(&other, h)?),
        },

        // ---------------- bulk numeric kernels
        TensorZip => {
            // op, a, b, wrap, threads
            let op = ArithOp::from_code(arg_int(&args, 0, 0) as u8);
            numeric::zip(op, &args[1], &args[2], args[3].deref().as_bool(), arg_int(&args, 4, 1), h)?
        }
        TensorScalar => {
            // op, a, scalar, scalar_left, wrap, threads
            let op = ArithOp::from_code(arg_int(&args, 0, 0) as u8);
            numeric::scalar(op, &args[1], &args[2], args[3].deref().as_bool(), args[4].deref().as_bool(), arg_int(&args, 5, 1), h)?
        }
        TensorMatMul => {
            // a, b, n, k, m, wrap, threads
            let dim = |i: usize| arg_int(&args, i, 0).max(0) as usize;
            numeric::matmul(&args[0], &args[1], dim(2), dim(3), dim(4), args[5].deref().as_bool(), arg_int(&args, 6, 1), h)?
        }
        TensorRound => {
            // a, mode, ndigits, whole, decimal, wrap, threads
            let mode = RoundMode::from_code(arg_int(&args, 1, 0));
            let digits: Vec<i128> = match arg_int(&args, 2, 0) {
                0 => vec![],
                1 => vec![arg_int(&args, 4, 0)],
                _ => vec![arg_int(&args, 3, 0), arg_int(&args, 4, 0)],
            };
            let spec = RoundSpec::from_args(&digits).map_err(|e| h.throw(ExcKind::IllegalArgument, e))?;
            numeric::round_all(&args[0], spec, mode, args[5].deref().as_bool(), arg_int(&args, 6, 1), h)?
        }
        TensorMigrate => {
            // a, target type code, mode, threads
            let to = RtType::decode(&arg_str(&args, 1)).unwrap_or(RtType::Any);
            let ms = arg_str(&args, 2);
            let Some(mode) = RoundMode::parse(&ms) else {
                return Err(h.throw(ExcKind::IllegalArgument, format!("unknown migration method \"{}\" (expected \"round\", \"ceil\" or \"floor\")", ms)));
            };
            numeric::migrate_all(&args[0], &to, mode, arg_int(&args, 3, 1), h)?
        }
        TensorTranspose => {
            let dim = |i: usize| arg_int(&args, i, 0).max(0) as usize;
            numeric::transpose(&args[0], dim(1), dim(2), h)?
        }

        _ => {
            if b.is_mutating() {
                // Called on a temporary: mutate a copy and return the result.
                let mut it = args.into_iter();
                let mut recv = it.next().unwrap_or(Value::Void);
                return call_mut(b, &mut recv, it.collect(), h);
            }
            return Err(h.throw(ExcKind::UnsupportedOperation, format!("builtin {} not callable here", b.name())));
        }
    })
}

fn random_between<H: Host>(s: &Value, e: &Value, h: &mut H) -> Result<Value, H::Err> {
    match (s, e) {
        (Value::Int(t, a), Value::Int(_, b)) => {
            if b <= a {
                return Err(h.throw(ExcKind::IllegalArgument, format!("bound must be greater than origin ({} <= {})", b, a)));
            }
            Ok(Value::Int(*t, a + random_below((b - a) as u128) as i128))
        }
        (Value::Float(t, a), Value::Float(_, b)) => {
            if b <= a {
                return Err(h.throw(ExcKind::IllegalArgument, "bound must be greater than origin".into()));
            }
            Ok(Value::Float(*t, t.round(a + (b - a) * random_f64())))
        }
        _ => Err(h.throw(ExcKind::IllegalArgument, "random bounds must be numbers".into())),
    }
}

/// Calls a mutating builtin on the storage location `recv`.
pub fn call_mut<H: Host>(b: Builtin, recv: &mut Value, args: Vec<Value>, h: &mut H) -> Result<Value, H::Err> {
    use Builtin::*;
    if let Value::Ref(r) = recv {
        let r = r.clone();
        return r.with_mut(|v| call_mut(b, v, args, h));
    }
    if recv.is_null() {
        return Err(h.throw(ExcKind::NullPointer, format!("{} on Null", b.name())));
    }
    Ok(match b {
        StrAppend | StrPrepend => {
            let x = to_display(&args[0].deref(), h)?;
            if let Value::Str(s) = recv {
                let m = Rc::make_mut(s);
                if b == StrAppend {
                    m.push_str(&x);
                } else {
                    m.insert_str(0, &x);
                }
            }
            Value::Void
        }
        StrRandomize => {
            *recv = Value::str(random_string(&arg_str(&args, 0), arg_int(&args, 1, 0) as i64, arg_int(&args, 2, 16) as i64));
            Value::Void
        }
        NumRandomize => {
            *recv = random_between(&args[0].deref(), &args[1].deref(), h)?;
            Value::Void
        }
        ArrSet => {
            let Value::Array(a) = recv else { unreachable!() };
            let i = norm_index(arg_int(&args, 0, 0), a.items.len(), h)?;
            Rc::make_mut(a).items[i] = args[1].clone();
            Value::Void
        }
        ArrFill => {
            let Value::Array(a) = recv else { unreachable!() };
            if !args.is_empty() {
                let m = Rc::make_mut(a);
                for (i, slot) in m.items.iter_mut().enumerate() {
                    *slot = args[i % args.len()].clone();
                }
            }
            Value::Void
        }
        ArrReverse => {
            let Value::Array(a) = recv else { unreachable!() };
            Rc::make_mut(a).items.reverse();
            Value::Void
        }
        ArrSort => {
            let Value::Array(a) = recv else { unreachable!() };
            let items = a.items.clone();
            let sorted = merge_sort(items, h)?;
            Rc::make_mut(a).items = sorted;
            Value::Void
        }
        ArrShuffle => {
            let Value::Array(a) = recv else { unreachable!() };
            let m = Rc::make_mut(a);
            for i in (1..m.items.len()).rev() {
                let j = random_below(i as u128 + 1) as usize;
                m.items.swap(i, j);
            }
            Value::Void
        }
        ArrInsert => {
            let Value::Array(a) = recv else { unreachable!() };
            ensure_growable(a, "insert", h)?;
            let len = a.items.len() as i128;
            let i = arg_int(&args, 0, len);
            let j = if i < 0 { i + len + 1 } else { i };
            if j < 0 || j > len {
                return Err(h.throw(ExcKind::IndexOutOfBounds, format!("Index {} out of bounds for insertion into length {}", i, len)));
            }
            Rc::make_mut(a).items.insert(j as usize, args[1].clone());
            Value::Void
        }
        ArrRemove => {
            let Value::Array(a) = recv else { unreachable!() };
            ensure_growable(a, "remove", h)?;
            let i = norm_index(arg_int(&args, 0, 0), a.items.len(), h)?;
            Rc::make_mut(a).items.remove(i)
        }
        DictSet => {
            let Value::Dict(d) = recv else { unreachable!() };
            Rc::make_mut(d).insert(args[0].deref(), args[1].clone());
            Value::Void
        }
        DictMerge => {
            let other = dict_arg(&args[0], h)?;
            let Value::Dict(d) = recv else { unreachable!() };
            let m = Rc::make_mut(d);
            for (k, v) in &other.entries {
                m.insert(k.clone(), v.clone());
            }
            Value::Void
        }
        DictRemove => {
            let Value::Dict(d) = recv else { unreachable!() };
            match Rc::make_mut(d).remove(&args[0].deref()) {
                Some(v) => v,
                None => {
                    let ks = to_display_nested(&args[0], h)?;
                    return Err(h.throw(ExcKind::IllegalArgument, format!("key not found: {}", ks)));
                }
            }
        }
        _ => {
            let mut all = vec![recv.clone()];
            all.extend(args);
            return call(b, all, h);
        }
    })
}

/// Creates a reference to an element of the container stored at `parent` (nested element
/// places such as `grid[1][2] = 9`), validating the index / key.
pub fn elem_ref<H: Host>(parent: RefTarget, key: Value, h: &mut H) -> Result<RefTarget, H::Err> {
    match parent.get().deref() {
        Value::Array(a) => {
            let i = norm_index(key.deref().as_int(), a.items.len(), h)?;
            Ok(RefTarget::Elem(Rc::new((parent, Value::i64(i as i64)))))
        }
        Value::Dict(d) => {
            let k = key.deref();
            if d.get(&k).is_none() {
                let ks = to_display_nested(&k, h)?;
                return Err(h.throw(ExcKind::IllegalArgument, format!("key not found: {}", ks)));
            }
            Ok(RefTarget::Elem(Rc::new((parent, k))))
        }
        Value::Null => Err(h.throw(ExcKind::NullPointer, "indexing null".into())),
        other => Err(h.throw(ExcKind::ClassCast, format!("{} cannot be indexed", other.type_name()))),
    }
}

/// Marks an object (and the objects it owns) as freed (`free(x)` in manual mode).
pub fn free_value(v: &Value) {
    match v {
        Value::Object(o) => {
            if o.freed.replace(true) {
                return;
            }
            let fields: Vec<Value> = o.fields.borrow().clone();
            for f in fields.iter().rev() {
                free_value(f);
            }
        }
        Value::Array(a) => a.items.iter().for_each(free_value),
        Value::Dict(d) => d.entries.iter().for_each(|(_, v)| free_value(v)),
        Value::Tuple(t) => t.iter().for_each(free_value),
        _ => {}
    }
}
