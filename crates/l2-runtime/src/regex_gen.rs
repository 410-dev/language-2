//! Random strings that match a regular expression (`String.random`, `s.randomize`, spec 11.2).
//!
//! The pattern is parsed with `regex-syntax`. For every node the set of lengths it can produce
//! (up to a cap) is computed first, so a string of an exact requested length is generated
//! without retries, choosing uniformly among the feasible lengths, alternatives, repetition
//! counts and characters.

use regex_syntax::hir::{Class, Hir, HirKind};
use std::collections::HashMap;

/// Extra length that unbounded repetitions (`*`, `+`, `{n,}`) may add when no length is given.
const DEFAULT_GROWTH: usize = 16;

type Lens = Vec<bool>;

fn single(n: usize, cap: usize) -> Lens {
    let mut v = vec![false; cap + 1];
    if n <= cap {
        v[n] = true;
    }
    v
}

fn conv(a: &Lens, b: &Lens) -> Lens {
    let cap = a.len() - 1;
    let mut out = vec![false; cap + 1];
    let bs: Vec<usize> = b.iter().enumerate().filter(|(_, &y)| y).map(|(j, _)| j).collect();
    for (i, &x) in a.iter().enumerate() {
        if !x {
            continue;
        }
        for &j in &bs {
            if i + j > cap {
                break;
            }
            out[i + j] = true;
        }
    }
    out
}

/// The only length of `l`, if it has exactly one.
fn fixed(l: &Lens) -> Option<usize> {
    let mut it = l.iter().enumerate().filter(|(_, &b)| b).map(|(i, _)| i);
    let first = it.next()?;
    it.next().is_none().then_some(first)
}

fn min_len(h: &Hir) -> usize {
    match h.kind() {
        HirKind::Empty | HirKind::Look(_) => 0,
        HirKind::Literal(l) => String::from_utf8_lossy(&l.0).chars().count(),
        HirKind::Class(_) => 1,
        HirKind::Capture(c) => min_len(&c.sub),
        HirKind::Concat(xs) => xs.iter().map(min_len).fold(0usize, |a, b| a.saturating_add(b)),
        HirKind::Alternation(xs) => xs.iter().map(min_len).min().unwrap_or(0),
        HirKind::Repetition(r) => (r.min as usize).saturating_mul(min_len(&r.sub)),
    }
}

fn union(a: &mut Lens, b: &Lens) {
    for (x, &y) in a.iter_mut().zip(b) {
        *x |= y;
    }
}

struct Gen<'r> {
    cap: usize,
    memo: HashMap<*const Hir, Lens>,
    rand: &'r mut dyn FnMut(usize) -> usize,
}

