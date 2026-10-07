//! Recursive-descent parser producing the AST.

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::lexer::{lex_at, FPart, Tok, Token};

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    file: u32,
}

type PResult<T> = Result<T, Diag>;

const DIRECTIVES: &[&str] = &["using", "runtime", "compiler", "runtimecfg"];

pub fn parse_file(toks: Vec<Token>, file: u32) -> PResult<FileAst> {
    let mut p = Parser { toks, pos: 0, file };
    p.file_ast()
}

impl Parser {
    // ------------------------------------------------------------------ token helpers
    fn peek(&self) -> &Tok {
        &self.toks[self.pos.min(self.toks.len() - 1)].tok
    }
    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].tok
    }
    fn span(&self) -> Span {
        self.toks[self.pos.min(self.toks.len() - 1)].span
    }
    fn bump(&mut self) -> Tok {
        let t = self.peek().clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn at(&self, t: &Tok) -> bool {
        self.peek() == t
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.at(t) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, t: &Tok) -> PResult<()> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(self.unexpected(&format!("'{}'", crate::lexer::tok_text(t))))
        }
    }
    fn unexpected(&self, wanted: &str) -> Diag {
        Diag::error(self.span(), format!("expected {}, found {}", wanted, self.peek().describe()))
    }
    fn ident(&mut self) -> PResult<String> {
        match self.peek().clone() {
            Tok::Ident(s) => {
                self.bump();
                Ok(s)
            }
            _ => Err(self.unexpected("an identifier")),
        }
    }
    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Tok::Newline | Tok::Semi) {
            self.bump();
        }
    }
    /// Peeks past newlines for a continuation keyword such as `else`, `catch`, `finally`.
    fn peek_past_newlines(&self) -> &Tok {
        let mut i = self.pos;
        while i < self.toks.len() - 1 && self.toks[i].tok == Tok::Newline {
            i += 1;
        }
        &self.toks[i].tok
    }
    fn skip_to_past_newlines(&mut self) {
        while self.at(&Tok::Newline) {
            self.bump();
        }
    }
    fn end_stmt(&mut self) -> PResult<()> {
        match self.peek() {
            Tok::Newline | Tok::Semi => {
                self.bump();
                Ok(())
            }
            Tok::RBrace | Tok::Eof => Ok(()),
            _ => Err(self.unexpected("end of statement")),
        }
    }

    // ------------------------------------------------------------------ file level
    fn file_ast(&mut self) -> PResult<FileAst> {
        let mut directives = Vec::new();
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(&Tok::Eof) {
                break;
            }
            if let Tok::At(name) = self.peek().clone() {
                if DIRECTIVES.contains(&name.as_str()) {
                    directives.push(self.directive()?);
                    continue;
                }
            }
            if self.at(&Tok::Using) {
                items.push(Item::Using(self.using()?));
                continue;
            }
            items.push(self.item()?);
        }
        Ok(FileAst { file: self.file, directives, items })
    }

    fn using(&mut self) -> PResult<UsingDecl> {
        let span = self.span();
        self.expect(&Tok::Using)?;
        let mut path = self.ident()?;
        let mut wildcard = false;
        while self.eat(&Tok::Dot) {
            if self.eat(&Tok::Star) {
                wildcard = true;
                break;
            }
            path.push('.');
            path.push_str(&self.ident()?);
        }
        let alias = if self.eat(&Tok::As) { Some(self.ident()?) } else { None };
        self.end_stmt()?;
        Ok(UsingDecl { path, alias, wildcard, span })
    }

    fn directive(&mut self) -> PResult<Directive> {
        let span = self.span();
        let Tok::At(name) = self.bump() else { unreachable!() };
        let mut d = Directive { name, words: Vec::new(), options: Vec::new(), span };
        if self.at(&Tok::LParen) {
            self.bump();
            while !self.at(&Tok::RParen) {
                if self.eat(&Tok::Comma) {
                    continue;
                }
                let ospan = self.span();
                let key = self.ident()?;
                self.expect(&Tok::Assign)?;
                let val = match self.bump() {
                    Tok::Ident(s) => DirValue::Ident(s),
                    Tok::True => DirValue::Bool(true),
                    Tok::False => DirValue::Bool(false),
                    Tok::Int(v) => DirValue::Int(v),
                    Tok::LBracket => {
                        let mut list = Vec::new();
                        while !self.at(&Tok::RBracket) {
                            if self.eat(&Tok::Comma) {
                                continue;
                            }
                            list.push(self.ident()?);
                        }
                        self.bump();
                        DirValue::List(list)
                    }
                    _ => return Err(Diag::error(ospan, "invalid directive option value")),
                };
                d.options.push((key, val, ospan));
            }
            self.bump();
        } else {
            while !matches!(self.peek(), Tok::Newline | Tok::Eof | Tok::Semi) {
                match self.bump() {
                    Tok::Ident(s) => d.words.push(s),
                    Tok::Int(v) => d.words.push(v.to_string()),
                    Tok::Float(v) => d.words.push(v.to_string()),
                    other => d.words.push(crate::lexer::tok_text(&other).to_string()),
                }
            }
        }
        self.end_stmt()?;
        Ok(d)
    }

    fn mods(&mut self) -> PResult<Mods> {
        let mut m = Mods::default();
        loop {
            self.skip_newlines_if_annotation(&m);
            match self.peek().clone() {
                Tok::At(name) => {
                    self.bump();
                    m.annotations.push(name);
                    self.skip_newlines();
                }
                Tok::Public => {
                    self.bump();
                    m.access = Access::Public;
                }
                Tok::Private => {
                    self.bump();
                    m.access = Access::Private;
                }
                Tok::Protected => {
                    self.bump();
                    m.access = Access::Protected;
                }
                Tok::Static => {
                    self.bump();
                    m.is_static = true;
                }
                Tok::Immutable => {
                    self.bump();
                    m.immutable = true;
                }
                Tok::Copied => {
                    self.bump();
                    m.copied = true;
                }
                Tok::Default => {
                    self.bump();
                    m.is_default = true;
                }
                Tok::Ident(s) if s == "getter" && self.starts_member_after_mod() => {
                    self.bump();
                    m.getter = true;
                }
                // json["key", encode, decode]
                Tok::Ident(s) if s == "json" && matches!(self.peek_at(1), Tok::LBracket) => {
                    let span = self.span();
                    self.bump();
                    self.bump();
                    let mut spec = JsonSpec { name: None, encode: false, decode: false, span };
                    loop {
                        match self.peek().clone() {
                            Tok::RBracket => {
                                self.bump();
                                break;
                            }
                            Tok::Str(name) => {
                                self.bump();
                                if spec.name.is_some() {
                                    return Err(Diag::error(self.span(), "json[...] takes one name"));
                                }
                                spec.name = Some(name);
                            }
                            Tok::Ident(w) if w == "encode" || w == "decode" => {
                                self.bump();
                                if w == "encode" {
                                    spec.encode = true;
                                } else {
                                    spec.decode = true;
                                }
                            }
                            _ => return Err(Diag::error(self.span(), "json[...] takes an optional \"name\" and the words encode and/or decode")),
                        }
                        match self.peek() {
                            Tok::Comma => {
                                self.bump();
                            }
                            Tok::RBracket => {}
                            _ => return Err(Diag::error(self.span(), "expected ',' or ']' in json[...]")),
                        }
                    }
                    if m.json.is_some() {
                        return Err(Diag::error(span, "duplicate json[...] modifier"));
                    }
                    m.json = Some(spec);
                }
                Tok::Ident(s) if s == "setter" && matches!(self.peek_at(1), Tok::Dot) => {
                    self.bump();
                    self.bump();
                    let kind = self.ident()?;
                    m.setter = Some(match kind.as_str() {
                        "chain" => true,
                        "nochain" => false,
                        _ => return Err(Diag::error(self.span(), "expected 'setter.chain' or 'setter.nochain'")),
                    });
                }
                _ => return Ok(m),
            }
        }
    }

    fn skip_newlines_if_annotation(&mut self, _m: &Mods) {}

    /// `getter` is a contextual keyword: treat it as a modifier only when a type follows.
    fn starts_member_after_mod(&self) -> bool {
        matches!(
            self.peek_at(1),
            Tok::Ident(_) | Tok::Immutable | Tok::Copied | Tok::Static | Tok::Public | Tok::Private | Tok::Protected | Tok::Amp | Tok::LParen
        )
    }

    fn item(&mut self) -> PResult<Item> {
        let mods = self.mods()?;
        match self.peek() {
            Tok::Function => Ok(Item::Function(self.function(mods)?)),
            Tok::Class => Ok(Item::Class(self.class(mods)?)),
            Tok::Interface => Ok(Item::Interface(self.interface(mods)?)),
            _ => Err(self.unexpected("'function', 'class' or 'interface'")),
        }
    }

    fn type_params(&mut self) -> PResult<Vec<TypeParam>> {
        let mut tps = Vec::new();
        if self.eat(&Tok::LBracket) {
            while !self.at(&Tok::RBracket) {
                let name = self.ident()?;
                let bound = if self.eat(&Tok::Extends) { Some(self.parse_type()?) } else { None };
                let default = if self.eat(&Tok::Assign) { Some(self.parse_type()?) } else { None };
                tps.push(TypeParam { name, bound, default });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RBracket)?;
        }
        Ok(tps)
    }

    fn params(&mut self) -> PResult<Vec<Param>> {
        self.expect(&Tok::LParen)?;
        let mut ps = Vec::new();
        while !self.at(&Tok::RParen) {
            let span = self.span();
            let mut mods = DeclMods::default();
            loop {
                if self.eat(&Tok::Immutable) {
                    mods.immutable = true;
                } else if self.eat(&Tok::Copied) {
                    mods.copied = true;
                } else {
                    break;
                }
            }
            let ty = self.parse_type()?;
            let name = self.ident()?;
            ps.push(Param { ty, name, mods, span });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(ps)
    }

    fn throws(&mut self) -> PResult<Vec<String>> {
        let mut t = Vec::new();
        if self.eat(&Tok::Throws) {
            loop {
                t.push(self.ident()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        Ok(t)
    }

    fn function(&mut self, mods: Mods) -> PResult<FuncDecl> {
        let span = self.span();
        self.expect(&Tok::Function)?;
        let ret = self.parse_type()?;
        let name = self.function_name()?;
        let type_params = self.type_params()?;
        let params = self.params()?;
        let throws = self.throws()?;
        let mut body = None;
        let mut delegate = None;
        if self.at(&Tok::LBrace) {
            body = Some(self.block()?);
        } else if self.eat(&Tok::Assign) {
            match self.ident()? {
                s if s == "origin" => delegate = Some(self.ident()?),
                _ => return Err(Diag::error(self.span(), "expected 'origin <Interface>'")),
            }
        }
        self.end_stmt()?;
        Ok(FuncDecl { mods, ret, name, type_params, params, throws, body, delegate, span })
    }

    /// A function name, or `operator<op>` for operator overloading (spec 6.9).
    fn function_name(&mut self) -> PResult<String> {
        let name = self.ident()?;
        if name != "operator" || matches!(self.peek(), Tok::LParen | Tok::LBracket if self.peek_at(1) != &Tok::RBracket) {
            return Ok(name);
        }
        let op = match self.bump() {
            Tok::LBracket => {
                self.expect(&Tok::RBracket)?;
                if self.eat(&Tok::Assign) {
                    "[]="
                } else {
                    "[]"
                }
            }
            t @ (Tok::Plus | Tok::Minus | Tok::Star | Tok::Slash | Tok::Percent | Tok::StarStar | Tok::AmpAmp | Tok::PipePipe | Tok::Caret | Tok::Shl | Tok::Shr | Tok::Tilde) => crate::lexer::tok_text(&t),
            other => return Err(Diag::error(self.span(), format!("'{}' cannot be overloaded", crate::lexer::tok_text(&other)))),
        };
        Ok(format!("operator{}", op))
    }

    fn class(&mut self, mods: Mods) -> PResult<ClassDecl> {
        let span = self.span();
        self.expect(&Tok::Class)?;
        let name = self.ident()?;
        let type_params = self.type_params()?;
        let extends = if self.eat(&Tok::Extends) { Some(self.parse_type()?) } else { None };
        let mut implements = Vec::new();
        if self.eat(&Tok::Implements) {
            loop {
                implements.push(self.parse_type()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        self.expect(&Tok::LBrace)?;
        let mut c = ClassDecl { mods, name, type_params, extends, implements, fields: Vec::new(), ctors: Vec::new(), methods: Vec::new(), span };
        loop {
            self.skip_newlines();
            if self.eat(&Tok::RBrace) {
                break;
            }
            let mspan = self.span();
            let mods = self.mods()?;
            match self.peek().clone() {
                Tok::Function => c.methods.push(self.function(mods)?),
                Tok::Ident(n) if n == c.name && self.peek_at(1) == &Tok::LParen => {
                    self.bump();
                    let params = self.params()?;
                    let throws = self.throws()?;
                    let body = self.block()?;
                    self.end_stmt()?;
                    c.ctors.push(CtorDecl { mods, params, throws, body, span: mspan });
                }
                _ => {
                    let ty = self.parse_type()?;
                    let name = self.ident()?;
                    let init = if self.eat(&Tok::Assign) { Some(self.expr()?) } else { None };
                    self.end_stmt()?;
                    c.fields.push(FieldDecl { mods, ty, name, init, span: mspan });
                }
            }
        }
        self.end_stmt()?;
        Ok(c)
    }

    fn interface(&mut self, mods: Mods) -> PResult<InterfaceDecl> {
        let span = self.span();
        self.expect(&Tok::Interface)?;
        let name = self.ident()?;
        let type_params = self.type_params()?;
        let mut extends = Vec::new();
        if self.eat(&Tok::Extends) {
            loop {
                extends.push(self.parse_type()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        self.expect(&Tok::LBrace)?;
        let mut methods = Vec::new();
        loop {
            self.skip_newlines();
            if self.eat(&Tok::RBrace) {
                break;
            }
            let mods = self.mods()?;
            if !self.at(&Tok::Function) {
                return Err(self.unexpected("'function' in interface body"));
            }
            methods.push(self.function(mods)?);
        }
        self.end_stmt()?;
        Ok(InterfaceDecl { mods, name, type_params, extends, methods, span })
    }

    // ------------------------------------------------------------------ types
    pub fn parse_type(&mut self) -> PResult<TypeExpr> {
        let first = self.type_postfix()?;
        if self.at(&Tok::Pipe) {
            let mut ts = vec![first];
            while self.eat(&Tok::Pipe) {
                ts.push(self.type_postfix()?);
            }
            return Ok(TypeExpr::Union(ts));
        }
        Ok(first)
    }

    fn type_postfix(&mut self) -> PResult<TypeExpr> {
        let mut t = self.type_primary()?;
        loop {
            if self.at(&Tok::LBracket) && self.peek_at(1) == &Tok::RBracket {
                self.bump();
                self.bump();
                t = TypeExpr::Array(Box::new(t));
            } else if self.at(&Tok::Question) {
                self.bump();
                t = TypeExpr::Nullable(Box::new(t));
            } else {
                return Ok(t);
            }
        }
    }

    fn type_primary(&mut self) -> PResult<TypeExpr> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Amp => {
                self.bump();
                Ok(TypeExpr::Ref { mutable: false, inner: Box::new(self.type_postfix()?) })
            }
            Tok::Star => {
                self.bump();
                Ok(TypeExpr::Ref { mutable: true, inner: Box::new(self.type_postfix()?) })
            }
            Tok::Void => {
                self.bump();
                Ok(TypeExpr::Void)
            }
            Tok::Question => {
                self.bump();
                Ok(TypeExpr::Wildcard(span))
            }
            Tok::LParen => {
                self.bump();
                let mut ts = Vec::new();
                while !self.at(&Tok::RParen) {
                    ts.push(self.parse_type()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RParen)?;
                Ok(TypeExpr::Tuple(ts))
            }
            Tok::Ident(name) => {
                self.bump();
                let mut name = name;
                // qualified names: `linear.Matrix`, `math.linear.Matrix`
                while self.at(&Tok::Dot) && matches!(self.peek_at(1), Tok::Ident(_)) {
                    self.bump();
                    name.push('.');
                    name.push_str(&self.ident()?);
                }
                let mut args = Vec::new();
                if self.at(&Tok::LBracket) && self.peek_at(1) != &Tok::RBracket {
                    self.bump();
                    while !self.at(&Tok::RBracket) {
                        args.push(self.parse_type()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RBracket)?;
                }
                if name == "Function" {
                    if args.len() != 2 {
                        return Err(Diag::error(span, "Function type needs the form Function[(params), return]"));
                    }
                    let ret = args.pop().unwrap();
                    let params = match args.pop().unwrap() {
                        TypeExpr::Tuple(ps) => ps,
                        other => vec![other],
                    };
                    return Ok(TypeExpr::Func(params, Box::new(ret)));
                }
                Ok(TypeExpr::Named { name, args, span })
            }
            _ => Err(self.unexpected("a type")),
        }
    }

    // ------------------------------------------------------------------ statements
    fn block(&mut self) -> PResult<Block> {
        let span = self.span();
        self.expect(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if self.eat(&Tok::RBrace) {
                break;
            }
            if self.at(&Tok::Eof) {
                return Err(Diag::error(span, "unclosed block"));
            }
            stmts.push(self.stmt()?);
        }
        Ok(Block { stmts, span })
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let span = self.span();
        let kind = match self.peek().clone() {
            Tok::LBrace => {
                let b = self.block()?;
                self.end_stmt()?;
                StmtKind::Block(b)
            }
            Tok::If => return self.if_stmt(),
            Tok::For => return self.for_stmt(),
            Tok::Switch => return self.switch_stmt(),
            Tok::Try => return self.try_stmt(),
            Tok::Return => {
                self.bump();
                let e = if matches!(self.peek(), Tok::Newline | Tok::Semi | Tok::RBrace | Tok::Eof) {
                    None
                } else {
                    let first = self.expr()?;
                    if self.at(&Tok::Comma) {
                        let mut items = vec![first];
                        while self.eat(&Tok::Comma) {
                            items.push(self.expr()?);
                        }
                        Some(Expr::new(ExprKind::Tuple(items), span))
                    } else {
                        Some(first)
                    }
                };
                self.end_stmt()?;
                StmtKind::Return(e)
            }
            Tok::Throw => {
                self.bump();
                let e = self.expr()?;
                self.end_stmt()?;
                StmtKind::Throw(e)
            }
            Tok::Break => {
                self.bump();
                self.end_stmt()?;
                StmtKind::Break
            }
            Tok::Continue => {
                self.bump();
                self.end_stmt()?;
                StmtKind::Continue
            }
            Tok::Fallthrough => {
                self.bump();
                self.end_stmt()?;
                StmtKind::Fallthrough
            }
            Tok::Using => StmtKind::Using(self.using()?),
            _ => {
                let k = self.simple_stmt()?;
                self.end_stmt()?;
                k
            }
        };
        Ok(Stmt { kind, span })
    }

    /// Declarations, assignments and expression statements (also used in `for (...)` headers).
    fn simple_stmt(&mut self) -> PResult<StmtKind> {
        // declaration with modifiers
        let mut mods = DeclMods::default();
        let mut had_mods = false;
        loop {
            if self.eat(&Tok::Immutable) {
                mods.immutable = true;
                had_mods = true;
            } else if self.eat(&Tok::Copied) {
                mods.copied = true;
                had_mods = true;
            } else {
                break;
            }
        }
        if had_mods {
            return self.var_decl_rest(mods);
        }
        // speculative declaration: Type name ...
        let save = self.pos;
        if let Ok(ty) = self.parse_type() {
            if let Tok::Ident(name) = self.peek().clone() {
                if matches!(self.peek_at(1), Tok::Assign | Tok::Newline | Tok::Semi | Tok::Eof | Tok::RBrace | Tok::RParen) {
                    self.bump();
                    let init = if self.eat(&Tok::Assign) { Some(self.expr()?) } else { None };
                    return Ok(StmtKind::VarDecl { mods, ty, name, init });
                }
            }
        }
        self.pos = save;
        // expression / assignment
        let first = self.expr_or_discard()?;
        if self.at(&Tok::Comma) {
            let mut targets = vec![first];
            while self.eat(&Tok::Comma) {
                targets.push(self.expr_or_discard()?);
            }
            self.expect(&Tok::Assign)?;
            let value = self.expr()?;
            return Ok(StmtKind::MultiAssign { targets, value });
        }
        let first = match first {
            Some(e) => e,
            None => return Err(Diag::error(self.span(), "'_' can only be used as an assignment target")),
        };
        let op = match self.peek() {
            Tok::Assign => Some(None),
            Tok::PlusEq => Some(Some(BinOp::Add)),
            Tok::MinusEq => Some(Some(BinOp::Sub)),
            Tok::StarEq => Some(Some(BinOp::Mul)),
            Tok::SlashEq => Some(Some(BinOp::Div)),
            Tok::PercentEq => Some(Some(BinOp::Rem)),
            Tok::StarStarEq => Some(Some(BinOp::Pow)),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let value = self.expr()?;
            return Ok(StmtKind::Assign { target: first, op, value });
        }
        Ok(StmtKind::Expr(first))
    }

    fn expr_or_discard(&mut self) -> PResult<Option<Expr>> {
        if let Tok::Ident(s) = self.peek() {
            if s == "_" && matches!(self.peek_at(1), Tok::Comma | Tok::Assign) {
                self.bump();
                return Ok(None);
            }
        }
        Ok(Some(self.expr()?))
    }

    fn var_decl_rest(&mut self, mods: DeclMods) -> PResult<StmtKind> {
        let ty = self.parse_type()?;
        let name = self.ident()?;
        let init = if self.eat(&Tok::Assign) { Some(self.expr()?) } else { None };
        Ok(StmtKind::VarDecl { mods, ty, name, init })
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        let span = self.span();
        self.expect(&Tok::If)?;
        self.expect(&Tok::LParen)?;
        let cond = self.expr()?;
        self.expect(&Tok::RParen)?;
        let then = self.block()?;
        let mut els = None;
        if self.peek_past_newlines() == &Tok::Else {
            self.skip_to_past_newlines();
            self.bump();
            if self.at(&Tok::If) {
                els = Some(Box::new(self.if_stmt()?));
                return Ok(Stmt { kind: StmtKind::If { cond, then, els }, span });
            }
            let b = self.block()?;
            let bspan = b.span;
            els = Some(Box::new(Stmt { kind: StmtKind::Block(b), span: bspan }));
        }
        self.end_stmt()?;
        Ok(Stmt { kind: StmtKind::If { cond, then, els }, span })
    }

    fn for_stmt(&mut self) -> PResult<Stmt> {
        let span = self.span();
        self.expect(&Tok::For)?;
        self.expect(&Tok::LParen)?;
        // for-each: ident (, ident)* in expr
        let mut look = self.pos;
        let mut is_each = false;
        loop {
            match &self.toks[look].tok {
                Tok::Ident(_) => look += 1,
                _ => break,
            }
            match &self.toks[look].tok {
                Tok::Comma => look += 1,
                Tok::In => {
                    is_each = true;
                    break;
                }
                _ => break,
            }
        }
        if is_each {
            let mut vars = Vec::new();
            loop {
                vars.push(self.ident()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::In)?;
            let iter = self.expr()?;
            self.expect(&Tok::RParen)?;
            let body = self.block()?;
            self.end_stmt()?;
            return Ok(Stmt { kind: StmtKind::ForEach { vars, iter, body }, span });
        }
        let init = if self.at(&Tok::Semi) {
            None
        } else {
            let s = self.span();
            Some(Box::new(Stmt { kind: self.simple_stmt()?, span: s }))
        };
        self.expect(&Tok::Semi)?;
        let cond = if self.at(&Tok::Semi) { None } else { Some(self.expr()?) };
        self.expect(&Tok::Semi)?;
        let step = if self.at(&Tok::RParen) {
            None
        } else {
            let s = self.span();
            Some(Box::new(Stmt { kind: self.simple_stmt()?, span: s }))
        };
        self.expect(&Tok::RParen)?;
        let body = self.block()?;
        self.end_stmt()?;
        Ok(Stmt { kind: StmtKind::ForC { init, cond, step, body }, span })
    }

    fn switch_stmt(&mut self) -> PResult<Stmt> {
        let span = self.span();
        self.expect(&Tok::Switch)?;
        self.expect(&Tok::LParen)?;
        let value = self.expr()?;
        let alias = if self.eat(&Tok::As) { Some(self.ident()?) } else { None };
        self.expect(&Tok::RParen)?;
        self.expect(&Tok::LBrace)?;
        let mut cases = Vec::new();
        loop {
            self.skip_newlines();
            if self.eat(&Tok::RBrace) {
                break;
            }
            let cspan = self.span();
            let cond = if self.eat(&Tok::Case) {
                let e = self.expr()?;
                Some(e)
            } else if self.eat(&Tok::Default) {
                None
            } else {
                return Err(self.unexpected("'case' or 'default'"));
            };
            self.expect(&Tok::Colon)?;
            let mut body = Vec::new();
            let mut fallthrough = false;
            loop {
                self.skip_newlines();
                if matches!(self.peek(), Tok::Case | Tok::Default | Tok::RBrace | Tok::Eof) {
                    break;
                }
                let s = self.stmt()?;
                if matches!(s.kind, StmtKind::Fallthrough) {
                    fallthrough = true;
                    self.skip_newlines();
                    if !matches!(self.peek(), Tok::Case | Tok::Default | Tok::RBrace) {
                        return Err(Diag::error(s.span, "'fallthrough' must be the last statement of a case"));
                    }
                    break;
                }
                body.push(s);
            }
            cases.push(SwitchCase { cond, body, fallthrough, span: cspan });
        }
        self.end_stmt()?;
        Ok(Stmt { kind: StmtKind::Switch { value, alias, cases }, span })
    }

    fn try_stmt(&mut self) -> PResult<Stmt> {
        let span = self.span();
        self.expect(&Tok::Try)?;
        let body = self.block()?;
        let mut catches = Vec::new();
        while self.peek_past_newlines() == &Tok::Catch {
            self.skip_to_past_newlines();
            let cspan = self.span();
            self.bump();
            self.expect(&Tok::LParen)?;
            let mut types = vec![self.ident()?];
            while self.eat(&Tok::Pipe) {
                types.push(self.ident()?);
            }
            let name = self.ident()?;
            self.expect(&Tok::RParen)?;
            let b = self.block()?;
            catches.push(CatchClause { types, name, body: b, span: cspan });
        }
        let mut finally = None;
        if self.peek_past_newlines() == &Tok::Finally {
            self.skip_to_past_newlines();
            self.bump();
            finally = Some(self.block()?);
        }
        if catches.is_empty() && finally.is_none() {
            return Err(Diag::error(span, "'try' needs at least one 'catch' or a 'finally'"));
        }
        self.end_stmt()?;
        Ok(Stmt { kind: StmtKind::Try { body, catches, finally }, span })
    }

    // ------------------------------------------------------------------ expressions
    pub fn expr(&mut self) -> PResult<Expr> {
        self.ternary()
    }

    fn ternary(&mut self) -> PResult<Expr> {
        let cond = self.coalesce()?;
        if self.at(&Tok::Question) {
            let span = self.span();
            self.bump();
            let a = self.ternary()?;
            self.expect(&Tok::Colon)?;
            let b = self.ternary()?;
            return Ok(Expr::new(ExprKind::Ternary(Box::new(cond), Box::new(a), Box::new(b)), span));
        }
        Ok(cond)
    }

    fn coalesce(&mut self) -> PResult<Expr> {
        let a = self.or()?;
        if self.at(&Tok::QQ) {
            let span = self.span();
            self.bump();
            let b = self.coalesce()?;
            return Ok(Expr::new(ExprKind::Coalesce(Box::new(a), Box::new(b)), span));
        }
        Ok(a)
    }

    fn or(&mut self) -> PResult<Expr> {
        let mut a = self.and()?;
        while self.at(&Tok::Or) {
            let span = self.span();
            self.bump();
            let b = self.and()?;
            a = Expr::new(ExprKind::Binary(BinOp::Or, Box::new(a), Box::new(b)), span);
        }
        Ok(a)
    }

    fn and(&mut self) -> PResult<Expr> {
        let mut a = self.not()?;
        while self.at(&Tok::And) {
            let span = self.span();
            self.bump();
            let b = self.not()?;
            a = Expr::new(ExprKind::Binary(BinOp::And, Box::new(a), Box::new(b)), span);
        }
        Ok(a)
    }

    fn not(&mut self) -> PResult<Expr> {
        if self.at(&Tok::Not) {
            let span = self.span();
            self.bump();
            let e = self.not()?;
            return Ok(Expr::new(ExprKind::Unary(UnOp::Not, Box::new(e)), span));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> PResult<Expr> {
        let mut a = self.bitor()?;
        loop {
            let op = match self.peek() {
                Tok::EqEq => BinOp::Eq,
                Tok::Ne => BinOp::Ne,
                Tok::Lt => BinOp::Lt,
                Tok::Gt => BinOp::Gt,
                Tok::Le => BinOp::Le,
                Tok::Ge => BinOp::Ge,
                _ => return Ok(a),
            };
            let span = self.span();
            self.bump();
            let b = self.bitor()?;
            a = Expr::new(ExprKind::Binary(op, Box::new(a), Box::new(b)), span);
        }
    }

    fn binary_level(&mut self, ops: &[(Tok, BinOp)], next: fn(&mut Self) -> PResult<Expr>) -> PResult<Expr> {
        let mut a = next(self)?;
        'outer: loop {
            for (t, op) in ops {
                if self.at(t) {
                    let span = self.span();
                    self.bump();
                    let b = next(self)?;
                    a = Expr::new(ExprKind::Binary(*op, Box::new(a), Box::new(b)), span);
                    continue 'outer;
                }
            }
            return Ok(a);
        }
    }

    fn bitor(&mut self) -> PResult<Expr> {
        self.binary_level(&[(Tok::PipePipe, BinOp::BitOr)], Self::bitxor)
    }
    fn bitxor(&mut self) -> PResult<Expr> {
        self.binary_level(&[(Tok::Caret, BinOp::BitXor)], Self::bitand)
    }
    fn bitand(&mut self) -> PResult<Expr> {
        self.binary_level(&[(Tok::AmpAmp, BinOp::BitAnd)], Self::shift)
    }
    fn shift(&mut self) -> PResult<Expr> {
        self.binary_level(&[(Tok::Shl, BinOp::Shl), (Tok::Shr, BinOp::Shr)], Self::additive)
    }
    fn additive(&mut self) -> PResult<Expr> {
        self.binary_level(&[(Tok::Plus, BinOp::Add), (Tok::Minus, BinOp::Sub)], Self::multiplicative)
    }
    fn multiplicative(&mut self) -> PResult<Expr> {
        self.binary_level(&[(Tok::Star, BinOp::Mul), (Tok::Slash, BinOp::Div), (Tok::Percent, BinOp::Rem)], Self::prefix)
    }

    fn prefix(&mut self) -> PResult<Expr> {
        let span = self.span();
        let op = match self.peek() {
            Tok::Bang => UnOp::Not,
            Tok::Tilde => UnOp::BitNot,
            Tok::Minus => UnOp::Neg,
            Tok::Amp => UnOp::Ref,
            Tok::Star => UnOp::RefMut,
            _ => return self.power(),
        };
        self.bump();
        let e = self.prefix()?;
        Ok(Expr::new(ExprKind::Unary(op, Box::new(e)), span))
    }

    fn power(&mut self) -> PResult<Expr> {
        let base = self.postfix()?;
        if self.at(&Tok::StarStar) {
            let span = self.span();
            self.bump();
            let exp = self.prefix()?;
            return Ok(Expr::new(ExprKind::Binary(BinOp::Pow, Box::new(base), Box::new(exp)), span));
        }
        Ok(base)
    }

    fn call_args(&mut self) -> PResult<Vec<Arg>> {
        self.expect(&Tok::LParen)?;
        let mut args = Vec::new();
        while !self.at(&Tok::RParen) {
            let name = match (self.peek().clone(), self.peek_at(1)) {
                (Tok::Ident(n), Tok::Assign) => {
                    self.bump();
                    self.bump();
                    Some(n)
                }
                _ => None,
            };
            let value = self.expr()?;
            args.push(Arg { name, value });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(args)
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            let span = self.span();
            match self.peek() {
                Tok::LParen => {
                    let args = self.call_args()?;
                    e = Expr::new(ExprKind::Call(Box::new(e), args), span);
                }
                Tok::Dot => {
                    self.bump();
                    let name = self.ident()?;
                    e = Expr::new(ExprKind::Member(Box::new(e), name), span);
                }
                Tok::LBracket => {
                    self.bump();
                    if self.eat(&Tok::RBracket) {
                        // `Int64[]` used as a type argument, e.g. `x.castTo(Int64[])`
                        let t = expr_to_type(&e).ok_or_else(|| Diag::error(span, "expected an index expression"))?;
                        e = Expr::new(ExprKind::TypeLit(TypeExpr::Array(Box::new(t))), span);
                        continue;
                    }
                    let mut idx = Vec::new();
                    while !self.at(&Tok::RBracket) {
                        idx.push(self.index_arg()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RBracket)?;
                    e = Expr::new(ExprKind::Index(Box::new(e), idx), span);
                }
                Tok::Bang => {
                    self.bump();
                    e = Expr::new(ExprKind::NonNull(Box::new(e)), span);
                }
                Tok::Question if matches!(self.peek_at(1), Tok::RParen | Tok::Comma | Tok::RBracket) => {
                    // `castTo(Int64?)`
                    self.bump();
                    let t = expr_to_type(&e).ok_or_else(|| Diag::error(span, "unexpected '?'"))?;
                    e = Expr::new(ExprKind::TypeLit(TypeExpr::Nullable(Box::new(t))), span);
                }
                _ => return Ok(e),
            }
        }
    }

    /// Index / type-argument position: allows types such as `(Int, Int)` or `Int|String`.
    fn index_arg(&mut self) -> PResult<Expr> {
        let span = self.span();
        if matches!(self.peek(), Tok::Question | Tok::Void) {
            let t = self.parse_type()?;
            return Ok(Expr::new(ExprKind::TypeLit(t), span));
        }
        let e = self.expr()?;
        if self.at(&Tok::Pipe) {
            let mut ts = vec![expr_to_type(&e).ok_or_else(|| Diag::error(span, "expected a type"))?];
            while self.eat(&Tok::Pipe) {
                ts.push(self.type_postfix()?);
            }
            return Ok(Expr::new(ExprKind::TypeLit(TypeExpr::Union(ts)), span));
        }
        Ok(e)
    }

    fn is_lambda_ahead(&self) -> bool {
        // at '(' : find the matching ')' and check for '->'
        let mut depth = 0;
        let mut i = self.pos;
        while i < self.toks.len() {
            match self.toks[i].tok {
                Tok::LParen => depth += 1,
                Tok::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return self.toks.get(i + 1).map(|t| t.tok == Tok::Arrow).unwrap_or(false);
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn lambda(&mut self, is_move: bool) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::LParen)?;
        let mut params = Vec::new();
        while !self.at(&Tok::RParen) {
            let pspan = self.span();
            let mut mods = DeclMods::default();
            if self.eat(&Tok::Copied) {
                mods.copied = true;
            }
            if let (Tok::Ident(n), Tok::Comma | Tok::RParen) = (self.peek().clone(), self.peek_at(1)) {
                self.bump();
                params.push(Param { ty: TypeExpr::Wildcard(pspan), name: n, mods, span: pspan });
            } else {
                let ty = self.parse_type()?;
                let name = self.ident()?;
                params.push(Param { ty, name, mods, span: pspan });
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        self.expect(&Tok::Arrow)?;
        let body = if self.at(&Tok::LBrace) { LambdaBody::Block(self.block()?) } else { LambdaBody::Expr(Box::new(self.expr()?)) };
        Ok(Expr::new(ExprKind::Lambda { params, ret: None, body, is_move }, span))
    }

    /// Parses an expression embedded in an f-string.
    fn sub_expr(&self, src: &str, at: Span) -> PResult<Expr> {
        let toks = lex_at(src, self.file, at.line, at.col)?;
        let mut sub = Parser { toks, pos: 0, file: self.file };
        let e = sub.expr()?;
        sub.skip_newlines();
        if !sub.at(&Tok::Eof) {
            return Err(sub.unexpected("end of f-string expression"));
        }
        Ok(e)
    }

    /// `new T(args)`: `T` is a (qualified) class name with optional type arguments.
    fn new_expr(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::New)?;
        let tspan = self.span();
        let mut callee = Expr::new(ExprKind::Ident(self.ident()?), tspan);
        while self.at(&Tok::Dot) {
            let s = self.span();
            self.bump();
            callee = Expr::new(ExprKind::Member(Box::new(callee), self.ident()?), s);
        }
        if self.at(&Tok::LBracket) {
            let s = self.span();
            self.bump();
            let mut targs = Vec::new();
            while !self.at(&Tok::RBracket) {
                targs.push(self.index_arg()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RBracket)?;
            callee = Expr::new(ExprKind::Index(Box::new(callee), targs), s);
        }
        if !self.at(&Tok::LParen) {
            return Err(self.unexpected("'(' after the class name in 'new'"));
        }
        let args = self.call_args()?;
        Ok(Expr::new(ExprKind::New(Box::new(callee), args), span))
    }

    fn primary(&mut self) -> PResult<Expr> {
        let span = self.span();
        let k = match self.peek().clone() {
            Tok::New => return self.new_expr(),
            Tok::Int(v) => {
                self.bump();
                ExprKind::Int(v)
            }
            Tok::Float(v) => {
                self.bump();
                ExprKind::Float(v)
            }
            Tok::Str(s) => {
                self.bump();
                ExprKind::Str(s)
            }
            Tok::FStr(parts) => {
                self.bump();
                let mut out = Vec::new();
                for p in parts {
                    match p {
                        FPart::Lit(s) => out.push(FStrPart::Lit(s)),
                        FPart::Expr(src, espan) => out.push(FStrPart::Expr(self.sub_expr(&src, espan)?)),
                        FPart::Field { src, span: espan, conv, debug, spec } => {
                            let expr = self.sub_expr(&src, espan)?;
                            let mut sp = Vec::new();
                            for p in spec {
                                match p {
                                    FPart::Lit(s) => sp.push(FStrPart::Lit(s)),
                                    FPart::Expr(src, nspan) => sp.push(FStrPart::Expr(self.sub_expr(&src, nspan)?)),
                                    FPart::Field { span, .. } => return Err(Diag::error(span, "format specifiers cannot nest formatted fields")),
                                }
                            }
                            out.push(FStrPart::Fmt { expr, conv, debug, spec: sp });
                        }
                    }
                }
                ExprKind::FStr(out)
            }
            Tok::True => {
                self.bump();
                ExprKind::Bool(true)
            }
            Tok::False => {
                self.bump();
                ExprKind::Bool(false)
            }
            Tok::Null => {
                self.bump();
                ExprKind::Null
            }
            Tok::This => {
                self.bump();
                ExprKind::This
            }
            Tok::Super => {
                self.bump();
                if self.at(&Tok::LParen) {
                    let args = self.call_args()?;
                    if self.at(&Tok::Dot) && args.len() == 1 && args[0].name.is_none() {
                        if let ExprKind::Ident(n) = &args[0].value.kind {
                            return Ok(Expr::new(ExprKind::Super(Some(n.clone())), span));
                        }
                    }
                    return Ok(Expr::new(ExprKind::Call(Box::new(Expr::new(ExprKind::Super(None), span)), args), span));
                }
                ExprKind::Super(None)
            }
            Tok::Ident(name) => {
                if name == "move" && self.peek_at(1) == &Tok::LParen {
                    self.bump();
                    if self.is_lambda_ahead() {
                        return self.lambda(true);
                    }
                    self.pos -= 1;
                }
                self.bump();
                ExprKind::Ident(name)
            }
            Tok::LParen => {
                if self.is_lambda_ahead() {
                    return self.lambda(false);
                }
                self.bump();
                if self.eat(&Tok::RParen) {
                    // `()` as an empty parameter list in a type argument: Function[(), T]
                    return Ok(Expr::new(ExprKind::TypeLit(TypeExpr::Tuple(Vec::new())), span));
                }
                let first = self.expr()?;
                if self.at(&Tok::Comma) {
                    let mut items = vec![first];
                    while self.eat(&Tok::Comma) {
                        items.push(self.expr()?);
                    }
                    self.expect(&Tok::RParen)?;
                    return Ok(Expr::new(ExprKind::Tuple(items), span));
                }
                self.expect(&Tok::RParen)?;
                return Ok(first);
            }
            Tok::LBrace => {
                self.bump();
                let mut entries = Vec::new();
                loop {
                    self.skip_newlines();
                    if self.eat(&Tok::RBrace) {
                        break;
                    }
                    let k = self.expr()?;
                    self.skip_newlines();
                    self.expect(&Tok::Colon)?;
                    self.skip_newlines();
                    let v = self.expr()?;
                    entries.push((k, v));
                    self.skip_newlines();
                    if !self.eat(&Tok::Comma) {
                        self.skip_newlines();
                        self.expect(&Tok::RBrace)?;
                        break;
                    }
                }
                ExprKind::Dict(entries)
            }
            Tok::Void => {
                self.bump();
                ExprKind::TypeLit(TypeExpr::Void)
            }
            Tok::LBracket => {
                self.bump();
                let mut items = Vec::new();
                loop {
                    self.skip_newlines();
                    if self.eat(&Tok::RBracket) {
                        break;
                    }
                    items.push(self.expr()?);
                    self.skip_newlines();
                    if !self.eat(&Tok::Comma) {
                        self.skip_newlines();
                        self.expect(&Tok::RBracket)?;
                        break;
                    }
                }
                ExprKind::ArrayLit(items)
            }
            _ => return Err(self.unexpected("an expression")),
        };
        Ok(Expr::new(k, span))
    }
}

/// `a.b.c` written as identifiers and member accesses.
pub fn dotted_name(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Ident(n) => Some(n.clone()),
        ExprKind::Member(o, n) => Some(format!("{}.{}", dotted_name(o)?, n)),
        _ => None,
    }
}

/// Reinterprets an expression as a type (for type arguments written in expression position).
pub fn expr_to_type(e: &Expr) -> Option<TypeExpr> {
    match &e.kind {
        ExprKind::Ident(n) => {
            if n == "Function" {
                return None;
            }
            Some(TypeExpr::Named { name: n.clone(), args: Vec::new(), span: e.span })
        }
        ExprKind::TypeLit(t) => Some(t.clone()),
        ExprKind::Member(..) => Some(TypeExpr::Named { name: dotted_name(e)?, args: Vec::new(), span: e.span }),
        ExprKind::Index(base, args) => {
            let n = &dotted_name(base)?;
            let mut targs = Vec::new();
            for a in args {
                targs.push(expr_to_type(a)?);
            }
            if n == "Function" && targs.len() == 2 {
                let ret = targs.pop().unwrap();
                let params = match targs.pop().unwrap() {
                    TypeExpr::Tuple(ps) => ps,
                    other => vec![other],
                };
                return Some(TypeExpr::Func(params, Box::new(ret)));
            }
            Some(TypeExpr::Named { name: n.clone(), args: targs, span: e.span })
        }
        ExprKind::Tuple(items) => {
            let mut ts = Vec::new();
            for i in items {
                ts.push(expr_to_type(i)?);
            }
            Some(TypeExpr::Tuple(ts))
        }
        ExprKind::Unary(UnOp::Ref, inner) => Some(TypeExpr::Ref { mutable: false, inner: Box::new(expr_to_type(inner)?) }),
        ExprKind::Unary(UnOp::RefMut, inner) => Some(TypeExpr::Ref { mutable: true, inner: Box::new(expr_to_type(inner)?) }),
        _ => None,
    }
}
