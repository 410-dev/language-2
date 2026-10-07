//! Text helpers for editor tooling: names in source lines, documentation comments, block
//! extents and UTF-16 columns (the unit of LSP positions).

pub fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Skips a string literal starting at `i` (`"..."`, `f"..."`, `r"..."`, `r#"..."#`); returns the
/// index after it and whether it is closed on this line, or `None` when no string starts at `i`.
fn skip_string(line: &[char], i: usize) -> Option<(usize, bool)> {
    let at = |k: usize| line.get(k).copied();
    let prev_ident = i > 0 && is_ident_char(line[i - 1]);
    let (mut j, raw, hashes) = match at(i)? {
        '"' => (i + 1, false, 0),
        'f' if !prev_ident && at(i + 1) == Some('"') => (i + 2, false, 0),
        'r' if !prev_ident => {
            let mut k = i + 1;
            while at(k) == Some('#') {
                k += 1;
            }
            if at(k) != Some('"') {
                return None;
            }
            (k + 1, true, k - i - 1)
        }
        _ => return None,
    };
    while j < line.len() {
        if !raw && line[j] == '\\' {
            j += 2;
            continue;
        }
        if line[j] == '"' && (0..hashes).all(|h| at(j + 1 + h) == Some('#')) {
            return Some((j + 1 + hashes, true));
        }
        j += 1;
    }
    Some((line.len(), false))
}

/// Index of the first occurrence of the identifier `name` at or after `from`, outside string
/// literals and comments.
pub fn find_word(line: &[char], from: usize, name: &str) -> Option<usize> {
    let name: Vec<char> = name.chars().collect();
    if name.is_empty() {
        return None;
    }
    let mut i = from.min(line.len());
    while i < line.len() {
        if line[i] == '/' && line.get(i + 1) == Some(&'/') {
            return None;
        }
        if let Some((j, _)) = skip_string(line, i) {
            i = j;
            continue;
        }
        if is_ident_char(line[i]) {
            let start = i;
            while i < line.len() && is_ident_char(line[i]) {
                i += 1;
            }
            if line[start..i] == name[..] {
                return Some(start);
            }
            continue;
        }
        i += 1;
    }
    None
}

/// The identifier around character index `i` of `line`: (start, end).
pub fn word_at(line: &[char], i: usize) -> Option<(usize, usize)> {
    let mut s = i.min(line.len());
    let mut e = s;
    while s > 0 && is_ident_char(line[s - 1]) {
        s -= 1;
    }
    while e < line.len() && is_ident_char(line[e]) {
        e += 1;
    }
    (s < e).then_some((s, e))
}

#[derive(Clone, Copy)]
enum Lex {
    Code,
    Str { raw: bool, hashes: usize, fmt: bool },
    Comment,
}

