//! Decimal rounding (`round` / `ceil` / `floor`), element type migration and the bulk numeric
//! kernels behind `math.linear.Tensor`. The kernels work on plain numbers extracted from the
//! array values, so they can optionally run on several worker threads; results (and the first
//! error, in element order) are identical to the sequential computation.

use crate::bigint::BigInt;
use crate::ops::{self, ArithOp};
use crate::value::*;
use std::ops::Range;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoundMode {
    Round,
    Ceil,
    Floor,
}

impl RoundMode {
    pub fn code(self) -> i64 {
        self as i64
    }
    pub fn from_code(c: i128) -> RoundMode {
        match c {
            1 => RoundMode::Ceil,
            2 => RoundMode::Floor,
            _ => RoundMode::Round,
        }
    }
    pub fn parse(s: &str) -> Option<RoundMode> {
        match s.to_ascii_lowercase().as_str() {
            "round" => Some(RoundMode::Round),
            "ceil" => Some(RoundMode::Ceil),
            "floor" => Some(RoundMode::Floor),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            RoundMode::Round => "round",
            RoundMode::Ceil => "ceil",
            RoundMode::Floor => "floor",
        }
    }
}

/// Where a rounding call puts its digits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoundSpec {
    /// Round to a multiple of `10^p` (`0`: whole number, `-2`: hundredths, `2`: hundreds).
    Places(i32),
    /// Round the integer part to `10^w` and the fraction to `10^-d`, independently
    /// (`123.456` with `(2, 1)` gives `100.5`).
    Split(i32, i32),
}

impl RoundSpec {
    /// The meaning of `x.round()`, `x.round(d)` and `x.round(w, d)`.
    pub fn from_args(args: &[i128]) -> Result<RoundSpec, String> {
        let clamp = |v: i128| v.clamp(-400, 400) as i32;
        match args {
            [] => Ok(RoundSpec::Places(0)),
            [d] => Ok(RoundSpec::Places(-clamp(*d))),
            [w, d] => {
                if *w < 0 || *d < 0 {
                    return Err(format!("round/ceil/floor(whole, decimal) needs non-negative digit counts, got ({}, {})", w, d));
                }
                Ok(RoundSpec::Split(clamp(*w), clamp(*d)))
            }
            _ => Err("round/ceil/floor take at most 2 arguments".into()),
        }
    }
}

fn pow10(k: u32) -> Option<u128> {
    10u128.checked_pow(k)
}

/// Rounds the magnitude `m` to a multiple of `10^k`; returns the number of `10^k` units.
fn round_units(m: u128, k: u32, neg: bool, mode: RoundMode) -> u128 {
    if k == 0 {
        return m;
    }
    let (q, r, unit) = match pow10(k) {
        Some(u) => (m / u, m % u, Some(u)),
        None => (0, m, None),
    };
    let up = match mode {
        // half away from zero
        RoundMode::Round => match unit {
            Some(u) => r >= u - r,
            None => false,
        },
        RoundMode::Ceil => !neg && r > 0,
        RoundMode::Floor => neg && r > 0,
    };
    if up {
        q + 1
    } else {
        q
    }
}

/// A finite decimal `±m × 10^e`.
#[derive(Clone, Copy, Debug)]
struct Dec {
    neg: bool,
    m: u128,
    e: i32,
}

impl Dec {
    /// The shortest decimal that reads back as `v` at precision `ty`.
    fn from_float(v: f64, ty: FloatTy) -> Dec {
        let s = match ty {
            FloatTy::F64 => format!("{:e}", v.abs()),
            _ => format!("{:e}", (v as f32).abs()),
        };
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        let (int_digits, frac_digits) = mant.split_once('.').unwrap_or((mant, ""));
        let digits = format!("{}{}", int_digits, frac_digits);
        let m: u128 = digits.parse().unwrap_or(0);
        Dec { neg: v.is_sign_negative(), m, e: exp - frac_digits.len() as i32 }
    }

    fn to_f64(self) -> f64 {
        if self.m == 0 {
            return 0.0;
        }
        let v: f64 = format!("{}e{}", self.m, self.e).parse().unwrap_or(0.0);
        if self.neg {
            -v
        } else {
            v
        }
    }

