//! Regular expressions (spec 14.15): the linear-time `regex` engine, or `fancy-regex` for
//! back-references and look-around. Positions are character indices, like String methods.

use super::{closed, handle, int_array, str_array, Args, Resource, SysOp};
use crate::value::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub enum Compiled {
    Std(regex::Regex),
    Fancy(fancy_regex::Regex),
}

/// Group spans of one match as byte ranges.
type Groups = Vec<Option<(usize, usize)>>;

impl Compiled {
    pub fn new(pattern: &str, flags: &str, fancy: bool) -> Result<Compiled, String> {
        for f in flags.chars() {
            if !"imsx".contains(f) {
                return Err(format!("unknown regex flag '{}' (expected i, m, s or x)", f));
            }
        }
        if fancy {
            let p = if flags.is_empty() { pattern.to_string() } else { format!("(?{}){}", flags, pattern) };
            fancy_regex::Regex::new(&p).map(Compiled::Fancy).map_err(|e| format!("invalid regular expression: {}", e))
        } else {
            regex::RegexBuilder::new(pattern)
                .case_insensitive(flags.contains('i'))
                .multi_line(flags.contains('m'))
                .dot_matches_new_line(flags.contains('s'))
                .ignore_whitespace(flags.contains('x'))
                .build()
                .map(Compiled::Std)
                .map_err(|e| {
                    let msg = e.to_string();
                    if msg.contains("look-around") || msg.contains("backreferences") {
                        format!("invalid regular expression: {} (use Regex.fancy(...) for back-references and look-around)", msg.trim())
                    } else {
                        format!("invalid regular expression: {}", msg.trim())
                    }
                })
        }
    }

    pub fn group_count(&self) -> usize {
        match self {
            Compiled::Std(r) => r.captures_len(),
            Compiled::Fancy(r) => r.captures_len(),
        }
    }

    pub fn group_names(&self) -> Vec<String> {
        match self {
            Compiled::Std(r) => r.capture_names().map(|n| n.unwrap_or("").to_string()).collect(),
            Compiled::Fancy(r) => r.capture_names().map(|n| n.unwrap_or("").to_string()).collect(),
        }
    }

    /// The first match starting at byte `pos` or later.
    pub fn captures_at(&self, text: &str, pos: usize) -> Result<Option<Groups>, String> {
        Ok(match self {
            Compiled::Std(r) => r.captures_at(text, pos).map(|c| (0..c.len()).map(|i| c.get(i).map(|m| (m.start(), m.end()))).collect()),
            Compiled::Fancy(r) => r.captures_from_pos(text, pos).map_err(|e| e.to_string())?.map(|c| (0..c.len()).map(|i| c.get(i).map(|m| (m.start(), m.end()))).collect()),
        })
    }

    /// All non-overlapping matches (an empty match never repeats at the same place).
    pub fn all(&self, text: &str, limit: usize) -> Result<Vec<Groups>, String> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos <= text.len() && (limit == 0 || out.len() < limit) {
            let Some(g) = self.captures_at(text, pos)? else { break };
            let (s, e) = g[0].unwrap_or((pos, pos));
            pos = if e > s {
                e
            } else {
                match text[e..].chars().next() {
                    Some(c) => e + c.len_utf8(),
                    None => text.len() + 1,
                }
            };
            out.push(g);
        }
        Ok(out)
    }

    pub fn replace(&self, text: &str, rep: &str, limit: usize) -> Result<String, String> {
        Ok(match self {
            Compiled::Std(r) => r.replacen(text, limit, rep).into_owned(),
            Compiled::Fancy(r) => r.try_replacen(text, limit, rep).map_err(|e| e.to_string())?.into_owned(),
        })
    }
}

/// Character index of every byte offset (offsets are always on character boundaries).
struct CharIndex(Vec<usize>);

impl CharIndex {
    fn new(text: &str) -> CharIndex {
        CharIndex(text.char_indices().map(|(b, _)| b).collect())
    }
    fn of(&self, byte: usize) -> i64 {
        self.0.partition_point(|&b| b < byte) as i64
    }
    fn byte(&self, ch: i64, len: usize) -> usize {
        if ch <= 0 {
            0
        } else {
            self.0.get(ch as usize).copied().unwrap_or(len)
        }
    }
}

fn flatten(groups: &[Groups], idx: &CharIndex) -> Vec<i64> {
    let mut out = Vec::new();
    for g in groups {
        for span in g {
            match span {
                Some((s, e)) => {
                    out.push(idx.of(*s));
                    out.push(idx.of(*e));
                }
                None => {
                    out.push(-1);
                    out.push(-1);
                }
            }
        }
    }
    out
}

thread_local! {
    static CACHE: RefCell<HashMap<String, Rc<Compiled>>> = RefCell::new(HashMap::new());
}