/// For each cursor position `0..=len` of `line`: whether it is in code. String literals and
/// `//` comments are not code; the `{...}` fields of f-strings are.
pub fn code_positions(line: &[char]) -> Vec<bool> {
    let n = line.len();
    let mut out = vec![true; n + 1];
    // contexts: the innermost last; an f-string field remembers its brace depth
    let mut stack: Vec<(Lex, i32)> = vec![(Lex::Code, 0)];
    let mut i = 0;
    while i < n {
        let c = line[i];
        let (ctx, depth) = *stack.last().unwrap();
        let mut next = i + 1;
        match ctx {
            Lex::Comment => {}
            Lex::Code => match c {
                '/' if line.get(i + 1) == Some(&'/') => {
                    stack.push((Lex::Comment, 0));
                }
                '"' => {
                    let prev = |k: usize| if k <= i && k > 0 { Some(line[i - k]) } else { None };
                    let ident_before = |k: usize| i > k && is_ident_char(line[i - k - 1]);
                    let mut hashes = 0;
                    while prev(hashes + 1) == Some('#') {
                        hashes += 1;
                    }
                    let raw = prev(hashes + 1) == Some('r') && !ident_before(hashes + 1);
                    let fmt = hashes == 0 && prev(1) == Some('f') && !ident_before(1);
                    stack.push((Lex::Str { raw, hashes: if raw { hashes } else { 0 }, fmt }, 0));
                }
                '{' if stack.len() > 1 => stack.last_mut().unwrap().1 += 1,
                '}' if stack.len() > 1 && depth == 0 => {
                    // end of an f-string field
                    stack.pop();
                }
                '}' if stack.len() > 1 => stack.last_mut().unwrap().1 -= 1,
                _ => {}
            },
            Lex::Str { raw, hashes, fmt } => match c {
                '\\' if !raw => next = i + 2,
                '"' if (0..hashes).all(|h| line.get(i + 1 + h) == Some(&'#')) => {
                    stack.pop();
                    next = i + 1 + hashes;
                }
                '{' if fmt && line.get(i + 1) == Some(&'{') => next = i + 2,
                '{' if fmt => stack.push((Lex::Code, 0)),
                _ => {}
            },
        }
        let code = matches!(stack.last().unwrap().0, Lex::Code);
        for slot in out.iter_mut().take(next.min(n) + 1).skip(i + 1) {
            *slot = code;
        }
        i = next;
    }
    out
}

/// Whether cursor position `i` of `line` is inside a string literal or a `//` comment.
pub fn in_string_or_comment(line: &[char], i: usize) -> bool {
    !code_positions(line)[i.min(line.len())]
}