    fn round_at(self, p: i32, mode: RoundMode) -> Dec {
        if self.e >= p {
            return self;
        }
        let k = (p as i64 - self.e as i64).min(1000) as u32;
        Dec { neg: self.neg, m: round_units(self.m, k, self.neg, mode), e: p }
    }
}

/// Rounds a float value (shortest decimal representation, so `2.675.round(2)` is `2.68`).
pub fn round_float(v: f64, ty: FloatTy, spec: RoundSpec, mode: RoundMode) -> f64 {
    if !v.is_finite() || v == 0.0 {
        return v;
    }
    let d = Dec::from_float(v, ty);
    let r = match spec {
        RoundSpec::Places(p) => d.round_at(p, mode).to_f64(),
        RoundSpec::Split(w, dd) => {
            if d.e >= 0 {
                d.round_at(w, mode).to_f64()
            } else {
                let k = (-d.e) as u32;
                let (ip, fp) = match pow10(k) {
                    Some(u) => (d.m / u, d.m % u),
                    None => (0, d.m),
                };
                let i = Dec { neg: d.neg, m: ip, e: 0 }.round_at(w, mode);
                let f = Dec { neg: d.neg, m: fp, e: d.e }.round_at(-dd, mode);
                // i is at exponent >= 0, f at exponent <= 0: add exactly when it fits
                let shift = (i.e - f.e) as u32;
                match pow10(shift).and_then(|s| i.m.checked_mul(s)).and_then(|x| x.checked_add(f.m)) {
                    Some(m) => Dec { neg: d.neg, m, e: f.e }.to_f64(),
                    None => i.to_f64() + f.to_f64(),
                }
            }
        }
    };
    let r = if r == 0.0 { 0.0 } else { r };
    ty.round(r)
}

/// Rounds an integer; only places above the units digit change it.
pub fn round_int(v: i128, spec: RoundSpec, mode: RoundMode) -> Option<i128> {
    let p = match spec {
        RoundSpec::Places(p) => p,
        RoundSpec::Split(w, _) => w,
    };
    if p <= 0 || v == 0 {
        return Some(v);
    }
    let q = round_units(v.unsigned_abs(), p as u32, v < 0, mode);
    let mag = pow10(p as u32).and_then(|u| q.checked_mul(u))?;
    let mag = i128::try_from(mag).ok()?;
    Some(if v < 0 { -mag } else { mag })
}

pub fn round_big(v: &BigInt, spec: RoundSpec, mode: RoundMode) -> BigInt {
    let p = match spec {
        RoundSpec::Places(p) => p,
        RoundSpec::Split(w, _) => w,
    };
    if p <= 0 || v.is_zero() {
        return v.clone();
    }
    let unit = BigInt::from_i128(10).pow(p as u64);
    let neg = v.is_negative();
    let mag = if neg { v.neg() } else { v.clone() };
    let (q, r) = mag.divmod(&unit).unwrap();
    let up = match mode {
        RoundMode::Round => r.add(&r).cmp(&unit) != std::cmp::Ordering::Less,
        RoundMode::Ceil => !neg && !r.is_zero(),
        RoundMode::Floor => neg && !r.is_zero(),
    };
    let q = if up { q.add(&BigInt::from_i128(1)) } else { q };
    let res = q.mul(&unit);
    if neg {
        res.neg()
    } else {
        res
    }
}

/// `x.round(...)` / `x.ceil(...)` / `x.floor(...)` on a number.
pub fn round_value<H: Host>(v: &Value, spec: RoundSpec, mode: RoundMode, wrap: bool, h: &mut H) -> Result<Value, H::Err> {
    Ok(match v {
        Value::Float(t, f) => Value::Float(*t, round_float(*f, *t, spec, mode)),
        Value::Int(t, x) => match round_int(*x, spec, mode) {
            Some(r) if t.fits(r) => Value::Int(*t, r),
            Some(r) if wrap => Value::Int(*t, t.wrap(r)),
            _ => return Err(h.throw(ExcKind::Arithmetic, format!("integer overflow: {} {}", t.name(), mode.name()))),
        },
        Value::Big(b) => Value::Big(Rc::new(round_big(b, spec, mode))),
        Value::Null => return Err(h.throw(ExcKind::NullPointer, format!("{} on Null", mode.name()))),
        other => other.clone(),
    })
}

