//! Operators, conversions, equality, ordering and string conversion on runtime values.

use crate::bigint::BigInt;
use crate::value::*;
use std::cmp::Ordering;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl ArithOp {
    pub fn code(self) -> u8 {
        self as u8
    }
    pub fn from_code(c: u8) -> ArithOp {
        use ArithOp::*;
        [Add, Sub, Mul, Div, Rem, Pow, BitAnd, BitOr, BitXor, Shl, Shr][c as usize]
    }
    pub fn symbol(self) -> &'static str {
        use ArithOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Rem => "%",
            Pow => "**",
            BitAnd => "&&",
            BitOr => "||",
            BitXor => "^",
            Shl => "<<",
            Shr => ">>",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

impl CmpOp {
    pub fn code(self) -> u8 {
        self as u8
    }
    pub fn from_code(c: u8) -> CmpOp {
        use CmpOp::*;
        [Eq, Ne, Lt, Gt, Le, Ge][c as usize]
    }
    pub fn test(self, o: Ordering) -> bool {
        match self {
            CmpOp::Eq => o == Ordering::Equal,
            CmpOp::Ne => o != Ordering::Equal,
            CmpOp::Lt => o == Ordering::Less,
            CmpOp::Gt => o == Ordering::Greater,
            CmpOp::Le => o != Ordering::Greater,
            CmpOp::Ge => o != Ordering::Less,
        }
    }
}

fn overflow<H: Host>(h: &mut H, ty: IntTy, op: &str) -> H::Err {
    h.throw(ExcKind::Arithmetic, format!("integer overflow: {} {}", ty.name(), op))
}

fn fit<H: Host>(ty: IntTy, v: Option<i128>, wrap_v: i128, wrap: bool, op: &str, h: &mut H) -> Result<Value, H::Err> {
    if wrap {
        return Ok(Value::Int(ty, ty.wrap(wrap_v)));
    }
    match v {
        Some(v) if ty.fits(v) => Ok(Value::Int(ty, v)),
        _ => Err(overflow(h, ty, op)),
    }
}

/// Integer arithmetic on two values of the same integer type.
pub fn int_arith<H: Host>(op: ArithOp, ty: IntTy, a: i128, b: i128, wrap: bool, h: &mut H) -> Result<Value, H::Err> {
    use ArithOp::*;
    match op {
        Add => fit(ty, a.checked_add(b), a.wrapping_add(b), wrap, "+", h),
        Sub => fit(ty, a.checked_sub(b), a.wrapping_sub(b), wrap, "-", h),
        Mul => fit(ty, a.checked_mul(b), a.wrapping_mul(b), wrap, "*", h),
        Div => {
            if b == 0 {
                return Err(h.throw(ExcKind::Arithmetic, "/ by zero".into()));
            }
            fit(ty, Some(a / b), a / b, wrap, "/", h)
        }
        Rem => {
            if b == 0 {
                return Err(h.throw(ExcKind::Arithmetic, "% by zero".into()));
            }
            Ok(Value::Int(ty, a % b))
        }
        Pow => {
            if b < 0 {
                return Err(h.throw(ExcKind::Arithmetic, "negative exponent for integer power".into()));
            }
            let mut e = b as u128;
            if wrap {
                let mut base = ty.wrap(a);
                let mut acc: i128 = 1;
                while e > 0 {
                    if e & 1 == 1 {
                        acc = ty.wrap(acc.wrapping_mul(base));
                    }
                    e >>= 1;
                    if e > 0 {
                        base = ty.wrap(base.wrapping_mul(base));
                    }
                }
                return Ok(Value::Int(ty, ty.wrap(acc)));
            }
            let mut base = a;
            let mut acc: i128 = 1;
            let mut ok = true;
            while e > 0 {
                if e & 1 == 1 {
                    match acc.checked_mul(base) {
                        Some(v) if ty.fits(v) => acc = v,
                        _ => {
                            ok = false;
                            break;
                        }
                    }
                }
                e >>= 1;
                if e > 0 {
                    match base.checked_mul(base) {
                        Some(v) if v.unsigned_abs() <= (1u128 << 64) => base = v,
                        _ => {
                            // base too large: overflow unless no more factors are needed
                            if e > 0 {
                                ok = acc == 0;
                                break;
                            }
                        }
                    }
                }
            }
            if ok && ty.fits(acc) {
                Ok(Value::Int(ty, acc))
            } else {
                Err(overflow(h, ty, "**"))
            }
        }
        BitAnd => Ok(Value::Int(ty, ty.wrap(a & b))),
        BitOr => Ok(Value::Int(ty, ty.wrap(a | b))),
        BitXor => Ok(Value::Int(ty, ty.wrap(a ^ b))),
        Shl => {
            let amt = (b & (ty.bits() as i128 - 1)) as u32;
            Ok(Value::Int(ty, ty.wrap(a.wrapping_shl(amt))))
        }
        Shr => {
            let amt = (b & (ty.bits() as i128 - 1)) as u32;
            Ok(Value::Int(ty, ty.wrap(a >> amt)))
        }
    }
}

pub fn float_arith(op: ArithOp, ty: FloatTy, a: f64, b: f64) -> f64 {
    use ArithOp::*;
    match ty {
        FloatTy::F64 => match op {
            Add => a + b,
            Sub => a - b,
            Mul => a * b,
            Div => a / b,
            Rem => a % b,
            Pow => a.powf(b),
            _ => f64::NAN,
        },
        FloatTy::F32 | FloatTy::F16 => {
            let (x, y) = (a as f32, b as f32);
            let r = match op {
                Add => x + y,
                Sub => x - y,
                Mul => x * y,
                Div => x / y,
                Rem => x % y,
                Pow => x.powf(y),
                _ => f32::NAN,
            };
            if ty == FloatTy::F16 {
                round_f16(r) as f64
            } else {
                r as f64
            }
        }
    }
}

pub fn big_arith<H: Host>(op: ArithOp, a: &BigInt, b: &BigInt, h: &mut H) -> Result<Value, H::Err> {
    use ArithOp::*;
    let r = match op {
        Add => a.add(b),
        Sub => a.sub(b),
        Mul => a.mul(b),
        Div => match a.divmod(b) {
            Some((q, _)) => q,
            None => return Err(h.throw(ExcKind::Arithmetic, "/ by zero".into())),
        },
        Rem => match a.divmod(b) {
            Some((_, r)) => r,
            None => return Err(h.throw(ExcKind::Arithmetic, "% by zero".into())),
        },
        Pow => {
            if b.is_negative() {
                return Err(h.throw(ExcKind::Arithmetic, "negative exponent for integer power".into()));
            }
            let e = b.to_i128().unwrap_or(i128::MAX);
            if e > u32::MAX as i128 {
                return Err(h.throw(ExcKind::Arithmetic, "exponent too large".into()));
            }
            a.pow(e as u64)
        }
        Shl | Shr => {
            let amt = b.to_i128().unwrap_or(0).clamp(0, 1 << 20) as u64;
            let p = BigInt::from_i128(2).pow(amt);
            if op == Shl {
                a.mul(&p)
            } else {
                // floor division for arithmetic shift
                let (q, r) = a.divmod(&p).unwrap();
                if a.is_negative() && !r.is_zero() {
                    q.sub(&BigInt::from_i128(1))
                } else {
                    q
                }
            }
        }
        BitAnd | BitOr | BitXor => match (a.to_i128(), b.to_i128()) {
            (Some(x), Some(y)) => BigInt::from_i128(match op {
                BitAnd => x & y,
                BitOr => x | y,
                _ => x ^ y,
            }),
            _ => return Err(h.throw(ExcKind::UnsupportedOperation, "bitwise operation on huge IntLarge".into())),
        },
    };
    Ok(Value::Big(Rc::new(r)))
}

/// Binary arithmetic on two runtime values (already converted to a common type by the checker,
/// except for shift amounts and powers).
pub fn arith<H: Host>(op: ArithOp, a: &Value, b: &Value, wrap: bool, h: &mut H) -> Result<Value, H::Err> {
    match (a, b) {
        (Value::Int(t, x), Value::Int(_, y)) => int_arith(op, *t, *x, *y, wrap, h),
        (Value::Float(t, x), Value::Float(_, y)) => Ok(Value::Float(*t, float_arith(op, *t, *x, *y))),
        (Value::Float(t, x), Value::Int(_, y)) => Ok(Value::Float(*t, float_arith(op, *t, *x, *y as f64))),
        (Value::Big(x), Value::Big(y)) => big_arith(op, x, y, h),
        (Value::Big(x), Value::Int(_, y)) => big_arith(op, x, &BigInt::from_i128(*y), h),
        (Value::Int(_, x), Value::Big(y)) => big_arith(op, &BigInt::from_i128(*x), y, h),
        (Value::Null, _) | (_, Value::Null) => Err(h.throw(ExcKind::NullPointer, "arithmetic on Null".into())),
        _ => Err(h.throw(ExcKind::UnsupportedOperation, format!("operator {} on {} and {}", op.symbol(), a.type_name(), b.type_name()))),
    }
}

pub fn negate<H: Host>(v: &Value, wrap: bool, h: &mut H) -> Result<Value, H::Err> {
    match v {
        Value::Int(t, x) => fit(*t, Some(-*x), -*x, wrap, "unary -", h),
        Value::Float(t, x) => Ok(Value::Float(*t, -*x)),
        Value::Big(b) => Ok(Value::Big(Rc::new(b.neg()))),
        _ => Err(h.throw(ExcKind::UnsupportedOperation, format!("unary - on {}", v.type_name()))),
    }
}

pub fn bit_not(v: &Value) -> Value {
    match v {
        Value::Int(t, x) => Value::Int(*t, t.wrap(!*x)),
        Value::Big(b) => Value::Big(Rc::new(b.neg().sub(&BigInt::from_i128(1)))),
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------------------------
// String conversion

pub fn fmt_float(ty: FloatTy, v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let a = v.abs();
    let plain = (1e-3..1e7).contains(&a);
    let s = match (ty, plain) {
        (FloatTy::F64, true) => format!("{}", v),
        (_, true) => format!("{}", v as f32),
        (FloatTy::F64, false) => format!("{:e}", v),
        (_, false) => format!("{:e}", v as f32),
    };
    if plain {
        if s.contains('.') {
            s
        } else {
            format!("{}.0", s)
        }
    } else {
        // Rust: 1.5e10 / 1e-5  ->  Java: 1.5E10 / 1.0E-5
        let (m, e) = s.split_once('e').unwrap();
        let m = if m.contains('.') { m.to_string() } else { format!("{}.0", m) };
        format!("{}E{}", m, e)
    }
}

fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn to_string_inner<H: Host>(v: &Value, nested: bool, h: &mut H) -> Result<String, H::Err> {
    Ok(match v {
        Value::Void => "void".into(),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(_, i) => i.to_string(),
        Value::Big(b) => b.to_string(),
        Value::Float(t, f) => fmt_float(*t, *f),
        Value::Str(s) => {
            if nested {
                quote(s)
            } else {
                s.to_string()
            }
        }
        Value::Array(a) => {
            let mut parts = Vec::with_capacity(a.items.len());
            for it in &a.items {
                parts.push(to_string_inner(it, true, h)?);
            }
            format!("[{}]", parts.join(", "))
        }
        Value::Dict(d) => {
            let mut parts = Vec::with_capacity(d.entries.len());
            for (k, val) in &d.entries {
                parts.push(format!("{}: {}", to_string_inner(k, true, h)?, to_string_inner(val, true, h)?));
            }
            format!("{{{}}}", parts.join(", "))
        }
        Value::Tuple(t) => {
            let mut parts = Vec::new();
            for it in t.iter() {
                parts.push(to_string_inner(it, true, h)?);
            }
            format!("({})", parts.join(", "))
        }
        Value::Object(o) => h.obj_to_string(o)?,
        Value::Closure(_) => "<function>".into(),
        Value::Ref(r) => to_string_inner(&r.get(), nested, h)?,
        Value::Raw(p) => format!("<raw {:#x}>", p),
    })
}

/// Converts a value to its display string (`println`, string concatenation, f-strings).
pub fn to_display<H: Host>(v: &Value, h: &mut H) -> Result<String, H::Err> {
    to_string_inner(v, false, h)
}

/// Display form for values nested inside containers / default `toString` of objects.
pub fn to_display_nested<H: Host>(v: &Value, h: &mut H) -> Result<String, H::Err> {
    to_string_inner(v, true, h)
}

// ---------------------------------------------------------------------------------------------
// Equality / ordering

fn num_cmp(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Int(_, x), Value::Int(_, y)) => Some(x.cmp(y)),
        (Value::Big(x), Value::Big(y)) => Some(x.cmp(y)),
        (Value::Big(x), Value::Int(_, y)) => Some(x.cmp(&BigInt::from_i128(*y))),
        (Value::Int(_, x), Value::Big(y)) => Some(BigInt::from_i128(*x).cmp(y)),
        (Value::Float(_, _), _) | (_, Value::Float(_, _)) => {
            let (x, y) = (num_f64(a)?, num_f64(b)?);
            x.partial_cmp(&y)
        }
        _ => None,
    }
}

fn num_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(_, x) => Some(*x as f64),
        Value::Float(_, f) => Some(*f),
        Value::Big(b) => Some(b.to_f64()),
        _ => None,
    }
}