/// A compiled pattern for the String methods, cached by its text.
pub fn cached(pattern: &str) -> Result<Rc<Compiled>, String> {
    if let Some(c) = CACHE.with(|c| c.borrow().get(pattern).cloned()) {
        return Ok(c);
    }
    let c = Rc::new(Compiled::new(pattern, "", false)?);
    CACHE.with(|cache| {
        let mut m = cache.borrow_mut();
        if m.len() >= 128 {
            m.clear();
        }
        m.insert(pattern.to_string(), c.clone());
    });
    Ok(c)
}

/// Compile-time check of a literal pattern.
pub fn check_pattern(pattern: &str, flags: &str, fancy: bool) -> Result<(), String> {
    Compiled::new(pattern, flags, fancy).map(|_| ())
}

fn with_regex<H: Host, R>(a: &Args, h: &mut H, f: impl FnOnce(&Compiled, &mut H) -> Result<R, H::Err>) -> Result<R, H::Err> {
    let x = a.handle(0, h)?;
    let r = x.res.borrow();
    match &*r {
        Resource::Regex(c) => f(c, h),
        _ => Err(closed("regex", h)),
    }
}

pub(super) fn call<H: Host>(op: SysOp, a: &Args, h: &mut H) -> Result<Value, H::Err> {
    use SysOp::*;
    let ill = |h: &mut H, e: String| h.throw(ExcKind::IllegalArgument, e);
    Ok(match op {
        regexNew => {
            let c = Compiled::new(&a.str(0), &a.str(1), a.bool(2)).map_err(|e| ill(h, e))?;
            handle("Regex", Resource::Regex(Box::new(c)))
        }
        regexGroupNames => with_regex(a, h, |c, _| Ok(str_array(c.group_names())))?,
        regexFind => {
            let text = a.str(1);
            let idx = CharIndex::new(&text);
            let from = idx.byte(a.int(2), text.len());
            with_regex(a, h, |c, h| {
                let g = c.captures_at(&text, from).map_err(|e| ill(h, e))?;
                Ok(int_array(g.map(|g| flatten(&[g], &idx)).unwrap_or_default()))
            })?
        }
        regexFindAll => {
            let text = a.str(1);
            let idx = CharIndex::new(&text);
            with_regex(a, h, |c, h| {
                let all = c.all(&text, 0).map_err(|e| ill(h, e))?;
                Ok(int_array(flatten(&all, &idx)))
            })?
        }
        regexReplace => {
            let (text, rep) = (a.str(1), a.str(2));
            let limit = a.int(3).max(0) as usize;
            Value::str(with_regex(a, h, |c, h| c.replace(&text, &rep, limit).map_err(|e| ill(h, e)))?)
        }
        regexSplit => {
            let text = a.str(1);
            let limit = a.int(2).max(0) as usize;
            let parts = with_regex(a, h, |c, h| split(c, &text, limit).map_err(|e| ill(h, e)))?;
            str_array(parts)
        }
        regexEscape => Value::str(regex::escape(&a.str(0))),
        regexCheck => match check_pattern(&a.str(0), "", a.bool(1)) {
            Ok(()) => Value::Null,
            Err(e) => Value::str(e),
        },
        strMatches => {
            let c = cached(&format!("^(?:{})$", a.str(1))).map_err(|e| ill(h, e))?;
            Value::Bool(c.captures_at(&a.str(0), 0).map_err(|e| ill(h, e))?.is_some())
        }
        strContainsMatch => {
            let c = cached(&a.str(1)).map_err(|e| ill(h, e))?;
            Value::Bool(c.captures_at(&a.str(0), 0).map_err(|e| ill(h, e))?.is_some())
        }
        strReplaceRegex => {
            let c = cached(&a.str(1)).map_err(|e| ill(h, e))?;
            Value::str(c.replace(&a.str(0), &a.str(2), 0).map_err(|e| ill(h, e))?)
        }
        strSplitRegex => {
            let c = cached(&a.str(1)).map_err(|e| ill(h, e))?;
            str_array(split(&c, &a.str(0), 0).map_err(|e| ill(h, e))?)
        }
        strFindAllRegex => {
            let text = a.str(0);
            let c = cached(&a.str(1)).map_err(|e| ill(h, e))?;
            let all = c.all(&text, 0).map_err(|e| ill(h, e))?;
            str_array(all.iter().filter_map(|g| g[0].map(|(s, e)| text[s..e].to_string())).collect())
        }
        _ => unreachable!(),
    })
}

/// Text between matches; `limit` > 0 gives at most `limit` parts.
fn split(c: &Compiled, text: &str, limit: usize) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut last = 0;
    let matches = c.all(text, if limit > 0 { limit - 1 } else { 0 })?;
    for g in matches {
        let (s, e) = g[0].unwrap_or((0, 0));
        if e == 0 && s == 0 {
            continue;
        }
        if s == e && s >= text.len() {
            break;
        }
        out.push(text[last..s].to_string());
        last = e;
    }
    out.push(text[last..].to_string());
    Ok(out)
}
