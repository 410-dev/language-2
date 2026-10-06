//! Minimal arbitrary-precision signed integer used for `IntLarge`.
//!
//! Magnitude is stored little-endian in base 10^9 so that decimal printing is trivial.

use std::cmp::Ordering;

const BASE: u64 = 1_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BigInt {
    neg: bool,
    mag: Vec<u32>, // little-endian base 1e9, no trailing zeros; zero = empty
}

impl BigInt {
    pub fn zero() -> Self {
        BigInt { neg: false, mag: Vec::new() }
    }

    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }

    pub fn is_negative(&self) -> bool {
        self.neg
    }

    pub fn from_i128(v: i128) -> Self {
        let neg = v < 0;
        let mut u = v.unsigned_abs();
        let mut mag = Vec::new();
        while u > 0 {
            mag.push((u % BASE as u128) as u32);
            u /= BASE as u128;
        }
        BigInt { neg, mag }
    }

    pub fn to_i128(&self) -> Option<i128> {
        let mut acc: i128 = 0;
        for &d in self.mag.iter().rev() {
            acc = acc.checked_mul(BASE as i128)?.checked_add(d as i128)?;
        }
        Some(if self.neg { -acc } else { acc })
    }

    pub fn to_f64(&self) -> f64 {
        let mut acc = 0f64;
        for &d in self.mag.iter().rev() {
            acc = acc * BASE as f64 + d as f64;
        }
        if self.neg {
            -acc
        } else {
            acc
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (neg, digits) = if let Some(r) = s.strip_prefix('-') {
            (true, r)
        } else if let Some(r) = s.strip_prefix('+') {
            (false, r)
        } else {
            (false, s)
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let bytes = digits.as_bytes();
        let mut mag = Vec::new();
        let mut end = bytes.len();
        while end > 0 {
            let start = end.saturating_sub(9);
            let chunk = std::str::from_utf8(&bytes[start..end]).ok()?;
            mag.push(chunk.parse::<u32>().ok()?);
            end = start;
        }
        let mut r = BigInt { neg, mag };
        r.trim();
        Some(r)
    }

    fn trim(&mut self) {
        while self.mag.last() == Some(&0) {
            self.mag.pop();
        }
        if self.mag.is_empty() {
            self.neg = false;
        }
    }

    fn cmp_mag(a: &[u32], b: &[u32]) -> Ordering {
        if a.len() != b.len() {
            return a.len().cmp(&b.len());
        }
        for i in (0..a.len()).rev() {
            if a[i] != b[i] {
                return a[i].cmp(&b[i]);
            }
        }
        Ordering::Equal
    }

    fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut r = Vec::with_capacity(a.len().max(b.len()) + 1);
        let mut carry = 0u64;
        for i in 0..a.len().max(b.len()) {
            let s = carry + *a.get(i).unwrap_or(&0) as u64 + *b.get(i).unwrap_or(&0) as u64;
            r.push((s % BASE) as u32);
            carry = s / BASE;
        }
        if carry > 0 {
            r.push(carry as u32);
        }
        r
    }

    // requires |a| >= |b|
    fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut r = Vec::with_capacity(a.len());
        let mut borrow = 0i64;
        for i in 0..a.len() {
            let mut d = a[i] as i64 - borrow - *b.get(i).unwrap_or(&0) as i64;
            if d < 0 {
                d += BASE as i64;
                borrow = 1;
            } else {
                borrow = 0;
            }
            r.push(d as u32);
        }
        r
    }

    fn mul_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        if a.is_empty() || b.is_empty() {
            return Vec::new();
        }
        let mut r = vec![0u64; a.len() + b.len() + 1];
        for (i, &x) in a.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &y) in b.iter().enumerate() {
                let cur = r[i + j] + x as u64 * y as u64 + carry;
                r[i + j] = cur % BASE;
                carry = cur / BASE;
            }
            let mut k = i + b.len();
            while carry > 0 {
                let cur = r[k] + carry;
                r[k] = cur % BASE;
                carry = cur / BASE;
                k += 1;
            }
        }
        r.into_iter().map(|d| d as u32).collect()
    }

    fn mul_small(a: &[u32], m: u64) -> Vec<u32> {
        let mut r = Vec::with_capacity(a.len() + 1);
        let mut carry = 0u64;
        for &d in a {
            let cur = d as u64 * m + carry;
            r.push((cur % BASE) as u32);
            carry = cur / BASE;
        }
        while carry > 0 {
            r.push((carry % BASE) as u32);
            carry /= BASE;
        }
        while r.last() == Some(&0) {
            r.pop();
        }
        r
    }

    // schoolbook long division on magnitudes; returns (quotient, remainder)
    fn divmod_mag(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
        if Self::cmp_mag(a, b) == Ordering::Less {
            return (Vec::new(), a.to_vec());
        }
        let mut q = vec![0u32; a.len()];
        let mut rem: Vec<u32> = Vec::new();
        for i in (0..a.len()).rev() {
            // rem = rem * BASE + a[i]
            rem.insert(0, a[i]);
            while rem.last() == Some(&0) {
                rem.pop();
            }
            // binary search the largest d with b*d <= rem
            let (mut lo, mut hi) = (0u64, BASE - 1);
            while lo < hi {
                let mid = (lo + hi + 1) / 2;
                let t = Self::mul_small(b, mid);
                if Self::cmp_mag(&t, &rem) != Ordering::Greater {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            if lo > 0 {
                let t = Self::mul_small(b, lo);
                rem = Self::sub_mag(&rem, &t);
                while rem.last() == Some(&0) {
                    rem.pop();
                }
            }
            q[i] = lo as u32;
        }
        while q.last() == Some(&0) {
            q.pop();
        }
        (q, rem)
    }

    pub fn neg(&self) -> Self {
        let mut r = self.clone();
        if !r.is_zero() {
            r.neg = !r.neg;
        }
        r
    }

    pub fn add(&self, o: &Self) -> Self {
        let mut r = if self.neg == o.neg {
            BigInt { neg: self.neg, mag: Self::add_mag(&self.mag, &o.mag) }
        } else {
            match Self::cmp_mag(&self.mag, &o.mag) {
                Ordering::Less => BigInt { neg: o.neg, mag: Self::sub_mag(&o.mag, &self.mag) },
                _ => BigInt { neg: self.neg, mag: Self::sub_mag(&self.mag, &o.mag) },
            }
        };
        r.trim();
        r
    }

    pub fn sub(&self, o: &Self) -> Self {
        self.add(&o.neg())
    }

    pub fn mul(&self, o: &Self) -> Self {
        let mut r = BigInt { neg: self.neg != o.neg, mag: Self::mul_mag(&self.mag, &o.mag) };
        r.trim();
        r
    }

    /// Truncating division (toward zero) and remainder with the sign of the dividend.
    pub fn divmod(&self, o: &Self) -> Option<(Self, Self)> {
        if o.is_zero() {
            return None;
        }
        let (q, r) = Self::divmod_mag(&self.mag, &o.mag);
        let mut q = BigInt { neg: self.neg != o.neg, mag: q };
        let mut r = BigInt { neg: self.neg, mag: r };
        q.trim();
        r.trim();
        Some((q, r))
    }

    pub fn pow(&self, mut e: u64) -> Self {
        let mut base = self.clone();
        let mut acc = BigInt::from_i128(1);
        while e > 0 {
            if e & 1 == 1 {
                acc = acc.mul(&base);
            }
            e >>= 1;
            if e > 0 {
                base = base.mul(&base);
            }
        }
        acc
    }

    pub fn cmp(&self, o: &Self) -> Ordering {
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => Self::cmp_mag(&self.mag, &o.mag),
            (true, true) => Self::cmp_mag(&o.mag, &self.mag),
        }
    }
}

impl std::fmt::Display for BigInt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.mag.is_empty() {
            return write!(f, "0");
        }
        let mut s = String::new();
        if self.neg {
            s.push('-');
        }
        s.push_str(&self.mag.last().unwrap().to_string());
        for d in self.mag.iter().rev().skip(1) {
            s.push_str(&format!("{:09}", d));
        }
        write!(f, "{}", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic() {
        let a = BigInt::parse("123456789012345678901234567890").unwrap();
        let b = BigInt::parse("-987654321").unwrap();
        assert_eq!(a.add(&b).to_string(), "123456789012345678900246913569");
        assert_eq!(a.mul(&b).to_string(), "-121932631124828532112482853211126352690");
        let (q, r) = a.divmod(&b).unwrap();
        assert_eq!(q.to_string(), "-124999998873437499901");
        assert!(!r.is_negative());
        assert_eq!(q.mul(&b).add(&r), a);
        assert_eq!(BigInt::from_i128(2).pow(100).to_string(), "1267650600228229401496703205376");
    }
}