pub fn abs_value<H: Host>(v: &Value, wrap: bool, h: &mut H) -> Result<Value, H::Err> {
    Ok(match v {
        Value::Float(t, f) => Value::Float(*t, f.abs()),
        Value::Int(t, x) => {
            let a = x.abs();
            if t.fits(a) {
                Value::Int(*t, a)
            } else if wrap {
                Value::Int(*t, t.wrap(a))
            } else {
                return Err(h.throw(ExcKind::Arithmetic, format!("integer overflow: {} abs", t.name())));
            }
        }
        Value::Big(b) => Value::Big(Rc::new(if b.is_negative() { b.neg() } else { (**b).clone() })),
        Value::Null => return Err(h.throw(ExcKind::NullPointer, "abs on Null".into())),
        other => other.clone(),
    })
}

// ---------------------------------------------------------------------------------------------
// migration between element types

/// The next representable value of `ty` above (`up`) or below `x`.
pub fn next_float(x: f64, ty: FloatTy, up: bool) -> f64 {
    if x.is_nan() || (x.is_infinite() && (x > 0.0) == up) {
        return x;
    }
    match ty {
        FloatTy::F64 => {
            if x == 0.0 {
                let tiny = f64::from_bits(1);
                return if up { tiny } else { -tiny };
            }
            let b = x.to_bits();
            f64::from_bits(if (x > 0.0) == up { b + 1 } else { b - 1 })
        }
        FloatTy::F32 => {
            let y = x as f32;
            if y == 0.0 {
                let tiny = f32::from_bits(1);
                return (if up { tiny } else { -tiny }) as f64;
            }
            let b = y.to_bits();
            f32::from_bits(if (y > 0.0) == up { b + 1 } else { b - 1 }) as f64
        }
        FloatTy::F16 => {
            let h = f32_to_f16_bits(x as f32);
            let y = f16_bits_to_f32(h);
            if y == 0.0 {
                let tiny = f16_bits_to_f32(1);
                return (if up { tiny } else { -tiny }) as f64;
            }
            f16_bits_to_f32(if (y > 0.0) == up { h + 1 } else { h - 1 }) as f64
        }
    }
}

/// Directed rounding of `x` to precision `ty`.
fn float_to(x: f64, ty: FloatTy, mode: RoundMode) -> f64 {
    let r = ty.round(x);
    match mode {
        RoundMode::Ceil if r < x => next_float(r, ty, true),
        RoundMode::Floor if r > x => next_float(r, ty, false),
        _ => r,
    }
}

fn migrate_err<H: Host>(to: &str, what: &str, h: &mut H) -> H::Err {
    h.throw(ExcKind::Arithmetic, format!("cannot migrate {} to {}", what, to))
}