pub fn values_equal<H: Host>(a: &Value, b: &Value, h: &mut H) -> Result<bool, H::Err> {
    Ok(match (a, b) {
        (Value::Ref(r), _) => return values_equal(&r.get(), b, h),
        (_, Value::Ref(r)) => return values_equal(a, &r.get(), h),
        (Value::Null, Value::Null) | (Value::Void, Value::Void) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            if Rc::ptr_eq(x, y) {
                return Ok(true);
            }
            if x.items.len() != y.items.len() {
                return Ok(false);
            }
            for (p, q) in x.items.iter().zip(y.items.iter()) {
                if !values_equal(p, q, h)? {
                    return Ok(false);
                }
            }
            true
        }
        (Value::Dict(x), Value::Dict(y)) => {
            if x.entries.len() != y.entries.len() {
                return Ok(false);
            }
            for (k, v) in &x.entries {
                match y.get(k) {
                    Some(v2) => {
                        if !values_equal(v, v2, h)? {
                            return Ok(false);
                        }
                    }
                    None => return Ok(false),
                }
            }
            true
        }
        (Value::Tuple(x), Value::Tuple(y)) => {
            if x.len() != y.len() {
                return Ok(false);
            }
            for (p, q) in x.iter().zip(y.iter()) {
                if !values_equal(p, q, h)? {
                    return Ok(false);
                }
            }
            true
        }
        (Value::Object(x), Value::Object(y)) => {
            if Rc::ptr_eq(x, y) {
                return Ok(true);
            }
            h.obj_equals(x, y)?
        }
        (Value::Closure(x), Value::Closure(y)) => Rc::ptr_eq(x, y),
        _ => match num_cmp(a, b) {
            Some(o) => o == Ordering::Equal,
            None => false,
        },
    })
}

