//! Mathematical functions of numbers (spec 12.3) and `math.Math` (spec 14.14).
//!
//! Floating-point functions use the pure-Rust `libm`, so results are bit-for-bit identical on
//! every operating system and backend. Float16 / Float32 values are computed in Float64 and
//! rounded to their type; integers give Float64 results.

use crate::bigint::BigInt;
use crate::ops;
use crate::value::*;
use std::rc::Rc;

macro_rules! math_ops {
    ($($name:ident: $args:literal $kind:ident),* $(,)?) => {
        #[allow(non_camel_case_types)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum MathOp { $($name),* }
        impl MathOp {
            pub const ALL: &'static [MathOp] = &[$(MathOp::$name),*];
            pub fn name(self) -> &'static str {
                match self { $(MathOp::$name => stringify!($name)),* }
            }
            /// Number of arguments besides the receiver.
            pub fn arity(self) -> usize {
                match self { $(MathOp::$name => $args),* }
            }
            pub fn kind(self) -> Kind {
                match self { $(MathOp::$name => Kind::$kind),* }
            }
            pub fn by_name(n: &str, arity: usize) -> Option<MathOp> {
                Self::ALL.iter().copied().find(|o| o.name().trim_end_matches('_') == n && o.arity() == arity)
            }
            pub fn code(self) -> u16 {
                self as u16
            }
            pub fn from_code(c: u16) -> MathOp {
                Self::ALL[c as usize]
            }
        }
    };
}

/// Result and argument typing of a math method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Float result (the receiver's float type, Float64 for integers); Float64 arguments.
    Float,
    /// Result and arguments of the receiver's type.
    Same,
    /// Integers only; result and arguments of the receiver's type.
    Integer,
    /// Boolean result.
    Test,
}

math_ops! {
    squareRoot: 0 Float,
    cubeRoot: 0 Float,
    powerOf: 1 Float,
    exponential: 0 Float,
    logarithm: 0 Float,
    logarithm_: 1 Float,
    logarithmBase2: 0 Float,
    logarithmBase10: 0 Float,
    sine: 0 Float,
    cosine: 0 Float,
    tangent: 0 Float,
    arcSine: 0 Float,
    arcCosine: 0 Float,
    arcTangent: 0 Float,
    hyperbolicSine: 0 Float,
    hyperbolicCosine: 0 Float,
    hyperbolicTangent: 0 Float,
    toRadians: 0 Float,
    toDegrees: 0 Float,
    sign: 0 Same,
    clamp: 2 Same,
    truncate: 0 Same,
    isNaN: 0 Test,
    isInfinite: 0 Test,
    isFinite: 0 Test,
    greatestCommonDivisor: 1 Integer,
    leastCommonMultiple: 1 Integer,
    integerSquareRoot: 0 Integer,
    modularPower: 2 Integer,
    // two-argument functions of math.Math (receiver = first argument)
    arcTangent2: 1 Float,
    hypotenuse: 1 Float,
}

fn float_fn(op: MathOp, x: f64, a: &[f64]) -> f64 {
    use MathOp::*;
    match op {
        squareRoot => libm::sqrt(x),
        cubeRoot => libm::cbrt(x),
        powerOf => libm::pow(x, a[0]),
        exponential => libm::exp(x),
        logarithm => libm::log(x),
        logarithm_ => libm::log(x) / libm::log(a[0]),
        logarithmBase2 => libm::log2(x),
        logarithmBase10 => libm::log10(x),
        sine => libm::sin(x),
        cosine => libm::cos(x),
        tangent => libm::tan(x),
        arcSine => libm::asin(x),
        arcCosine => libm::acos(x),
        arcTangent => libm::atan(x),
        hyperbolicSine => libm::sinh(x),
        hyperbolicCosine => libm::cosh(x),
        hyperbolicTangent => libm::tanh(x),
        toRadians => x * (std::f64::consts::PI / 180.0),
        toDegrees => x * (180.0 / std::f64::consts::PI),
        arcTangent2 => libm::atan2(x, a[0]),
        hypotenuse => libm::hypot(x, a[0]),
        _ => f64::NAN,
    }
}

fn big(v: &Value) -> BigInt {
    match v {
        Value::Big(b) => (**b).clone(),
        Value::Int(_, x) => BigInt::from_i128(*x),
        _ => BigInt::zero(),
    }
}

fn big_abs(b: &BigInt) -> BigInt {
    if b.is_negative() {
        b.neg()
    } else {
        b.clone()
    }
}