/// Converts one number to another numeric type; narrowing uses `mode` (spec: Tensor.migrate).
pub fn migrate_value<H: Host>(v: &Value, to: &RtType, mode: RoundMode, h: &mut H) -> Result<Value, H::Err> {
    let v = v.deref();
    match to {
        RtType::Int(t) => {
            let x: i128 = match &v {
                Value::Int(_, x) => *x,
                Value::Big(b) => match b.to_i128() {
                    Some(x) => x,
                    None => return Err(h.throw(ExcKind::Arithmetic, format!("integer overflow: {} migrate", t.name()))),
                },
                Value::Float(ft, f) => {
                    if !f.is_finite() {
                        return Err(migrate_err(t.name(), &ops::fmt_float(*ft, *f), h));
                    }
                    let r = round_float(*f, *ft, RoundSpec::Places(0), mode);
                    if r.abs() >= 1.7e38 {
                        return Err(h.throw(ExcKind::Arithmetic, format!("integer overflow: {} migrate", t.name())));
                    }
                    r as i128
                }
                other => return Err(migrate_err(t.name(), &other.type_name(), h)),
            };
            if t.fits(x) {
                Ok(Value::Int(*t, x))
            } else {
                Err(h.throw(ExcKind::Arithmetic, format!("integer overflow: {} migrate", t.name())))
            }
        }
        RtType::Big => Ok(Value::Big(Rc::new(match &v {
            Value::Int(_, x) => BigInt::from_i128(*x),
            Value::Big(b) => (**b).clone(),
            Value::Float(ft, f) => {
                if !f.is_finite() {
                    return Err(migrate_err("IntLarge", &ops::fmt_float(*ft, *f), h));
                }
                let r = round_float(*f, *ft, RoundSpec::Places(0), mode);
                BigInt::parse(&format!("{:.0}", r)).unwrap_or_else(BigInt::zero)
            }
            other => return Err(migrate_err("IntLarge", &other.type_name(), h)),
        }))),
        RtType::Float(t) => Ok(Value::Float(
            *t,
            match &v {
                Value::Float(_, f) => float_to(*f, *t, mode),
                Value::Int(_, x) => {
                    let r = t.round(*x as f64);
                    let exact = if r.is_finite() && r.abs() < 1.7e38 { Some((r as i128).cmp(x)) } else { None };
                    match (mode, exact) {
                        (RoundMode::Ceil, Some(std::cmp::Ordering::Less)) => next_float(r, *t, true),
                        (RoundMode::Floor, Some(std::cmp::Ordering::Greater)) => next_float(r, *t, false),
                        _ => r,
                    }
                }
                Value::Big(b) => {
                    let r = t.round(b.to_f64());
                    let exact = if r.is_finite() { BigInt::parse(&format!("{:.0}", r)).map(|rb| rb.cmp(b)) } else { None };
                    match (mode, exact) {
                        (RoundMode::Ceil, Some(std::cmp::Ordering::Less)) => next_float(r, *t, true),
                        (RoundMode::Floor, Some(std::cmp::Ordering::Greater)) => next_float(r, *t, false),
                        _ => r,
                    }
                }
                other => return Err(migrate_err(t.name(), &other.type_name(), h)),
            },
        )),
        other => Ok(ops::cast(&v, other, false, h)?),
    }
}

// ---------------------------------------------------------------------------------------------
// bulk kernels

/// A host that only records the error (used on worker threads, where no objects exist).
pub struct ErrHost;

impl Host for ErrHost {
    type Err = (ExcKind, String);
    fn throw(&mut self, kind: ExcKind, msg: String) -> (ExcKind, String) {
        (kind, msg)
    }
    fn obj_to_string(&mut self, _: &Rc<Object>) -> Result<String, Self::Err> {
        Err((ExcKind::UnsupportedOperation, "object in a numeric kernel".into()))
    }
    fn obj_default_string(&mut self, _: &Rc<Object>) -> Result<String, Self::Err> {
        Err((ExcKind::UnsupportedOperation, "object in a numeric kernel".into()))
    }
    fn obj_equals(&mut self, _: &Rc<Object>, _: &Rc<Object>) -> Result<bool, Self::Err> {
        Err((ExcKind::UnsupportedOperation, "object in a numeric kernel".into()))
    }
    fn obj_compare(&mut self, _: &Rc<Object>, _: &Rc<Object>) -> Result<i32, Self::Err> {
        Err((ExcKind::UnsupportedOperation, "object in a numeric kernel".into()))
    }
    fn is_subclass(&self, _: u32, _: u32) -> bool {
        false
    }
    fn implements(&self, _: u32, _: u32) -> bool {
        false
    }
    fn class_name(&self, _: u32) -> String {
        String::new()
    }
}

/// Numbers of one array, unboxed when every element has the same primitive type.
enum Nums {
    Int(IntTy, Vec<i128>),
    Float(FloatTy, Vec<f64>),
    Other(Vec<Value>),
}

fn unbox(items: &Items) -> Nums {
    match items {
        Items::U8(v) => Nums::Int(IntTy::U8, v.iter().map(|&x| x as i128).collect()),
        Items::Int(IntTy::U64, v) => Nums::Int(IntTy::U64, v.iter().map(|&x| x as u64 as i128).collect()),
        Items::Int(t, v) => Nums::Int(*t, v.iter().map(|&x| x as i128).collect()),
        Items::Float(t, v) => Nums::Float(*t, v.clone()),
        other => Nums::Other(other.iter().map(|v| v.deref()).collect()),
    }
}

/// Packs kernel results.
fn ints(t: IntTy, v: Vec<i128>) -> Items {
    match t {
        IntTy::U8 => Items::U8(v.into_iter().map(|x| x as u8).collect()),
        IntTy::U64 => Items::Int(t, v.into_iter().map(|x| x as u64 as i64).collect()),
        _ => Items::Int(t, v.into_iter().map(|x| x as i64).collect()),
    }
}

