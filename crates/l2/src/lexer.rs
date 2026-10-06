//! Lexer. Produces a token stream with explicit `Newline` tokens acting as statement terminators
//! (spec 3.2): newlines are dropped inside `(`/`[`, after a binary operator or comma, and
//! repeated newlines collapse into one.

use crate::diag::{Diag, Span};

#[derive(Clone, Debug, PartialEq)]
pub enum FPart {
    Lit(String),
    Expr(String, Span),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(u128),
    Float(f64),
    Str(String),
    FStr(Vec<FPart>),
    At(String),
    // keywords
    Function,
    Class,
    Interface,
    Extends,
    Implements,
    Public,
    Private,
    Protected,
    Static,
    Immutable,
    Copied,
    Return,
    If,
    Else,
    For,
    In,
    Break,
    Continue,
    Switch,
    Case,
    Default,
    Fallthrough,
    Try,
    Catch,
    Finally,
    Throw,
    Throws,
    True,
    False,
    Null,
    This,
    Super,
    And,
    Or,
    Not,
    Using,
    As,
    Void,
    // punctuation
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,
    Colon,
    Semi,
    Arrow,
    Question,
    QQ,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    Amp,
    AmpAmp,
    Pipe,
    PipePipe,
    Caret,
    Tilde,
    Bang,
    Shl,
    Shr,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    Ne,
    Assign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    StarStarEq,
    Newline,
    Eof,
}

impl Tok {
    /// Tokens after which a newline does not end the statement.
    fn continues_line(&self) -> bool {
        use Tok::*;
        matches!(
            self,
            Plus | Minus
                | Star
                | Slash
                | Percent
                | StarStar
                | AmpAmp
                | PipePipe
                | Caret
                | Shl
                | Shr
                | Lt
                | Gt
                | Le
                | Ge
                | EqEq
                | Ne
                | Assign
                | PlusEq
                | MinusEq
                | StarEq
                | SlashEq
                | PercentEq
                | StarStarEq
                | And
                | Or
                | Comma
                | QQ
                | Question
                | Arrow
                | Dot
                | Pipe
                | LParen
                | LBracket
                | LBrace
                | Newline
        )
    }

    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("identifier '{}'", s),
            Tok::Int(v) => format!("integer {}", v),
            Tok::Float(v) => format!("number {}", v),
            Tok::Str(_) => "string literal".into(),
            Tok::FStr(_) => "f-string".into(),
            Tok::At(s) => format!("'@{}'", s),
            Tok::Newline => "end of line".into(),
            Tok::Eof => "end of file".into(),
            other => format!("'{}'", tok_text(other)),
        }
    }
}

pub fn tok_text(t: &Tok) -> &'static str {
    use Tok::*;
    match t {
        Function => "function",
        Class => "class",
        Interface => "interface",
        Extends => "extends",
        Implements => "implements",
        Public => "public",
        Private => "private",
        Protected => "protected",
        Static => "static",
        Immutable => "Immutable",
        Copied => "copied",
        Return => "return",
        If => "if",
        Else => "else",
        For => "for",
        In => "in",
        Break => "break",
        Continue => "continue",
        Switch => "switch",
        Case => "case",
        Default => "default",
        Fallthrough => "fallthrough",
        Try => "try",
        Catch => "catch",
        Finally => "finally",
        Throw => "throw",
        Throws => "throws",
        True => "true",
        False => "false",
        Null => "null",
        This => "this",
        Super => "super",
        And => "and",
        Or => "or",
        Not => "not",
        Using => "using",
        As => "as",
        Void => "void",
        LParen => "(",
        RParen => ")",
        LBracket => "[",
        RBracket => "]",
        LBrace => "{",
        RBrace => "}",
        Comma => ",",
        Dot => ".",
        Colon => ":",
        Semi => ";",
        Arrow => "->",
        Question => "?",
        QQ => "??",
        Plus => "+",
        Minus => "-",
        Star => "*",
        Slash => "/",
        Percent => "%",
        StarStar => "**",
        Amp => "&",
        AmpAmp => "&&",
        Pipe => "|",
        PipePipe => "||",
        Caret => "^",
        Tilde => "~",
        Bang => "!",
        Shl => "<<",
        Shr => ">>",
        Lt => "<",
        Gt => ">",
        Le => "<=",
        Ge => ">=",
        EqEq => "==",
        Ne => "!=",
        Assign => "=",
        PlusEq => "+=",
        MinusEq => "-=",
        StarEq => "*=",
        SlashEq => "/=",
        PercentEq => "%=",
        StarStarEq => "**=",
        _ => "?",
    }
}