impl Gen<'_> {
    fn lens(&mut self, h: &Hir) -> Lens {
        let key = h as *const Hir;
        if let Some(l) = self.memo.get(&key) {
            return l.clone();
        }
        let cap = self.cap;
        let l = match h.kind() {
            HirKind::Empty | HirKind::Look(_) => single(0, cap),
            HirKind::Literal(lit) => single(String::from_utf8_lossy(&lit.0).chars().count(), cap),
            HirKind::Class(c) => {
                if class_size(c) == 0 {
                    vec![false; cap + 1]
                } else {
                    single(1, cap)
                }
            }
            HirKind::Capture(c) => self.lens(&c.sub),
            HirKind::Concat(xs) => {
                let mut acc = single(0, cap);
                for x in xs {
                    let l = self.lens(x);
                    acc = conv(&acc, &l);
                }
                acc
            }
            HirKind::Alternation(xs) => {
                let mut acc = vec![false; cap + 1];
                for x in xs {
                    let l = self.lens(x);
                    union(&mut acc, &l);
                }
                acc
            }
            HirKind::Repetition(r) => {
                let sub = self.lens(&r.sub);
                let mut acc = vec![false; cap + 1];
                if let Some(c) = fixed(&sub) {
                    // k copies of a fixed-length part: lengths k * c
                    let hi = r.max.map(|m| m as usize).unwrap_or(usize::MAX);
                    let mut k = r.min as usize;
                    while k <= hi && k.saturating_mul(c) <= cap {
                        acc[k * c] = true;
                        if c == 0 {
                            break;
                        }
                        k += 1;
                    }
                } else {
                    for (_, p) in &self.powers(&sub, r.min as usize, r.max.map(|m| m as usize)) {
                        union(&mut acc, p);
                    }
                }
                acc
            }
        };
        self.memo.insert(key, l.clone());
        l
    }

    /// (k, lengths of k copies) for the useful repetition counts k in [min, max].
    fn powers(&self, sub: &Lens, min: usize, max: Option<usize>) -> Vec<(usize, Lens)> {
        let cap = self.cap;
        let mut cur = single(0, cap);
        let mut out = Vec::new();
        let hi = max.unwrap_or(min + cap + 1).min(min + cap + 1);
        for k in 0..=hi {
            if k >= min {
                out.push((k, cur.clone()));
            }
            if k == hi {
                break;
            }
            let next = conv(&cur, sub);
            if next == cur && k >= min {
                break;
            }
            cur = next;
            if !cur.iter().any(|&b| b) {
                break;
            }
        }
        out
    }

    fn pick<T: Clone>(&mut self, xs: &[T]) -> T {
        xs[(self.rand)(xs.len())].clone()
    }

    fn emit(&mut self, h: &Hir, n: usize, out: &mut String) {
        match h.kind() {
            HirKind::Empty | HirKind::Look(_) => {}
            HirKind::Literal(lit) => out.push_str(&String::from_utf8_lossy(&lit.0)),
            HirKind::Class(c) => out.push(self.class_char(c)),
            HirKind::Capture(c) => self.emit(&c.sub, n, out),
            HirKind::Concat(xs) => {
                let parts: Vec<Lens> = xs.iter().map(|x| self.lens(x)).collect();
                self.split_emit(xs.iter().collect(), &parts, n, out);
            }
            HirKind::Alternation(xs) => {
                let ok: Vec<&Hir> = xs.iter().filter(|x| self.lens(x)[n]).collect();
                let x = self.pick(&ok);
                self.emit(x, n, out);
            }
            HirKind::Repetition(r) => {
                let sub = self.lens(&r.sub);
                if let Some(c) = fixed(&sub) {
                    let k = n.checked_div(c).unwrap_or(r.min as usize);
                    for _ in 0..k {
                        self.emit(&r.sub, c, out);
                    }
                    return;
                }
                let ks: Vec<usize> = self.powers(&sub, r.min as usize, r.max.map(|m| m as usize)).into_iter().filter(|(_, p)| p[n]).map(|(k, _)| k).collect();
                let k = self.pick(&ks);
                let copies: Vec<&Hir> = (0..k).map(|_| &*r.sub).collect();
                let parts = vec![sub; k];
                self.split_emit(copies, &parts, n, out);
            }
        }
    }

    /// Emits the nodes in order with total length `n`, choosing each part's length among
    /// those that leave a feasible remainder.
    fn split_emit(&mut self, nodes: Vec<&Hir>, parts: &[Lens], n: usize, out: &mut String) {
        let cap = self.cap;
        // suffix[i] = lengths the nodes i.. can make together
        let mut suffix = vec![single(0, cap)];
        for p in parts.iter().rev() {
            let next = conv(p, suffix.last().unwrap());
            suffix.push(next);
        }
        suffix.reverse();
        let mut left = n;
        for (i, node) in nodes.iter().enumerate() {
            let choices: Vec<usize> = (0..=left).filter(|&l| parts[i][l] && suffix[i + 1][left - l]).collect();
            let l = self.pick(&choices);
            self.emit(node, l, out);
            left -= l;
        }
    }

    fn class_char(&mut self, c: &Class) -> char {
        let ranges: Vec<(char, char)> = match c {
            Class::Unicode(u) => u.ranges().iter().map(|r| (r.start(), r.end())).collect(),
            Class::Bytes(b) => b.ranges().iter().map(|r| (r.start() as char, r.end() as char)).collect(),
        };
        let total: usize = ranges.iter().map(|(a, b)| *b as usize - *a as usize + 1).sum();
        // wide classes draw from their printable ASCII part
        if total > 256 {
            let ascii: Vec<char> = (' '..='~').filter(|ch| ranges.iter().any(|(a, b)| a <= ch && ch <= b)).collect();
            if !ascii.is_empty() {
                return self.pick(&ascii);
            }
        }
        let mut k = (self.rand)(total);
        for (a, b) in ranges {
            let size = b as usize - a as usize + 1;
            if k < size {
                return char::from_u32(a as u32 + k as u32).unwrap_or(a);
            }
            k -= size;
        }
        '?'
    }
}