/// Minimum amount of work (element operations) before worker threads are used.
pub const PARALLEL_THRESHOLD: usize = 1 << 15;

/// Number of workers for `work` element operations; `requested` is `1` (sequential), `<= 0`
/// (one per available core) or an explicit maximum.
pub fn worker_count(requested: i128, work: usize, items: usize) -> usize {
    if requested == 1 || work < PARALLEL_THRESHOLD || items < 2 {
        return 1;
    }
    let avail = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let n = if requested <= 0 { avail } else { (requested as usize).min(avail.max(1) * 4) };
    n.clamp(1, items)
}

type KErr = (usize, ExcKind, String);

/// Runs `f` over `0..n` split into contiguous chunks on up to `workers` threads, concatenating
/// the results in order; on failure returns the error with the smallest element index.
fn run_chunks<R: Send>(n: usize, workers: usize, f: &(dyn Fn(Range<usize>) -> Result<Vec<R>, KErr> + Sync)) -> Result<Vec<R>, KErr> {
    if workers <= 1 || n < 2 {
        return f(0..n);
    }
    let chunk = n.div_ceil(workers);
    let ranges: Vec<Range<usize>> = (0..n).step_by(chunk).map(|s| s..(s + chunk).min(n)).collect();
    let results: Vec<Result<Vec<R>, KErr>> = std::thread::scope(|s| {
        let handles: Vec<_> = ranges.into_iter().map(|r| s.spawn(move || f(r))).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err((usize::MAX, ExcKind::UnsupportedOperation, "worker thread panicked".into())))).collect()
    });
    let mut out = Vec::with_capacity(n);
    let mut err: Option<KErr> = None;
    for r in results {
        match r {
            Ok(v) => out.extend(v),
            Err(e) => {
                if err.as_ref().map(|x| e.0 < x.0).unwrap_or(true) {
                    err = Some(e);
                }
            }
        }
    }
    match err {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

fn int_op(op: ArithOp, t: IntTy, a: i128, b: i128, wrap: bool) -> Result<i128, (ExcKind, String)> {
    match ops::int_arith(op, t, a, b, wrap, &mut ErrHost)? {
        Value::Int(_, x) => Ok(x),
        _ => Ok(0),
    }
}

fn kerr<H: Host>(e: KErr, h: &mut H) -> H::Err {
    h.throw(e.1, e.2)
}

fn arr_items<H: Host>(v: &Value, h: &mut H) -> Result<Rc<ArrayVal>, H::Err> {
    match v.deref() {
        Value::Array(a) => Ok(a),
        Value::Null => Err(h.throw(ExcKind::NullPointer, "array is Null".into())),
        other => Err(h.throw(ExcKind::ClassCast, format!("expected array, got {}", other.type_name()))),
    }
}

/// Element-wise `a op b` (same length).
pub fn zip<H: Host>(op: ArithOp, a: &Value, b: &Value, wrap: bool, threads: i128, h: &mut H) -> Result<Value, H::Err> {
    let (a, b) = (arr_items(a, h)?, arr_items(b, h)?);
    if a.items.len() != b.items.len() {
        return Err(h.throw(ExcKind::IllegalArgument, format!("length mismatch: {} vs {}", a.items.len(), b.items.len())));
    }
    let n = a.items.len();
    let workers = worker_count(threads, n, n);
    let items = match (unbox(&a.items), unbox(&b.items)) {
        (Nums::Int(t, x), Nums::Int(u, y)) if t == u => {
            let r = run_chunks(n, workers, &|rg: Range<usize>| rg.map(|i| int_op(op, t, x[i], y[i], wrap).map_err(|e| (i, e.0, e.1))).collect());
            ints(t, r.map_err(|e| kerr(e, h))?)
        }
        (Nums::Float(t, x), Nums::Float(u, y)) if t == u => {
            let r = run_chunks(n, workers, &|rg: Range<usize>| Ok(rg.map(|i| ops::float_arith(op, t, x[i], y[i])).collect()));
            Items::Float(t, r.map_err(|e| kerr(e, h))?)
        }
        _ => {
            let mut out = Vec::with_capacity(n);
            for (x, y) in a.items.iter().zip(b.items.iter()) {
                out.push(ops::arith(op, &x.deref(), &y.deref(), wrap, h)?);
            }
            Items::from_values(out)
        }
    };
    Ok(Value::packed(items, a.fixed))
}

/// `a op s` (or `s op a` when `scalar_left`) for every element.
pub fn scalar<H: Host>(op: ArithOp, a: &Value, s: &Value, scalar_left: bool, wrap: bool, threads: i128, h: &mut H) -> Result<Value, H::Err> {
    let a = arr_items(a, h)?;
    let s = s.deref();
    let n = a.items.len();
    let workers = worker_count(threads, n, n);
    let items = match (unbox(&a.items), &s) {
        (Nums::Int(t, x), Value::Int(u, y)) if t == *u => {
            let y = *y;
            let r = run_chunks(n, workers, &|rg: Range<usize>| {
                rg.map(|i| if scalar_left { int_op(op, t, y, x[i], wrap) } else { int_op(op, t, x[i], y, wrap) }.map_err(|e| (i, e.0, e.1))).collect()
            });
            ints(t, r.map_err(|e| kerr(e, h))?)
        }
        (Nums::Float(t, x), Value::Float(u, y)) if t == *u => {
            let y = *y;
            let r = run_chunks(n, workers, &|rg: Range<usize>| Ok(rg.map(|i| if scalar_left { ops::float_arith(op, t, y, x[i]) } else { ops::float_arith(op, t, x[i], y) }).collect()));
            Items::Float(t, r.map_err(|e| kerr(e, h))?)
        }
        _ => {
            let mut out = Vec::with_capacity(n);
            for x in a.items.iter() {
                let x = x.deref();
                out.push(if scalar_left { ops::arith(op, &s, &x, wrap, h)? } else { ops::arith(op, &x, &s, wrap, h)? });
            }
            Items::from_values(out)
        }
    };
    Ok(Value::packed(items, a.fixed))
}

#[allow(clippy::too_many_arguments)]
/// Matrix product of `a` (n×k) and `b` (k×m), both row-major; `k >= 1`. Each result element is
/// the left-to-right sum of its products, so threading never changes the result.
pub fn matmul<H: Host>(a: &Value, b: &Value, n: usize, k: usize, m: usize, wrap: bool, threads: i128, h: &mut H) -> Result<Value, H::Err> {
    let (a, b) = (arr_items(a, h)?, arr_items(b, h)?);
    if k == 0 || a.items.len() != n * k || b.items.len() != k * m {
        return Err(h.throw(ExcKind::IllegalArgument, format!("matmul shape mismatch: {}x{} * {}x{}", n, k, k, m)));
    }
    let workers = worker_count(threads, n.saturating_mul(k).saturating_mul(m), n);
    let items: Items = match (unbox(&a.items), unbox(&b.items)) {
        (Nums::Int(t, x), Nums::Int(u, y)) if t == u => {
            // row i of the result accumulates the products for p = 0, 1, ... in order while
            // streaming through row p of b (cache friendly; same sums as the dot-product order)
            let rows = run_chunks(n, workers, &|rg: Range<usize>| {
                let mut out = Vec::with_capacity(rg.len() * m);
                for i in rg {
                    let mut acc: Vec<i128> = Vec::with_capacity(m);
                    for &yv in &y[..m] {
                        acc.push(int_op(ArithOp::Mul, t, x[i * k], yv, wrap).map_err(|e| (i, e.0, e.1))?);
                    }
                    for p in 1..k {
                        let xv = x[i * k + p];
                        let yr = &y[p * m..p * m + m];
                        for j in 0..m {
                            let prod = int_op(ArithOp::Mul, t, xv, yr[j], wrap).map_err(|e| (i, e.0, e.1))?;
                            acc[j] = int_op(ArithOp::Add, t, acc[j], prod, wrap).map_err(|e| (i, e.0, e.1))?;
                        }
                    }
                    out.extend(acc);
                }
                Ok(out)
            });
            ints(t, rows.map_err(|e| kerr(e, h))?)
        }
        (Nums::Float(t, x), Nums::Float(u, y)) if t == u => {
            let rows = run_chunks(n, workers, &|rg: Range<usize>| {
                let mut out = Vec::with_capacity(rg.len() * m);
                for i in rg {
                    let xv = x[i * k];
                    let mut acc: Vec<f64> = y[..m].iter().map(|&b| ops::float_arith(ArithOp::Mul, t, xv, b)).collect();
                    for p in 1..k {
                        let xv = x[i * k + p];
                        let yr = &y[p * m..p * m + m];
                        if t == FloatTy::F64 {
                            for (a, &b) in acc.iter_mut().zip(yr) {
                                *a += xv * b;
                            }
                        } else {
                            for (a, &b) in acc.iter_mut().zip(yr) {
                                let prod = ops::float_arith(ArithOp::Mul, t, xv, b);
                                *a = ops::float_arith(ArithOp::Add, t, *a, prod);
                            }
                        }
                    }
                    out.extend(acc);
                }
                Ok(out)
            });
            Items::Float(t, rows.map_err(|e| kerr(e, h))?)
        }
        _ => {
            let mut out = Vec::with_capacity(n * m);
            for i in 0..n {
                for j in 0..m {
                    let mut acc = ops::arith(ArithOp::Mul, &a.items.get(i * k).deref(), &b.items.get(j).deref(), wrap, h)?;
                    for p in 1..k {
                        let prod = ops::arith(ArithOp::Mul, &a.items.get(i * k + p).deref(), &b.items.get(p * m + j).deref(), wrap, h)?;
                        acc = ops::arith(ArithOp::Add, &acc, &prod, wrap, h)?;
                    }
                    out.push(acc);
                }
            }
            Items::from_values(out)
        }
    };
    Ok(Value::packed(items, a.fixed))
}

/// Rounds every element (`round` / `ceil` / `floor` with the number method's arguments).
pub fn round_all<H: Host>(a: &Value, spec: RoundSpec, mode: RoundMode, wrap: bool, threads: i128, h: &mut H) -> Result<Value, H::Err> {
    let a = arr_items(a, h)?;
    let n = a.items.len();
    let workers = worker_count(threads, n.saturating_mul(16), n);
    let items: Items = match unbox(&a.items) {
        Nums::Float(t, x) => {
            let r = run_chunks(n, workers, &|rg: Range<usize>| Ok(rg.map(|i| round_float(x[i], t, spec, mode)).collect()));
            Items::Float(t, r.map_err(|e| kerr(e, h))?)
        }
        Nums::Int(t, x) => {
            let r = run_chunks(n, workers, &|rg: Range<usize>| {
                rg.map(|i| match round_int(x[i], spec, mode) {
                    Some(v) if t.fits(v) => Ok(v),
                    Some(v) if wrap => Ok(t.wrap(v)),
                    _ => Err((i, ExcKind::Arithmetic, format!("integer overflow: {} {}", t.name(), mode.name()))),
                })
                .collect()
            });
            ints(t, r.map_err(|e| kerr(e, h))?)
        }
        Nums::Other(vs) => {
            let mut out = Vec::with_capacity(n);
            for v in &vs {
                out.push(round_value(v, spec, mode, wrap, h)?);
            }
            Items::from_values(out)
        }
    };
    Ok(Value::packed(items, a.fixed))
}

/// Converts every element to the numeric type `to` (spec: Tensor.migrate).
pub fn migrate_all<H: Host>(a: &Value, to: &RtType, mode: RoundMode, threads: i128, h: &mut H) -> Result<Value, H::Err> {
    let a = arr_items(a, h)?;
    let n = a.items.len();
    let workers = worker_count(threads, n.saturating_mul(8), n);
    // plain numbers only on the workers: (int type, ints) or (float type, floats)
    let (int_vals, floats) = match unbox(&a.items) {
        Nums::Int(t, x) => (Some((t, x)), None),
        Nums::Float(t, x) => (None, Some((t, x))),
        Nums::Other(_) => (None, None),
    };
    let items: Items = match to {
        RtType::Int(_) | RtType::Float(_) if int_vals.is_some() || floats.is_some() => {
            let r = run_chunks(n, workers, &|rg: Range<usize>| {
                let mut out = Vec::with_capacity(rg.len());
                for i in rg {
                    let v = match (&int_vals, &floats) {
                        (Some((t, x)), _) => Value::Int(*t, x[i]),
                        (_, Some((t, x))) => Value::Float(*t, x[i]),
                        _ => Value::Null,
                    };
                    let r = migrate_value(&v, to, mode, &mut ErrHost).map_err(|e| (i, e.0, e.1))?;
                    out.push(match r {
                        Value::Int(_, x) => (x, 0.0),
                        Value::Float(_, f) => (0, f),
                        _ => (0, 0.0),
                    });
                }
                Ok(out)
            });
            let r = r.map_err(|e| kerr(e, h))?;
            match to {
                RtType::Int(t) => ints(*t, r.into_iter().map(|(x, _)| x).collect()),
                RtType::Float(t) => Items::Float(*t, r.into_iter().map(|(_, f)| f).collect()),
                _ => unreachable!(),
            }
        }
        _ => {
            let mut out = Vec::with_capacity(n);
            for v in a.items.iter() {
                out.push(migrate_value(&v, to, mode, h)?);
            }
            Items::from_values(out)
        }
    };
    Ok(Value::packed(items, a.fixed))
}

/// Transposes a row-major `rows × cols` array.
pub fn transpose<H: Host>(a: &Value, rows: usize, cols: usize, h: &mut H) -> Result<Value, H::Err> {
    let a = arr_items(a, h)?;
    if a.items.len() != rows * cols {
        return Err(h.throw(ExcKind::IllegalArgument, format!("transpose shape mismatch: {} elements for {}x{}", a.items.len(), rows, cols)));
    }
    let idx = (0..cols).flat_map(|j| (0..rows).map(move |i| i * cols + j));
    let items = match &a.items {
        Items::U8(v) => Items::U8(idx.map(|i| v[i]).collect()),
        Items::Int(t, v) => Items::Int(*t, idx.map(|i| v[i]).collect()),
        Items::Float(t, v) => Items::Float(*t, idx.map(|i| v[i]).collect()),
        Items::Bool(v) => Items::Bool(idx.map(|i| v[i]).collect()),
        Items::Vals(v) => Items::Vals(idx.map(|i| v[i].clone()).collect()),
    };
    Ok(Value::packed(items, a.fixed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(v: f64, args: &[i128], mode: RoundMode) -> f64 {
        round_float(v, FloatTy::F64, RoundSpec::from_args(args).unwrap(), mode)
    }

    #[test]
    fn rounding() {
        assert_eq!(r(123.456, &[], RoundMode::Round), 123.0);
        assert_eq!(r(123.456, &[1], RoundMode::Round), 123.5);
        assert_eq!(r(123.456, &[2], RoundMode::Round), 123.46);
        assert_eq!(r(123.456, &[2, 1], RoundMode::Round), 100.5);
        assert_eq!(r(123.456, &[2, 1], RoundMode::Ceil), 200.5);
        assert_eq!(r(123.456, &[2, 1], RoundMode::Floor), 100.4);
        assert_eq!(r(-123.456, &[2, 1], RoundMode::Round), -100.5);
        assert_eq!(r(-123.456, &[2, 1], RoundMode::Ceil), -100.4);
        assert_eq!(r(2.675, &[2], RoundMode::Round), 2.68);
        assert_eq!(r(2.5, &[], RoundMode::Round), 3.0);
        assert_eq!(r(-2.5, &[], RoundMode::Round), -3.0);
        assert_eq!(r(1.1, &[1], RoundMode::Ceil), 1.1);
        assert_eq!(r(1234.5, &[-2], RoundMode::Round), 1200.0);
        assert_eq!(r(-0.4, &[], RoundMode::Round), 0.0);
        assert_eq!(round_int(127, RoundSpec::Places(1), RoundMode::Round), Some(130));
        assert_eq!(round_int(-125, RoundSpec::Places(1), RoundMode::Ceil), Some(-120));
    }

    #[test]
    fn directed_float_narrowing() {
        let x = 0.1f64;
        let near = float_to(x, FloatTy::F32, RoundMode::Round);
        let up = float_to(x, FloatTy::F32, RoundMode::Ceil);
        let down = float_to(x, FloatTy::F32, RoundMode::Floor);
        assert!(down <= x && x <= up && down < up);
        assert!(near == up || near == down);
    }
}