fn comment_text(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let rest = t.strip_prefix("//")?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn is_rule(text: &str) -> bool {
    text.contains("----") || text.contains("====")
}

/// The `//` comment block directly above line `line` (1-based), skipping annotations. Section
/// rules (`// ----- name`) end the block.
pub fn doc_comment(src: &str, line: u32) -> Option<String> {
    let lines: Vec<&str> = src.lines().collect();
    let mut i = line as usize;
    if i == 0 || i > lines.len() {
        return None;
    }
    i -= 1; // index of the declaration line
    let mut out: Vec<&str> = Vec::new();
    while i > 0 {
        i -= 1;
        let l = lines[i];
        if l.trim_start().starts_with('@') && !l.trim_start().starts_with("@using") {
            continue;
        }
        match comment_text(l) {
            Some(t) if !is_rule(t) => out.push(t),
            _ => break,
        }
    }
    if out.is_empty() {
        return None;
    }
    out.reverse();
    Some(join_comment(&out))
}

/// The comment block at the top of a file (after directives), describing its module.
pub fn header_comment(src: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for l in src.lines() {
        let t = l.trim();
        if out.is_empty() && (t.is_empty() || t.starts_with('@')) {
            continue;
        }
        match comment_text(l) {
            Some(c) => out.push(c),
            None => break,
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(join_comment(&out))
    }
}

/// Comment lines as Markdown: indented lines (code examples) become a code block.
fn join_comment(lines: &[&str]) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for l in lines {
        let code = l.starts_with("    ") && !l.trim().is_empty();
        if code && !in_code {
            out.push_str("```l2\n");
            in_code = true;
        } else if !code && in_code && !l.trim().is_empty() {
            out.push_str("```\n");
            in_code = false;
        }
        if in_code {
            out.push_str(l.strip_prefix("    ").unwrap_or(l));
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    if in_code {
        out.push_str("```\n");
    }
    out.trim_end().to_string()
}

/// The 0-based (line, character) just past the `}` that closes the first `{` at or after
/// 1-based (line, col); `None` when there is no complete block.
pub fn block_end(src: &str, line: u32, col: u32) -> Option<(u32, u32)> {
    let lines: Vec<Vec<char>> = src.lines().map(|l| l.chars().collect()).collect();
    let mut depth = 0i32;
    let mut started = false;
    let mut li = line.checked_sub(1)? as usize;
    let mut ci = col.saturating_sub(1) as usize;
    let mut block_comment = false;
    while li < lines.len() {
        let l = &lines[li];
        while ci < l.len() {
            if block_comment {
                if l[ci] == '*' && l.get(ci + 1) == Some(&'/') {
                    block_comment = false;
                    ci += 2;
                } else {
                    ci += 1;
                }
                continue;
            }
            if l[ci] == '/' && l.get(ci + 1) == Some(&'/') {
                break;
            }
            if l[ci] == '/' && l.get(ci + 1) == Some(&'*') {
                block_comment = true;
                ci += 2;
                continue;
            }
            if let Some((j, _)) = skip_string(l, ci) {
                ci = j;
                continue;
            }
            match l[ci] {
                '{' => {
                    depth += 1;
                    started = true;
                }
                '}' => {
                    depth -= 1;
                    if started && depth == 0 {
                        return Some((li as u32, ci as u32 + 1));
                    }
                }
                _ => {}
            }
            ci += 1;
        }
        li += 1;
        ci = 0;
    }
    None
}

/// UTF-16 offset of character index `chars` in `line`.
pub fn utf16_of(line: &str, chars: usize) -> u32 {
    line.chars().take(chars).map(|c| c.len_utf16() as u32).sum()
}

/// Character index of UTF-16 offset `units` in `line`.
pub fn chars_of(line: &str, units: u32) -> usize {
    let mut n = 0u32;
    for (i, c) in line.chars().enumerate() {
        if n >= units {
            return i;
        }
        n += c.len_utf16() as u32;
    }
    line.chars().count()
}

/// Line `n` (0-based) of `src` without its line break.
pub fn line_of(src: &str, n: usize) -> &str {
    src.split('\n').nth(n).map(|l| l.trim_end_matches('\r')).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn finds_names_outside_strings() {
        let l = chars(r#"    private json["name", encode] String name = "name""#);
        assert_eq!(find_word(&l, 4, "name"), Some(40));
        let l = chars("Int64 count = count2 + count");
        assert_eq!(find_word(&l, 0, "count"), Some(6));
        assert_eq!(find_word(&l, 8, "count"), Some(23));
        assert_eq!(find_word(&chars(r##"r#"x"# x"##), 0, "x"), Some(7));
    }

    #[test]
    fn doc_comments() {
        let src = "// A point.\n//\n//     Point p = new Point()\n@Deprecated\nclass Point {\n    // ---- section\n    // x coordinate\n    Int64 x\n}\n";
        assert_eq!(doc_comment(src, 5).unwrap(), "A point.\n\n```l2\nPoint p = new Point()\n```");
        assert_eq!(doc_comment(src, 8).unwrap(), "x coordinate");
        assert_eq!(header_comment("@using sdk 1\n\n// io.File: files\n// more\nusing x\n").unwrap(), "io.File: files\nmore");
    }

    #[test]
    fn blocks_and_columns() {
        let src = "class A {\n    function void f() { \"}\" }\n}\n";
        assert_eq!(block_end(src, 1, 1), Some((2, 1)));
        assert_eq!(block_end(src, 2, 5), Some((1, 29)));
        assert_eq!(utf16_of("a😀b", 2), 3);
        assert_eq!(chars_of("a😀b", 3), 2);
        assert!(in_string_or_comment(&chars("x = \"ab\" // c"), 6));
        assert!(!in_string_or_comment(&chars("x = \"ab\" + c"), 11));
        assert!(in_string_or_comment(&chars("x = 1 // c"), 9));
        // f-string fields are code, the text around them is not
        let l = chars(r#"f"a {p.x + d["k"]} b {{c}}" + y"#);
        let code: String = code_positions(&l).iter().map(|c| if *c { 'c' } else { '.' }).collect();
        assert_eq!(code, "cc...ccccccccc..cc.........ccccc");
        assert!(in_string_or_comment(&chars(r##"r#"a"b"# + x"##), 5));
        assert!(!in_string_or_comment(&chars(r##"r#"a"b"# + x"##), 9));
        assert!(in_string_or_comment(&chars(r#""unterminated"#), 5));
    }
}