fn big_gcd(a: &BigInt, b: &BigInt) -> BigInt {
    let (mut a, mut b) = (big_abs(a), big_abs(b));
    while !b.is_zero() {
        let (_, r) = a.divmod(&b).unwrap();
        a = b;
        b = r;
    }
    a
}

fn big_isqrt(n: &BigInt) -> BigInt {
    if n.is_zero() {
        return BigInt::zero();
    }
    // Newton iteration from above
    let two = BigInt::from_i128(2);
    let mut x = n.clone();
    loop {
        let (q, _) = n.divmod(&x).unwrap();
        let (y, _) = x.add(&q).divmod(&two).unwrap();
        if y.cmp(&x) != std::cmp::Ordering::Less {
            return x;
        }
        x = y;
    }
}

fn big_mod(a: &BigInt, m: &BigInt) -> BigInt {
    let (_, r) = a.divmod(m).unwrap();
    if r.is_negative() {
        r.add(m)
    } else {
        r
    }
}

fn big_modpow(base: &BigInt, exp: &BigInt, m: &BigInt) -> BigInt {
    let one = BigInt::from_i128(1);
    if m.cmp(&one) == std::cmp::Ordering::Equal {
        return BigInt::zero();
    }
    let two = BigInt::from_i128(2);
    let mut result = one;
    let mut b = big_mod(base, m);
    let mut e = exp.clone();
    while !e.is_zero() {
        let (q, r) = e.divmod(&two).unwrap();
        if !r.is_zero() {
            result = big_mod(&result.mul(&b), m);
        }
        b = big_mod(&b.mul(&b), m);
        e = q;
    }
    result
}

fn int_result<H: Host>(t: IntTy, v: i128, wrap: bool, what: &str, h: &mut H) -> Result<Value, H::Err> {
    if t.fits(v) {
        Ok(Value::Int(t, v))
    } else if wrap {
        Ok(Value::Int(t, t.wrap(v)))
    } else {
        Err(h.throw(ExcKind::Arithmetic, format!("integer overflow: {} {}", t.name(), what)))
    }
}