fn keyword(s: &str) -> Option<Tok> {
    use Tok::*;
    Some(match s {
        "function" => Function,
        "class" => Class,
        "interface" => Interface,
        "extends" => Extends,
        "implements" => Implements,
        "public" => Public,
        "private" => Private,
        "protected" => Protected,
        "static" => Static,
        "Immutable" => Immutable,
        "copied" => Copied,
        "return" => Return,
        "if" => If,
        "else" => Else,
        "for" => For,
        "in" => In,
        "break" => Break,
        "continue" => Continue,
        "switch" => Switch,
        "case" => Case,
        "default" => Default,
        "fallthrough" => Fallthrough,
        "try" => Try,
        "catch" => Catch,
        "finally" => Finally,
        "throw" => Throw,
        "throws" => Throws,
        "true" => True,
        "false" => False,
        "null" | "Null" => Null,
        "this" => This,
        "super" => Super,
        "and" => And,
        "or" => Or,
        "not" => Not,
        "using" => Using,
        "as" => As,
        "void" => Void,
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub struct Lexer<'a> {
    chars: Vec<char>,
    pos: usize,
    line: u32,
    col: u32,
    file: u32,
    out: Vec<Token>,
    depth: Vec<char>,
    _src: &'a str,
}

pub fn lex(src: &str, file: u32) -> Result<Vec<Token>, Diag> {
    lex_at(src, file, 1, 1)
}

/// Lexes `src` as if it started at the given position (used for f-string expressions).
pub fn lex_at(src: &str, file: u32, line: u32, col: u32) -> Result<Vec<Token>, Diag> {
    let mut lx = Lexer { chars: src.chars().collect(), pos: 0, line, col, file, out: Vec::new(), depth: Vec::new(), _src: src };
    lx.run()?;
    Ok(lx.out)
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> char {
        self.chars.get(self.pos).copied().unwrap_or('\0')
    }
    fn peek_at(&self, n: usize) -> char {
        self.chars.get(self.pos + n).copied().unwrap_or('\0')
    }
    fn bump(&mut self) -> char {
        let c = self.peek();
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        c
    }
    fn span(&self) -> Span {
        Span::new(self.file, self.line, self.col)
    }
    fn err(&self, msg: impl Into<String>) -> Diag {
        Diag::error(self.span(), msg)
    }

    fn push(&mut self, tok: Tok, span: Span) {
        match &tok {
            Tok::LParen => self.depth.push('('),
            Tok::LBracket => self.depth.push('['),
            Tok::LBrace => self.depth.push('{'),
            Tok::RParen | Tok::RBracket | Tok::RBrace => {
                self.depth.pop();
            }
            _ => {}
        }
        self.out.push(Token { tok, span });
    }

    fn newline(&mut self, span: Span) {
        if matches!(self.depth.last(), Some('(') | Some('[')) {
            return;
        }
        match self.out.last() {
            None => {}
            Some(t) if t.tok.continues_line() => {}
            _ => self.out.push(Token { tok: Tok::Newline, span }),
        }
    }

    fn run(&mut self) -> Result<(), Diag> {
        loop {
            let c = self.peek();
            if c == '\0' && self.pos >= self.chars.len() {
                break;
            }
            let span = self.span();
            match c {
                '\n' => {
                    self.bump();
                    self.newline(span);
                }
                ' ' | '\t' | '\r' | '\u{feff}' => {
                    self.bump();
                }
                '/' if self.peek_at(1) == '/' => {
                    while self.peek() != '\n' && self.pos < self.chars.len() {
                        self.bump();
                    }
                }
                '/' if self.peek_at(1) == '*' => {
                    self.bump();
                    self.bump();
                    let mut had_newline = false;
                    loop {
                        if self.pos >= self.chars.len() {
                            return Err(Diag::error(span, "unterminated block comment"));
                        }
                        if self.peek() == '*' && self.peek_at(1) == '/' {
                            self.bump();
                            self.bump();
                            break;
                        }
                        if self.bump() == '\n' {
                            had_newline = true;
                        }
                    }
                    if had_newline {
                        self.newline(span);
                    }
                }
                '"' => {
                    let s = self.string()?;
                    self.push(Tok::Str(s), span);
                }
                'f' if self.peek_at(1) == '"' => {
                    self.bump();
                    let parts = self.fstring()?;
                    self.push(Tok::FStr(parts), span);
                }
                c if c.is_ascii_digit() => {
                    let t = self.number()?;
                    self.push(t, span);
                }
                c if c.is_alphabetic() || c == '_' => {
                    let mut s = String::new();
                    while self.peek().is_alphanumeric() || self.peek() == '_' {
                        s.push(self.bump());
                    }
                    let t = keyword(&s).unwrap_or(Tok::Ident(s));
                    self.push(t, span);
                }
                '@' => {
                    self.bump();
                    let mut s = String::new();
                    while self.peek().is_alphanumeric() || self.peek() == '_' {
                        s.push(self.bump());
                    }
                    if s.is_empty() {
                        return Err(Diag::error(span, "expected a name after '@'"));
                    }
                    self.push(Tok::At(s), span);
                }
                _ => {
                    let t = self.punct()?;
                    self.push(t, span);
                }
            }
        }
        let span = self.span();
        self.newline(span);
        self.out.push(Token { tok: Tok::Eof, span });
        Ok(())
    }

    fn escape(&mut self) -> Result<char, Diag> {
        let c = self.bump();
        Ok(match c {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '0' => '\0',
            '\\' => '\\',
            '"' => '"',
            '\'' => '\'',
            '{' => '{',
            '}' => '}',
            '%' => '%',
            'u' => {
                if self.peek() != '{' {
                    return Err(self.err("expected '{' after \\u"));
                }
                self.bump();
                let mut h = String::new();
                while self.peek() != '}' {
                    if self.pos >= self.chars.len() {
                        return Err(self.err("unterminated unicode escape"));
                    }
                    h.push(self.bump());
                }
                self.bump();
                let v = u32::from_str_radix(&h, 16).map_err(|_| self.err("invalid unicode escape"))?;
                char::from_u32(v).ok_or_else(|| self.err("invalid unicode scalar"))?
            }
            other => return Err(self.err(format!("unknown escape sequence '\\{}'", other))),
        })
    }

    fn string(&mut self) -> Result<String, Diag> {
        let start = self.span();
        self.bump(); // opening quote
        let mut s = String::new();
        loop {
            match self.peek() {
                '"' => {
                    self.bump();
                    return Ok(s);
                }
                '\\' => {
                    self.bump();
                    s.push(self.escape()?);
                }
                '\n' | '\0' if self.pos >= self.chars.len() || self.peek() == '\n' => {
                    return Err(Diag::error(start, "unterminated string literal"));
                }
                _ => s.push(self.bump()),
            }
        }
    }

    fn fstring(&mut self) -> Result<Vec<FPart>, Diag> {
        let start = self.span();
        self.bump(); // opening quote
        let mut parts = Vec::new();
        let mut lit = String::new();
        loop {
            match self.peek() {
                '"' => {
                    self.bump();
                    if !lit.is_empty() {
                        parts.push(FPart::Lit(lit));
                    }
                    return Ok(parts);
                }
                '\\' => {
                    self.bump();
                    lit.push(self.escape()?);
                }
                '{' if self.peek_at(1) == '{' => {
                    self.bump();
                    self.bump();
                    lit.push('{');
                }
                '}' if self.peek_at(1) == '}' => {
                    self.bump();
                    self.bump();
                    lit.push('}');
                }
                '{' => {
                    self.bump();
                    if !lit.is_empty() {
                        parts.push(FPart::Lit(std::mem::take(&mut lit)));
                    }
                    let espan = self.span();
                    let mut depth = 0;
                    let mut e = String::new();
                    loop {
                        let c = self.peek();
                        if self.pos >= self.chars.len() || c == '\n' {
                            return Err(Diag::error(start, "unterminated f-string expression"));
                        }
                        if c == '}' && depth == 0 {
                            self.bump();
                            break;
                        }
                        if c == '{' {
                            depth += 1;
                        }
                        if c == '}' {
                            depth -= 1;
                        }
                        if c == '"' {
                            // nested string literal inside the expression
                            e.push(self.bump());
                            while self.peek() != '"' {
                                if self.pos >= self.chars.len() {
                                    return Err(Diag::error(start, "unterminated string in f-string"));
                                }
                                if self.peek() == '\\' {
                                    e.push(self.bump());
                                }
                                e.push(self.bump());
                            }
                            e.push(self.bump());
                            continue;
                        }
                        e.push(self.bump());
                    }
                    if e.trim().is_empty() {
                        return Err(Diag::error(espan, "empty expression in f-string"));
                    }
                    parts.push(FPart::Expr(e, espan));
                }
                _ if self.pos >= self.chars.len() || self.peek() == '\n' => {
                    return Err(Diag::error(start, "unterminated f-string"));
                }
                _ => lit.push(self.bump()),
            }
        }
    }

    fn number(&mut self) -> Result<Tok, Diag> {
        let start = self.span();
        if self.peek() == '0' && matches!(self.peek_at(1), 'x' | 'X' | 'b' | 'B' | 'o' | 'O') {
            self.bump();
            let radix = match self.bump() {
                'x' | 'X' => 16,
                'b' | 'B' => 2,
                _ => 8,
            };
            let mut s = String::new();
            while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
                let c = self.bump();
                if c != '_' {
                    s.push(c);
                }
            }
            return u128::from_str_radix(&s, radix).map(Tok::Int).map_err(|_| Diag::error(start, "invalid integer literal"));
        }
        let mut s = String::new();
        let mut is_float = false;
        while self.peek().is_ascii_digit() || self.peek() == '_' {
            let c = self.bump();
            if c != '_' {
                s.push(c);
            }
        }
        if self.peek() == '.' && self.peek_at(1).is_ascii_digit() {
            is_float = true;
            s.push(self.bump());
            while self.peek().is_ascii_digit() || self.peek() == '_' {
                let c = self.bump();
                if c != '_' {
                    s.push(c);
                }
            }
        }
        if matches!(self.peek(), 'e' | 'E')
            && (self.peek_at(1).is_ascii_digit() || (matches!(self.peek_at(1), '+' | '-') && self.peek_at(2).is_ascii_digit()))
        {
            is_float = true;
            s.push(self.bump());
            if matches!(self.peek(), '+' | '-') {
                s.push(self.bump());
            }
            while self.peek().is_ascii_digit() {
                s.push(self.bump());
            }
        }
        if is_float {
            s.parse::<f64>().map(Tok::Float).map_err(|_| Diag::error(start, "invalid number literal"))
        } else {
            s.parse::<u128>().map(Tok::Int).map_err(|_| Diag::error(start, "integer literal too large"))
        }
    }

    fn punct(&mut self) -> Result<Tok, Diag> {
        use Tok::*;
        let c = self.bump();
        let n = self.peek();
        let two = |lx: &mut Self, t: Tok| {
            lx.bump();
            t
        };
        Ok(match c {
            '(' => LParen,
            ')' => RParen,
            '[' => LBracket,
            ']' => RBracket,
            '{' => LBrace,
            '}' => RBrace,
            ',' => Comma,
            '.' => Dot,
            ':' => Colon,
            ';' => Semi,
            '~' => Tilde,
            '^' => Caret,
            '?' if n == '?' => two(self, QQ),
            '?' => Question,
            '+' if n == '=' => two(self, PlusEq),
            '+' => Plus,
            '-' if n == '>' => two(self, Arrow),
            '-' if n == '=' => two(self, MinusEq),
            '-' => Minus,
            '*' if n == '*' => {
                self.bump();
                if self.peek() == '=' {
                    self.bump();
                    StarStarEq
                } else {
                    StarStar
                }
            }
            '*' if n == '=' => two(self, StarEq),
            '*' => Star,
            '/' if n == '=' => two(self, SlashEq),
            '/' => Slash,
            '%' if n == '=' => two(self, PercentEq),
            '%' => Percent,
            '&' if n == '&' => two(self, AmpAmp),
            '&' => Amp,
            '|' if n == '|' => two(self, PipePipe),
            '|' => Pipe,
            '!' if n == '=' => two(self, Ne),
            '!' => Bang,
            '<' if n == '<' => two(self, Shl),
            '<' if n == '=' => two(self, Le),
            '<' => Lt,
            '>' if n == '>' => {
                self.bump();
                if self.peek() == '>' {
                    return Err(self.err("the '>>>' operator does not exist; convert to an unsigned type and use '>>'"));
                }
                Shr
            }
            '>' if n == '=' => two(self, Ge),
            '>' => Gt,
            '=' if n == '=' => two(self, EqEq),
            '=' => Assign,
            other => return Err(self.err(format!("unexpected character '{}'", other))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        lex(s, 0).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn newlines() {
        use Tok::*;
        assert_eq!(
            toks("a = 1 +\n 2\nb(\n1,\n2)\n\n"),
            vec![
                Ident("a".into()),
                Assign,
                Int(1),
                Plus,
                Int(2),
                Newline,
                Ident("b".into()),
                LParen,
                Int(1),
                Comma,
                Int(2),
                RParen,
                Newline,
                Eof
            ]
        );
    }

    #[test]
    fn fstring() {
        let t = toks("f\"a={x.format(-1, 3)} {{b}}\"");
        match &t[0] {
            Tok::FStr(p) => {
                assert_eq!(p.len(), 3);
                assert_eq!(p[2], FPart::Lit(" {b}".into()));
            }
            _ => panic!(),
        }
    }
}