/// Default structural equality of two objects of the same class (field-by-field `==`).
pub fn fields_equal<H: Host>(a: &Rc<Object>, b: &Rc<Object>, h: &mut H) -> Result<bool, H::Err> {
    if a.class != b.class {
        return Ok(false);
    }
    let fa: Vec<Value> = a.fields.borrow().clone();
    let fb: Vec<Value> = b.fields.borrow().clone();
    for (x, y) in fa.iter().zip(fb.iter()) {
        if !values_equal(x, y, h)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn compare_values<H: Host>(a: &Value, b: &Value, h: &mut H) -> Result<Ordering, H::Err> {
    if let Some(o) = num_cmp(a, b) {
        return Ok(o);
    }
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => Ok(x.as_str().cmp(y.as_str())),
        (Value::Bool(x), Value::Bool(y)) => Ok(x.cmp(y)),
        (Value::Object(x), Value::Object(y)) => Ok(h.obj_compare(x, y)?.cmp(&0)),
        (Value::Float(_, _), Value::Float(_, _)) => Ok(Ordering::Equal), // NaN
        (Value::Null, _) | (_, Value::Null) => Err(h.throw(ExcKind::NullPointer, "comparison with Null".into())),
        _ => Err(h.throw(ExcKind::ClassCast, format!("{} is not comparable with {}", a.type_name(), b.type_name()))),
    }
}

/// `<`, `>`, `<=`, `>=`, `==`, `!=` on runtime values.
pub fn compare<H: Host>(op: CmpOp, a: &Value, b: &Value, h: &mut H) -> Result<bool, H::Err> {
    match op {
        CmpOp::Eq => values_equal(a, b, h),
        CmpOp::Ne => Ok(!values_equal(a, b, h)?),
        _ => {
            // NaN comparisons are always false, like Java.
            if let (Some(x), Some(y)) = (num_f64(a), num_f64(b)) {
                if x.is_nan() || y.is_nan() {
                    return Ok(false);
                }
            }
            Ok(op.test(compare_values(a, b, h)?))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Cloning

fn contains_objects(v: &Value) -> bool {
    match v {
        Value::Object(_) | Value::Closure(_) => true,
        Value::Array(a) => a.items.iter().any(contains_objects),
        Value::Dict(d) => d.entries.iter().any(|(k, v)| contains_objects(k) || contains_objects(v)),
        Value::Tuple(t) => t.iter().any(contains_objects),
        _ => false,
    }
}

/// `.clone()`: strings, arrays and dictionaries of plain data are shared copy-on-write;
/// containers holding objects and objects themselves are copied deeply.
pub fn deep_clone<H: Host>(v: &Value, h: &mut H) -> Result<Value, H::Err> {
    Ok(match v {
        Value::Array(a) => {
            if contains_objects(v) {
                let mut items = Vec::with_capacity(a.items.len());
                for it in &a.items {
                    items.push(deep_clone(it, h)?);
                }
                Value::Array(Rc::new(ArrayVal { items, fixed: a.fixed }))
            } else {
                Value::Array(a.clone())
            }
        }
        Value::Dict(d) => {
            if contains_objects(v) {
                let mut nd = DictVal::default();
                for (k, val) in &d.entries {
                    nd.insert(deep_clone(k, h)?, deep_clone(val, h)?);
                }
                Value::Dict(Rc::new(nd))
            } else {
                Value::Dict(d.clone())
            }
        }
        Value::Tuple(t) => {
            let mut items = Vec::new();
            for it in t.iter() {
                items.push(deep_clone(it, h)?);
            }
            Value::Tuple(Rc::new(items))
        }
        Value::Object(o) => {
            if o.freed.get() {
                return Err(h.throw(ExcKind::UseAfterFree, "use of freed object".into()));
            }
            let fields: Vec<Value> = o.fields.borrow().clone();
            let mut nf = Vec::with_capacity(fields.len());
            for f in &fields {
                nf.push(deep_clone(f, h)?);
            }
            Value::Object(Rc::new(Object { class: o.class, fields: std::cell::RefCell::new(nf), freed: std::cell::Cell::new(false) }))
        }
        Value::Ref(r) => deep_clone(&r.get(), h)?,
        other => other.clone(),
    })
}

// ---------------------------------------------------------------------------------------------
// Conversions

/// Implicit (lossless) widening conversion. Never fails.
pub fn widen(v: Value, to: &RtType) -> Value {
    match (&v, to) {
        (Value::Int(_, x), RtType::Int(t)) => Value::Int(*t, *x),
        (Value::Int(_, x), RtType::Float(t)) => Value::Float(*t, t.round(*x as f64)),
        (Value::Int(_, x), RtType::Big) => Value::Big(Rc::new(BigInt::from_i128(*x))),
        (Value::Float(_, x), RtType::Float(t)) => Value::Float(*t, *x),
        (_, RtType::Nullable(inner)) => {
            if v.is_null() {
                v
            } else {
                widen(v, inner)
            }
        }
        _ => v,
    }
}

pub fn conforms<H: Host>(v: &Value, ty: &RtType, h: &H) -> bool {
    match (v, ty) {
        (_, RtType::Any) => true,
        (Value::Null, RtType::Nullable(_)) => true,
        (_, RtType::Nullable(t)) => conforms(v, t, h),
        (_, RtType::Union(ts)) => ts.iter().any(|t| conforms(v, t, h)),
        (Value::Int(a, _), RtType::Int(b)) => a == b,
        (Value::Big(_), RtType::Big) => true,
        (Value::Float(a, _), RtType::Float(b)) => a == b,
        (Value::Bool(_), RtType::Bool) => true,
        (Value::Str(_), RtType::Str) => true,
        (Value::Array(a), RtType::Array(e)) => a.items.iter().all(|x| conforms(x, e, h)),
        (Value::Dict(d), RtType::Dict(k, val)) => d.entries.iter().all(|(x, y)| conforms(x, k, h) && conforms(y, val, h)),
        (Value::Object(o), RtType::Class(c)) => h.is_subclass(o.class, *c),
        (Value::Object(o), RtType::Iface(i)) => h.implements(o.class, *i),
        (Value::Tuple(t), RtType::Tuple(ts)) => t.len() == ts.len() && t.iter().zip(ts.iter()).all(|(x, y)| conforms(x, y, h)),
        (Value::Closure(_), RtType::Func) => true,
        (Value::Void, RtType::Void) => true,
        _ => false,
    }
}

fn type_display<H: Host>(t: &RtType, h: &H) -> String {
    match t {
        RtType::Int(i) => i.name().into(),
        RtType::Big => "IntLarge".into(),
        RtType::Float(f) => f.name().into(),
        RtType::Bool => "Boolean".into(),
        RtType::Str => "String".into(),
        RtType::Array(e) => format!("{}[]", type_display(e, h)),
        RtType::Dict(k, v) => format!("Dictionary[{}, {}]", type_display(k, h), type_display(v, h)),
        RtType::Class(c) | RtType::Iface(c) => h.class_name(*c),
        RtType::Nullable(t) => format!("{}?", type_display(t, h)),
        RtType::Union(ts) => ts.iter().map(|t| type_display(t, h)).collect::<Vec<_>>().join("|"),
        RtType::Tuple(ts) => format!("({})", ts.iter().map(|t| type_display(t, h)).collect::<Vec<_>>().join(", ")),
        RtType::Func => "Function".into(),
        RtType::Any => "DTVariable".into(),
        RtType::Void => "void".into(),
    }
}

fn value_type_display<H: Host>(v: &Value, h: &H) -> String {
    match v {
        Value::Object(o) => h.class_name(o.class),
        other => other.type_name(),
    }
}

fn num_to_int<H: Host>(v: &Value, t: IntTy, wrap: bool, h: &mut H) -> Result<Value, H::Err> {
    let x: i128 = match v {
        Value::Int(_, x) => *x,
        Value::Big(b) => match b.to_i128() {
            Some(x) => x,
            None => {
                if wrap {
                    // wrap using the low bits: compute b mod 2^128 is overkill; use string fallback
                    let m = b.divmod(&BigInt::from_i128(1i128 << 64)).map(|(_, r)| r).unwrap();
                    m.to_i128().unwrap_or(0)
                } else {
                    return Err(overflow(h, t, "castTo"));
                }
            }
        },
        Value::Float(_, f) => {
            if f.is_nan() || f.is_infinite() || *f >= 1.7e38 || *f <= -1.7e38 {
                if wrap {
                    if f.is_nan() {
                        0
                    } else {
                        *f as i128
                    }
                } else {
                    return Err(overflow(h, t, "castTo"));
                }
            } else {
                f.trunc() as i128
            }
        }
        _ => unreachable!(),
    };
    if t.fits(x) {
        Ok(Value::Int(t, x))
    } else if wrap {
        Ok(Value::Int(t, t.wrap(x)))
    } else {
        Err(overflow(h, t, "castTo"))
    }
}

/// Explicit conversion `.castTo(T)`.
pub fn cast<H: Host>(v: &Value, to: &RtType, wrap: bool, h: &mut H) -> Result<Value, H::Err> {
    let v = v.deref();
    let is_num = |v: &Value| matches!(v, Value::Int(..) | Value::Float(..) | Value::Big(_));
    match to {
        RtType::Any => return Ok(v),
        RtType::Nullable(inner) => {
            if v.is_null() {
                return Ok(v);
            }
            return cast(&v, inner, wrap, h);
        }
        RtType::Union(ts) => {
            if ts.iter().any(|t| conforms(&v, t, h)) {
                return Ok(v);
            }
            for t in ts {
                if let Ok(r) = cast(&v, t, wrap, h) {
                    return Ok(r);
                }
            }
        }
        RtType::Int(t) if is_num(&v) => return num_to_int(&v, *t, wrap, h),
        RtType::Float(t) if is_num(&v) => return Ok(Value::Float(*t, t.round(num_f64(&v).unwrap()))),
        RtType::Big if is_num(&v) => {
            return Ok(match &v {
                Value::Int(_, x) => Value::Big(Rc::new(BigInt::from_i128(*x))),
                Value::Float(_, f) => Value::Big(Rc::new(BigInt::parse(&format!("{:.0}", f.trunc())).unwrap_or_else(BigInt::zero))),
                _ => v.clone(),
            })
        }
        _ => {
            if conforms(&v, to, h) {
                return Ok(v);
            }
            // element-wise numeric conversion for containers
            if let (Value::Array(a), RtType::Array(e)) = (&v, to) {
                let mut items = Vec::with_capacity(a.items.len());
                let mut ok = true;
                for it in &a.items {
                    match cast(it, e, wrap, h) {
                        Ok(x) => items.push(x),
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    return Ok(Value::Array(Rc::new(ArrayVal { items, fixed: a.fixed })));
                }
            }
        }
    }
    if v.is_null() {
        return Err(h.throw(ExcKind::NullPointer, format!("cannot cast Null to {}", type_display(to, h))));
    }
    let msg = format!("{} cannot be cast to {}", value_type_display(&v, h), type_display(to, h));
    Err(h.throw(ExcKind::ClassCast, msg))
}

pub fn default_value(t: &RtType) -> Option<Value> {
    Some(match t {
        RtType::Int(i) => Value::Int(*i, 0),
        RtType::Big => Value::Big(Rc::new(BigInt::zero())),
        RtType::Float(f) => Value::Float(*f, 0.0),
        RtType::Bool => Value::Bool(false),
        RtType::Str => Value::str(""),
        RtType::Array(_) => Value::array(Vec::new(), false),
        RtType::Dict(_, _) => Value::Dict(Rc::new(DictVal::default())),
        RtType::Nullable(_) | RtType::Any => Value::Null,
        RtType::Tuple(ts) => {
            let mut items = Vec::new();
            for t in ts {
                items.push(default_value(t)?);
            }
            Value::Tuple(Rc::new(items))
        }
        _ => return None,
    })
}

/// Number formatting `.format(whole, decimal)`: `whole` is the minimum number of integer digits
/// (zero padded), `decimal` the number of fraction digits; `-1` means unrestricted.
pub fn format_number(v: &Value, whole: i64, decimal: i64) -> String {
    let (neg, int_part, frac_part) = match v {
        Value::Int(_, x) => {
            let frac = if decimal > 0 { "0".repeat(decimal as usize) } else { String::new() };
            (*x < 0, x.unsigned_abs().to_string(), frac)
        }
        Value::Big(b) => {
            let s = b.to_string();
            let frac = if decimal > 0 { "0".repeat(decimal as usize) } else { String::new() };
            (b.is_negative(), s.trim_start_matches('-').to_string(), frac)
        }
        Value::Float(t, f) => {
            let neg = f.is_sign_negative() && *f != 0.0;
            let a = f.abs();
            let s = if decimal >= 0 {
                format!("{:.*}", decimal as usize, a)
            } else if *t == FloatTy::F64 {
                format!("{}", a)
            } else {
                format!("{}", a as f32)
            };
            let (i, fr) = match s.split_once('.') {
                Some((i, fr)) => (i.to_string(), fr.to_string()),
                None => (s.clone(), String::new()),
            };
            (neg, i, fr)
        }
        other => return format!("{:?}", other),
    };
    let mut ip = int_part;
    if whole > 0 && (ip.len() as i64) < whole {
        ip = format!("{}{}", "0".repeat(whole as usize - ip.len()), ip);
    }
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    s.push_str(&ip);
    if !frac_part.is_empty() {
        s.push('.');
        s.push_str(&frac_part);
    }
    s
}