/// `recv.op(args)`; `wrap` is the file's integer overflow policy.
pub fn call<H: Host>(op: MathOp, recv: &Value, args: &[Value], wrap: bool, h: &mut H) -> Result<Value, H::Err> {
    use MathOp::*;
    let recv = recv.deref();
    let args: Vec<Value> = args.iter().map(|a| a.deref()).collect();
    let arith = |h: &mut H, msg: String| h.throw(ExcKind::Arithmetic, msg);
    match op.kind() {
        Kind::Float => {
            let x = recv.as_f64();
            let a: Vec<f64> = args.iter().map(|v| v.as_f64()).collect();
            let r = float_fn(op, x, &a);
            Ok(match recv {
                Value::Float(t, _) => Value::Float(t, t.round(r)),
                _ => Value::Float(FloatTy::F64, r),
            })
        }
        Kind::Test => Ok(Value::Bool(match recv {
            Value::Float(_, f) => match op {
                isNaN => f.is_nan(),
                isInfinite => f.is_infinite(),
                _ => f.is_finite(),
            },
            _ => op == isFinite,
        })),
        Kind::Same => match op {
            sign => Ok(match &recv {
                Value::Int(t, x) => Value::Int(*t, x.signum()),
                Value::Big(b) => Value::Big(Rc::new(BigInt::from_i128(if b.is_zero() { 0 } else if b.is_negative() { -1 } else { 1 }))),
                Value::Float(t, f) => Value::Float(*t, if f.is_nan() || *f == 0.0 { *f } else { f.signum() }),
                other => other.clone(),
            }),
            truncate => Ok(match &recv {
                Value::Float(t, f) => Value::Float(*t, f.trunc()),
                other => other.clone(),
            }),
            _ => {
                // clamp(min, max)
                let (lo, hi) = (&args[0], &args[1]);
                if ops::compare_values(lo, hi, h)? == std::cmp::Ordering::Greater {
                    let (a, b) = (ops::to_display(lo, h)?, ops::to_display(hi, h)?);
                    return Err(h.throw(ExcKind::IllegalArgument, format!("clamp: minimum {} is greater than maximum {}", a, b)));
                }
                if let Value::Float(_, f) = recv {
                    if f.is_nan() {
                        return Ok(recv);
                    }
                }
                Ok(if ops::compare_values(&recv, lo, h)? == std::cmp::Ordering::Less {
                    lo.clone()
                } else if ops::compare_values(&recv, hi, h)? == std::cmp::Ordering::Greater {
                    hi.clone()
                } else {
                    recv
                })
            }
        },
        Kind::Integer => {
            if let Value::Big(_) = recv {
                let x = big(&recv);
                let a: Vec<BigInt> = args.iter().map(big).collect();
                let r = match op {
                    greatestCommonDivisor => big_gcd(&x, &a[0]),
                    leastCommonMultiple => {
                        if x.is_zero() || a[0].is_zero() {
                            BigInt::zero()
                        } else {
                            let g = big_gcd(&x, &a[0]);
                            let (q, _) = big_abs(&x).divmod(&g).unwrap();
                            q.mul(&big_abs(&a[0]))
                        }
                    }
                    integerSquareRoot => {
                        if x.is_negative() {
                            return Err(arith(h, "integerSquareRoot of a negative number".into()));
                        }
                        big_isqrt(&x)
                    }
                    _ => {
                        if a[0].is_negative() {
                            return Err(arith(h, "modularPower with a negative exponent".into()));
                        }
                        if !(a[1].cmp(&BigInt::zero()) == std::cmp::Ordering::Greater) {
                            return Err(arith(h, "modularPower needs a positive modulus".into()));
                        }
                        big_modpow(&x, &a[0], &a[1])
                    }
                };
                return Ok(Value::Big(Rc::new(r)));
            }
            let Value::Int(t, x) = recv else {
                return Err(h.throw(ExcKind::IllegalArgument, format!("{} needs an integer", op.name())));
            };
            let a: Vec<i128> = args.iter().map(|v| v.as_int()).collect();
            let gcd = |mut p: i128, mut q: i128| {
                p = p.abs();
                q = q.abs();
                while q != 0 {
                    let r = p % q;
                    p = q;
                    q = r;
                }
                p
            };
            match op {
                greatestCommonDivisor => int_result(t, gcd(x, a[0]), wrap, "greatestCommonDivisor", h),
                leastCommonMultiple => {
                    let v = if x == 0 || a[0] == 0 { 0 } else { (x / gcd(x, a[0])).abs().checked_mul(a[0].abs()).unwrap_or(i128::MAX) };
                    int_result(t, v, wrap, "leastCommonMultiple", h)
                }
                integerSquareRoot => {
                    if x < 0 {
                        return Err(arith(h, "integerSquareRoot of a negative number".into()));
                    }
                    let mut r = (x as f64).sqrt() as i128;
                    while r * r > x {
                        r -= 1;
                    }
                    while (r + 1) * (r + 1) <= x {
                        r += 1;
                    }
                    Ok(Value::Int(t, r))
                }
                _ => {
                    let (e, m) = (a[0], a[1]);
                    if e < 0 {
                        return Err(arith(h, "modularPower with a negative exponent".into()));
                    }
                    if m <= 0 {
                        return Err(arith(h, "modularPower needs a positive modulus".into()));
                    }
                    let m = m as u128;
                    let mut b = x.rem_euclid(m as i128) as u128;
                    let mut e = e as u128;
                    let mut r: u128 = 1 % m;
                    while e > 0 {
                        if e & 1 == 1 {
                            r = mulmod(r, b, m);
                        }
                        b = mulmod(b, b, m);
                        e >>= 1;
                    }
                    Ok(Value::Int(t, r as i128))
                }
            }
        }
    }
}

/// `a * b % m` without overflow (operands below 2^64 for every built-in integer type).
fn mulmod(a: u128, b: u128, m: u128) -> u128 {
    if let Some(p) = a.checked_mul(b) {
        return p % m;
    }
    let (mut r, mut a, mut b) = (0u128, a % m, b);
    while b > 0 {
        if b & 1 == 1 {
            r = (r + a) % m;
        }
        a = (a << 1) % m;
        b >>= 1;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(MathOp::by_name("logarithm", 0), Some(MathOp::logarithm));
        assert_eq!(MathOp::by_name("logarithm", 1), Some(MathOp::logarithm_));
        assert_eq!(MathOp::by_name("squareRoot", 1), None);
        assert_eq!(big_isqrt(&BigInt::parse("1000000000000000000000000000000").unwrap()).to_string(), "1000000000000000");
        assert_eq!(big_modpow(&BigInt::from_i128(4), &BigInt::from_i128(13), &BigInt::from_i128(497)).to_string(), "445");
        assert_eq!(mulmod(u64::MAX as u128, u64::MAX as u128, 1_000_000_007), ((u64::MAX as u128 % 1_000_000_007).pow(2)) % 1_000_000_007);
    }
}