fn class_size(c: &Class) -> usize {
    match c {
        Class::Unicode(u) => u.ranges().iter().map(|r| r.end() as usize - r.start() as usize + 1).sum(),
        Class::Bytes(b) => b.ranges().iter().map(|r| r.end() as usize - r.start() as usize + 1).sum(),
    }
}

fn parse(pattern: &str) -> Result<Hir, String> {
    regex_syntax::ParserBuilder::new().build().parse(pattern).map_err(|e| format!("invalid regular expression: {}", e))
}

/// A random string matching all of `pattern` with a length in `[min, max]` (characters); without
/// bounds any length up to the pattern's minimum + 16. A pattern that matches exactly one
/// character is repeated to the requested length (`"[a-z0-9]"` with 8 gives 8 characters).
pub fn generate(pattern: &str, bounds: Option<(usize, usize)>, rand: &mut dyn FnMut(usize) -> usize) -> Result<String, String> {
    let mut hir = parse(pattern)?;
    let cap = match bounds {
        Some((_, hi)) => hi,
        None => min_len(&hir).saturating_add(DEFAULT_GROWTH),
    };
    if cap > 1 << 20 {
        return Err(format!("random strings are limited to {} characters", 1 << 20));
    }
    let mut g = Gen { cap, memo: HashMap::new(), rand };
    let mut lens = g.lens(&hir);
    // one-character patterns repeat
    if bounds.is_some() && lens.iter().enumerate().all(|(i, &b)| b == (i == 1)) {
        hir = parse(&format!("(?:{})*", pattern))?;
        g.memo.clear();
        lens = g.lens(&hir);
    }
    let (lo, hi) = match bounds {
        Some((a, b)) => (a, b.min(cap)),
        None => (0, cap),
    };
    let feasible: Vec<usize> = (lo..=hi.min(cap)).filter(|&n| lens[n]).collect();
    if feasible.is_empty() {
        return Err(match bounds {
            Some((a, b)) if a == b => format!("the pattern cannot produce a string of length {}", a),
            Some((a, b)) => format!("the pattern cannot produce a string of length {}..{}", a, b),
            None => "the pattern cannot match any string".into(),
        });
    }
    let n = g.pick(&feasible);
    let mut out = String::new();
    // re-borrow: emit needs the memo of the final pattern
    g.emit(&hir, n, &mut out);
    Ok(out)
}

/// Compile-time check of a generator pattern.
pub fn check(pattern: &str) -> Result<(), String> {
    parse(pattern).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng() -> impl FnMut(usize) -> usize {
        let mut s: u64 = 12345;
        move |n| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % n as u64) as usize
        }
    }

    #[test]
    fn exact_lengths() {
        let mut r = rng();
        for (p, n) in [("[a-z0-9]", 12), (r"\d{3}-\d{4}", 8), ("(ab|cde)+", 7), ("x[0-9]*y", 5), ("가[나-다]{2}", 3)] {
            let s = generate(p, Some((n, n)), &mut r).unwrap();
            assert_eq!(s.chars().count(), n, "{} -> {}", p, s);
            assert!(regex::Regex::new(&format!("^(?:{})$", if p == "[a-z0-9]" { "[a-z0-9]*" } else { p })).unwrap().is_match(&s), "{} -> {}", p, s);
        }
        assert!(generate("(ab|cde)+", Some((1, 1)), &mut r).is_err());
        let free = generate(r"[A-Z]{2}\d+", None, &mut r).unwrap();
        assert!(regex::Regex::new(r"^[A-Z]{2}\d+$").unwrap().is_match(&free));
        assert!(generate(r"\w", Some((50, 50)), &mut r).unwrap().chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }
}
