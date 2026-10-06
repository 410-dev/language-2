//! Expression checking and lowering.

use super::*;
use crate::ast::{BinOp, ExprKind as A, UnOp};
use crate::hir::{Expr as HExpr, ExprKind as H};
use l2_runtime::ops::{ArithOp, CmpOp};
use l2_runtime::{Builtin, IntTy};

pub struct Cand {
    pub params: Vec<Type>,
    pub names: Vec<String>,
}

pub fn is_literalish(e: &ast::Expr) -> bool {
    match &e.kind {
        A::Int(_) | A::Float(_) | A::Null | A::Dict(_) | A::Lambda { .. } => true,
        A::Unary(UnOp::Neg, inner) => matches!(inner.kind, A::Int(_) | A::Float(_)),
        A::Tuple(items) => items.iter().any(is_literalish),
        _ => false,
    }
}

impl<'a> Checker<'a> {
    pub fn cur(&mut self) -> &mut FnCtx {
        self.fstack.last_mut().expect("no function context")
    }
    pub fn cur_ref(&self) -> &FnCtx {
        self.fstack.last().expect("no function context")
    }
    pub fn manual(&self) -> bool {
        self.config.memory == MemoryMode::Manual
    }

    // ------------------------------------------------------------------ locals
    pub fn add_local(&mut self, name: &str, ty: Type, span: Span, immutable: bool, copied: bool) -> LocalId {
        let ctx = self.cur();
        let id = ctx.locals.len() as LocalId;
        ctx.locals.push(LocalInfo { name: name.to_string(), ty, cell: false, span });
        ctx.immutable.push(immutable);
        ctx.copied.push(copied);
        id
    }

    /// Declares a named local in the current scope, enforcing the no-redeclaration and
    /// no-shadowing rules (spec 5.1).
    pub fn declare(&mut self, name: &str, ty: Type, span: Span, immutable: bool, copied: bool, owned: bool) -> LocalId {
        if name != "_" {
            let mut exists = false;
            for ctx in self.fstack.iter().rev() {
                if ctx.scopes.iter().any(|s| s.names.iter().any(|(n, _)| n == name)) {
                    exists = true;
                    break;
                }
                if ctx.kind != FuncKind::Lambda {
                    break;
                }
            }
            if exists {
                self.err(span, format!("variable '{}' is already defined in this scope (redeclaration and shadowing are not allowed)", name));
            }
        }
        if copied && ty.is_copy() {
            self.warn(span, format!("'copied' has no effect on copy type {} and is removed", self.tname(&ty)));
        }
        let copied = copied && !ty.is_copy();
        let id = self.add_local(name, ty.clone(), span, immutable, copied);
        let ctx = self.cur();
        let scope = ctx.scopes.last_mut().unwrap();
        scope.names.push((name.to_string(), id));
        if owned && !ty.is_copy() && !matches!(ty, Type::Ref(..)) {
            scope.owned.push(id);
        }
        id
    }

    pub fn temp(&mut self, ty: Type, span: Span) -> LocalId {
        self.add_local("$tmp", ty, span, false, false)
    }

    pub fn push_scope(&mut self) {
        self.cur().scopes.push(Scope { names: Vec::new(), owned: Vec::new() });
    }

    pub fn pop_scope(&mut self) -> Vec<LocalId> {
        self.cur().scopes.pop().map(|s| s.owned).unwrap_or_default()
    }

    fn find_in_ctx(ctx: &FnCtx, name: &str) -> Option<LocalId> {
        for s in ctx.scopes.iter().rev() {
            for (n, id) in s.names.iter().rev() {
                if n == name {
                    return Some(*id);
                }
            }
        }
        None
    }

    /// Looks up a local, creating lambda captures through enclosing functions as needed.
    pub fn lookup_local(&mut self, name: &str) -> Option<LocalId> {
        let depth = self.fstack.len() - 1;
        self.lookup_local_at(depth, name)
    }

    fn lookup_local_at(&mut self, depth: usize, name: &str) -> Option<LocalId> {
        if let Some(id) = Self::find_in_ctx(&self.fstack[depth], name) {
            return Some(id);
        }
        if let Some(&(_, inner, _)) = self.fstack[depth].captures.iter().find(|(_, inner, _)| self.fstack[depth].locals[*inner as usize].name == name) {
            return Some(inner);
        }
        if self.fstack[depth].kind != FuncKind::Lambda || depth == 0 {
            return None;
        }
        let outer = self.lookup_local_at(depth - 1, name)?;
        let oinfo = self.fstack[depth - 1].locals[outer as usize].clone();
        let is_move = self.fstack[depth].is_move_lambda;
        let by_ref = !is_move && !matches!(oinfo.ty, Type::Ref(..)) && !self.manual();
        let ctx = &mut self.fstack[depth];
        let inner = ctx.locals.len() as LocalId;
        ctx.locals.push(LocalInfo { name: name.to_string(), ty: oinfo.ty.clone(), cell: by_ref, span: oinfo.span });
        let imm = self.fstack[depth - 1].immutable[outer as usize];
        let ctx = &mut self.fstack[depth];
        ctx.immutable.push(imm);
        ctx.copied.push(false);
        ctx.captures.push((outer, inner, by_ref));
        if by_ref {
            self.fstack[depth - 1].locals[outer as usize].cell = true;
        }
        Some(inner)
    }

    pub fn local_ty(&self, id: LocalId) -> Type {
        self.cur_ref().locals[id as usize].ty.clone()
    }

    pub fn narrowed(&self, id: LocalId) -> Option<Type> {
        for m in self.cur_ref().narrow.iter().rev() {
            if let Some(t) = m.get(&id) {
                return Some(t.clone());
            }
        }
        None
    }

    pub fn local_read(&mut self, id: LocalId, span: Span) -> HExpr {
        let ty = self.local_ty(id);
        let e = match &ty {
            Type::Ref(false, t) => HExpr::new(H::Local(id), (**t).clone(), span),
            Type::Ref(true, t) => HExpr::new(H::Deref(Box::new(HExpr::new(H::Local(id), ty.clone(), span))), (**t).clone(), span),
            _ => HExpr::new(H::Local(id), ty.clone(), span),
        };
        if let Some(nt) = self.narrowed(id) {
            return HExpr::new(H::Unwrap(Box::new(e)), nt, span);
        }
        e
    }

    pub fn this_expr(&mut self, span: Span) -> Option<HExpr> {
        let ctx = self.cur_ref();
        if ctx.is_static && ctx.kind != FuncKind::Lambda {
            return None;
        }
        let id = self.lookup_local("this")?;
        let ty = self.local_ty(id);
        Some(HExpr::new(H::Local(id), ty, span))
    }

    fn current_class(&self) -> Option<ClassId> {
        for ctx in self.fstack.iter().rev() {
            if ctx.class.is_some() {
                return ctx.class;
            }
            if ctx.kind != FuncKind::Lambda {
                break;
            }
        }
        None
    }

    fn in_static_context(&self) -> bool {
        for ctx in self.fstack.iter().rev() {
            if ctx.kind != FuncKind::Lambda {
                return ctx.is_static;
            }
        }
        true
    }

    fn current_module(&self) -> usize {
        self.cur_ref().module
    }

    // ------------------------------------------------------------------ conversions
    pub fn coerce(&mut self, e: HExpr, to: &Type, span: Span) -> HExpr {
        if e.ty == *to || to.is_error() || e.ty.is_error() {
            return e;
        }
        // auto-deref of a `*T` value used as T
        if let Type::Ref(true, inner) = &e.ty {
            if !matches!(to, Type::Ref(true, _)) {
                let inner = (**inner).clone();
                let sp = e.span;
                let d = HExpr::new(H::Deref(Box::new(e)), inner, sp);
                return self.coerce(d, to, span);
            }
        }
        if let Type::Ref(true, _) = to {
            let (f, t) = (self.tname(&e.ty), self.tname(to));
            self.err(span, format!("expected a mutable reference {}, found {} (pass it with '*x')", t, f));
            return e;
        }
        if let Type::Ref(false, inner) = to {
            if self.assignable(&e.ty, to) {
                if e.ty == **inner || matches!(e.ty, Type::Ref(..)) {
                    return e;
                }
                // borrowed view at a supertype: no conversion needed
                let mut e = e;
                e.ty = (**inner).clone();
                return e;
            }
        }
        if let Type::Ref(false, inner) = &e.ty {
            let inner = (**inner).clone();
            let mut e2 = e;
            e2.ty = inner;
            if !e2.ty.is_copy() && !matches!(to, Type::Ref(..)) && e2.ty != *to && !self.assignable(&e2.ty, to) {
                let (f, t) = (self.tname(&e2.ty), self.tname(to));
                self.err(span, format!("type mismatch: expected {}, found &{}", t, f));
                return e2;
            }
            return self.coerce(e2, to, span);
        }
        // element-wise conversion for tuple expressions
        if let (Type::Tuple(from), Type::Tuple(tt)) = (&e.ty, to) {
            if from.len() == tt.len() && self.assignable(&e.ty, to) {
                let tt = tt.clone();
                if let H::Tuple(items) = e.kind {
                    let items: Vec<HExpr> = items.into_iter().zip(tt.iter()).map(|(x, t)| self.coerce(x, t, span)).collect();
                    return HExpr::new(H::Tuple(items), to.clone(), e.span);
                }
                let sp = e.span;
                let from = from.clone();
                let tmp = self.temp(e.ty.clone(), sp);
                let mut items = Vec::new();
                for (i, t) in tt.iter().enumerate() {
                    let g = HExpr::new(H::TupleGet(Box::new(HExpr::new(H::Local(tmp), e.ty.clone(), sp)), i as u32), from[i].clone(), sp);
                    items.push(self.coerce(g, t, span));
                }
                let ety = e.ty.clone();
                let _ = ety;
                return HExpr::new(H::Seq(vec![hir::Stmt { kind: StmtKind::Let(tmp, Some(e)), span: sp }], Box::new(HExpr::new(H::Tuple(items), to.clone(), sp))), to.clone(), sp);
            }
        }
        if self.assignable(&e.ty, to) {
            let sp = e.span;
            return HExpr::new(H::Convert(Box::new(e)), to.clone(), sp);
        }
        let (f, t) = (self.tname(&e.ty), self.tname(to));
        let hint = if e.ty.is_numeric() && to.is_numeric() { " (use .castTo() for narrowing conversions)" } else { "" };
        let hint = if matches!(e.ty, Type::Nullable(_)) && !to.is_nullable() { " (use '!' to assert non-null or '??' for a default)" } else { hint };
        let hint = if matches!(e.ty, Type::Dyn) { " (use .castTo() to convert a DTVariable)" } else { hint };
        self.err(span, format!("type mismatch: expected {}, found {}{}", t, f, hint));
        e
    }

    /// Marks an expression as consumed by an owning context, turning local reads of move-type
    /// values into moves (or clones for `copied` variables) and rejecting moves out of borrows.
    pub fn consume(&mut self, e: HExpr) -> HExpr {
        if e.ty.is_copy() || self.manual() {
            return e;
        }
        let span = e.span;
        match e.kind {
            H::Local(id) => {
                let info = self.cur_ref().locals[id as usize].clone();
                if matches!(info.ty, Type::Ref(..)) {
                    self.err(span, format!("cannot move out of borrowed reference '{}'; use .clone()", info.name));
                    return HExpr::new(H::Local(id), e.ty, span);
                }
                if self.cur_ref().copied[id as usize] {
                    let ty = e.ty.clone();
                    return HExpr::new(H::Builtin(Builtin::Clone, vec![HExpr::new(H::Local(id), ty.clone(), span)]), ty, span);
                }
                HExpr::new(H::Move(id), e.ty, span)
            }
            H::Unwrap(inner) => {
                let ty = e.ty.clone();
                let inner = self.consume(*inner);
                HExpr::new(H::Unwrap(Box::new(inner)), ty, span)
            }
            H::Convert(inner) => {
                let ty = e.ty.clone();
                let inner = self.consume(*inner);
                HExpr::new(H::Convert(Box::new(inner)), ty, span)
            }
            H::Field(obj, idx) => {
                let name = match &obj.ty {
                    Type::Class(c) => self.classes[*c as usize].fields[idx as usize].name.clone(),
                    _ => "?".into(),
                };
                self.err(span, format!("cannot move out of field '{}'; use .clone()", name));
                HExpr::new(H::Field(obj, idx), e.ty, span)
            }
            H::Deref(inner) => {
                self.err(span, "cannot move out of a reference; use .clone()");
                HExpr::new(H::Deref(inner), e.ty, span)
            }
            H::Global(g) => {
                self.err(span, "cannot move out of a static field; use .clone()");
                HExpr::new(H::Global(g), e.ty, span)
            }
            H::Builtin(b @ (Builtin::ArrAt | Builtin::ArrFirst | Builtin::ArrLast | Builtin::DictGet), args) => {
                self.err(span, "cannot move an element out of a collection; use .clone() or .remove()");
                HExpr::new(H::Builtin(b, args), e.ty, span)
            }
            H::Call(f, args) if matches!(self.sigs.get(f as usize).map(|s| &s.ret), Some(Type::Ref(..))) => {
                self.err(span, "cannot move out of a borrowed value; use .clone()");
                HExpr::new(H::Call(f, args), e.ty, span)
            }
            H::CallVirtual(s, args) if matches!(self.selectors.get(s as usize).map(|s| &s.ret), Some(Type::Ref(..))) => {
                self.err(span, "cannot move out of a borrowed value; use .clone()");
                HExpr::new(H::CallVirtual(s, args), e.ty, span)
            }
            kind => HExpr::new(kind, e.ty, span),
        }
    }

    pub fn default_value_expr(&mut self, ty: &Type, span: Span) -> HExpr {
        let lit = match ty {
            Type::Int(_) => Lit::Int(0),
            Type::Float(_) => Lit::Float(0.0),
            Type::Bool => Lit::Bool(false),
            Type::Str => Lit::Str(String::new()),
            Type::Big => Lit::Big("0".into()),
            Type::Nullable(_) | Type::Dyn | Type::Null => Lit::Null,
            Type::Array(e) => {
                let def = self.default_value_expr(e, span);
                return HExpr::new(H::Builtin(Builtin::ArrNew, vec![def]), ty.clone(), span);
            }
            Type::Dict(_, _) => return HExpr::new(H::Dict(vec![]), ty.clone(), span),
            Type::Tuple(ts) => {
                let items = ts.iter().map(|t| self.default_value_expr(t, span)).collect();
                return HExpr::new(H::Tuple(items), ty.clone(), span);
            }
            _ => Lit::Null,
        };
        HExpr::new(H::Lit(lit), ty.clone(), span)
    }

    pub fn has_default(&self, ty: &Type) -> bool {
        match ty {
            Type::Int(_) | Type::Float(_) | Type::Bool | Type::Str | Type::Big | Type::Nullable(_) | Type::Dyn | Type::Array(_) | Type::Dict(_, _) => true,
            Type::Tuple(ts) => ts.iter().all(|t| self.has_default(t)),
            _ => false,
        }
    }

    // ------------------------------------------------------------------ checked exceptions
    pub fn note_throws(&mut self, classes: &[ClassId], span: Span) {
        for &c in classes {
            if !self.is_checked_exception(c) {
                continue;
            }
            let mut handled = false;
            for ctx in self.fstack.iter().rev() {
                if ctx.catch_stack.iter().any(|cs| cs.iter().any(|&k| self.is_subclass(c, k))) {
                    handled = true;
                    break;
                }
                if ctx.declared_throws.iter().any(|&k| self.is_subclass(c, k)) {
                    handled = true;
                    break;
                }
                // checked exceptions cannot escape a lambda body
                break;
            }
            if !handled {
                let n = self.classes[c as usize].name.clone();
                self.err(span, format!("unreported exception {}; it must be caught or declared to be thrown", n));
            }
        }
    }

    // ------------------------------------------------------------------ expressions
    pub fn expr(&mut self, e: &'a ast::Expr, expected: Option<&Type>) -> HExpr {
        let span = e.span;
        match &e.kind {
            A::Int(v) => self.int_literal(*v as i128, false, expected, span),
            A::Float(v) => {
                let ty = match expected.map(|t| t.deref().non_null()) {
                    Some(Type::Float(f)) => Type::Float(f),
                    _ => Type::Float(FloatTy::F64),
                };
                let v = match &ty {
                    Type::Float(f) => f.round(*v),
                    _ => *v,
                };
                HExpr::new(H::Lit(Lit::Float(v)), ty, span)
            }
            A::Str(s) => HExpr::new(H::Lit(Lit::Str(s.clone())), Type::Str, span),
            A::Bool(b) => HExpr::new(H::Lit(Lit::Bool(*b)), Type::Bool, span),
            A::Null => {
                let ty = match expected {
                    Some(t) if t.is_nullable() => t.clone(),
                    _ => Type::Null,
                };
                HExpr::new(H::Lit(Lit::Null), ty, span)
            }
            A::FStr(parts) => {
                let mut out = Vec::new();
                for p in parts {
                    match p {
                        ast::FStrPart::Lit(s) => out.push(HExpr::new(H::Lit(Lit::Str(s.clone())), Type::Str, span)),
                        ast::FStrPart::Expr(x) => {
                            let h = self.expr(x, None);
                            if h.ty == Type::Void {
                                self.err(x.span, "cannot interpolate a void expression");
                            }
                            out.push(h);
                        }
                    }
                }
                HExpr::new(H::Concat(out), Type::Str, span)
            }
            A::Ident(name) => self.ident(name, span),
            A::This => match self.this_expr(span) {
                Some(t) => t,
                None => {
                    self.err(span, "'this' is not available in a static context");
                    HExpr::new(H::Lit(Lit::Null), Type::Error, span)
                }
            },
            A::Super(_) => {
                self.err(span, "'super' can only be used to call a method or constructor");
                HExpr::new(H::Lit(Lit::Null), Type::Error, span)
            }
            A::Unary(op, inner) => self.unary(*op, inner, expected, span),
            A::Binary(op, a, b) => self.binary(*op, a, b, expected, span),
            A::Ternary(c, a, b) => {
                let c = self.expr(c, Some(&Type::Bool));
                let c = self.cond(c);
                let (x, y) = match expected {
                    Some(t) => {
                        let x = self.expr(a, Some(t));
                        let y = self.expr(b, Some(t));
                        (x, y)
                    }
                    None => {
                        if is_literalish(a) && !is_literalish(b) {
                            let y = self.expr(b, None);
                            let yt = y.ty.clone();
                            let x = self.expr(a, Some(&yt));
                            (x, y)
                        } else {
                            let x = self.expr(a, None);
                            let xt = x.ty.clone();
                            let y = self.expr(b, Some(&xt));
                            (x, y)
                        }
                    }
                };
                let ty = match expected {
                    Some(t) => t.clone(),
                    None => self.unify_branch(&x.ty, &y.ty, span),
                };
                let x = self.coerce(x, &ty, a.span);
                let y = self.coerce(y, &ty, b.span);
                HExpr::new(H::Ternary(Box::new(c), Box::new(x), Box::new(y)), ty, span)
            }
            A::Coalesce(a, b) => {
                let x = self.expr(a, expected.map(|t| t.clone().nullable()).as_ref());
                let inner = match &x.ty {
                    Type::Nullable(t) => (**t).clone(),
                    Type::Dyn => Type::Dyn,
                    Type::Null => expected.cloned().unwrap_or(Type::Error),
                    other => {
                        let n = self.tname(other);
                        self.err(span, format!("left side of '??' must be nullable, found {}", n));
                        other.clone()
                    }
                };
                let target = expected.cloned().unwrap_or(inner.clone());
                let y = self.expr(b, Some(&target));
                let rty = if y.ty.is_nullable() && !target.is_nullable() { target.clone().nullable() } else { target.clone() };
                let x = self.coerce(x, &rty.clone().nullable(), a.span);
                let y = self.coerce(y, &rty, b.span);
                HExpr::new(H::Coalesce(Box::new(x), Box::new(y)), rty, span)
            }
            A::NonNull(inner) => {
                let x = self.expr(inner, expected.map(|t| t.clone().nullable()).as_ref());
                let ty = match &x.ty {
                    Type::Nullable(t) => (**t).clone(),
                    Type::Tuple(ts) => Type::Tuple(ts.iter().map(|t| t.non_null()).collect()),
                    Type::Null => {
                        self.err(span, "'!' applied to null");
                        Type::Error
                    }
                    other => other.clone(),
                };
                HExpr::new(H::NonNull(Box::new(x)), ty, span)
            }
            A::Call(callee, args) => self.call(callee, args, expected, span),
            A::Member(obj, name) => self.member(obj, name, span),
            A::Index(base, idx) => self.index(base, idx, span),
            A::Dict(entries) => self.dict_literal(entries, expected, span),
            A::Tuple(items) => {
                let exp: Option<Vec<Type>> = match expected {
                    Some(Type::Tuple(ts)) if ts.len() == items.len() => Some(ts.clone()),
                    _ => None,
                };
                let mut out = Vec::new();
                let mut tys = Vec::new();
                for (i, it) in items.iter().enumerate() {
                    let t = exp.as_ref().map(|v| &v[i]);
                    let x = self.expr(it, t);
                    let x = match t {
                        Some(t) => self.coerce(x, t, it.span),
                        None => x,
                    };
                    let x = self.consume(x);
                    tys.push(x.ty.clone());
                    out.push(x);
                }
                HExpr::new(H::Tuple(out), Type::Tuple(tys), span)
            }
            A::Lambda { params, ret, body, is_move } => self.lambda(params, ret.as_ref(), body, *is_move, expected, span),
            A::TypeLit(_) => {
                self.err(span, "a type is not a value here");
                HExpr::new(H::Lit(Lit::Null), Type::Error, span)
            }
        }
    }

    pub fn cond(&mut self, c: HExpr) -> HExpr {
        if c.ty != Type::Bool && !c.ty.is_error() {
            let n = self.tname(&c.ty);
            let hint = if c.ty.is_nullable() { " (compare with null explicitly: x != null)" } else { "" };
            self.err(c.span, format!("condition must be Boolean, found {}{}", n, hint));
        }
        c
    }

    fn int_literal(&mut self, v: i128, _neg: bool, expected: Option<&Type>, span: Span) -> HExpr {
        let exp = match expected.map(|t| t.deref().non_null()) {
            Some(Type::Union(us)) => {
                if us.contains(&Type::int32()) && IntTy::I32.fits(v) {
                    Some(Type::int32())
                } else {
                    us.iter().find(|u| matches!(u, Type::Int(t) if t.fits(v))).or_else(|| us.iter().find(|u| matches!(u, Type::Float(_) | Type::Big))).cloned()
                }
            }
            other => other,
        };
        match exp {
            Some(Type::Int(t)) => {
                if !t.fits(v) {
                    self.err(span, format!("integer literal {} does not fit in {}", v, t.name()));
                }
                HExpr::new(H::Lit(Lit::Int(v)), Type::Int(t), span)
            }
            Some(Type::Float(f)) => HExpr::new(H::Lit(Lit::Float(f.round(v as f64))), Type::Float(f), span),
            Some(Type::Big) => HExpr::new(H::Lit(Lit::Big(v.to_string())), Type::Big, span),
            _ => {
                if IntTy::I32.fits(v) {
                    HExpr::new(H::Lit(Lit::Int(v)), Type::int32(), span)
                } else if IntTy::I64.fits(v) {
                    HExpr::new(H::Lit(Lit::Int(v)), Type::int64(), span)
                } else if IntTy::U64.fits(v) {
                    HExpr::new(H::Lit(Lit::Int(v)), Type::Int(IntTy::U64), span)
                } else {
                    HExpr::new(H::Lit(Lit::Big(v.to_string())), Type::Big, span)
                }
            }
        }
    }

    pub fn unify_branch(&mut self, a: &Type, b: &Type, span: Span) -> Type {
        if a == b {
            return a.clone();
        }
        if a.is_error() {
            return b.clone();
        }
        if b.is_error() {
            return a.clone();
        }
        if let Some(t) = common_numeric(a, b) {
            return t;
        }
        if *a == Type::Null {
            return b.clone().nullable();
        }
        if *b == Type::Null {
            return a.clone().nullable();
        }
        if self.assignable(a, b) {
            return b.clone();
        }
        if self.assignable(b, a) {
            return a.clone();
        }
        if let (Type::Class(x), Type::Class(y)) = (a, b) {
            let mut c = Some(*x);
            while let Some(cc) = c {
                if self.is_subclass(*y, cc) {
                    return Type::Class(cc);
                }
                c = self.classes[cc as usize].parent;
            }
        }
        let (x, y) = (self.tname(a), self.tname(b));
        self.err(span, format!("incompatible types {} and {}", x, y));
        Type::Error
    }

    fn ident(&mut self, name: &str, span: Span) -> HExpr {
        if let Some(id) = self.lookup_local(name) {
            return self.local_read(id, span);
        }
        // implicit field / static field of the current class
        if let Some(c) = self.current_class() {
            if let Some(idx) = self.field_index(c, name) {
                if self.in_static_context() {
                    self.err(span, format!("instance field '{}' cannot be used in a static context", name));
                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                }
                let this = self.this_expr(span).unwrap();
                let ty = self.classes[c as usize].fields[idx as usize].ty.clone();
                return HExpr::new(H::Field(Box::new(this), idx), ty, span);
            }
            if let Some(s) = self.find_static(c, name) {
                let ty = self.globals[s.global as usize].ty.clone();
                return HExpr::new(H::Global(s.global), ty, span);
            }
        }
        // function as a value
        let m = self.current_module();
        if let Some(fs) = self.lookup_funcs(m, name) {
            if fs.len() == 1 {
                if let Some(f) = fs[0].func {
                    let sig = self.sigs[f as usize].clone();
                    return HExpr::new(H::FuncRef(f), Type::Func(sig.params, Box::new(sig.ret)), span);
                }
            }
            self.err(span, format!("'{}' is overloaded or generic and cannot be used as a value here", name));
            return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
        }
        if self.class_decls.contains_key(name) || self.iface_decls.contains_key(name) || is_builtin_type_name(name) {
            self.err(span, format!("'{}' is a type, not a value", name));
        } else if self.modules[m].imports.contains_key(name) {
            self.err(span, format!("'{}' is a module, not a value", name));
        } else {
            self.err(span, format!("cannot find '{}' in this scope", name));
        }
        HExpr::new(H::Lit(Lit::Null), Type::Error, span)
    }

    pub fn lookup_funcs(&self, module: usize, name: &str) -> Option<Vec<FnRef<'a>>> {
        if let Some(v) = self.modules[module].funcs.get(name) {
            return Some(v.clone());
        }
        None
    }

    fn unary(&mut self, op: UnOp, inner: &'a ast::Expr, expected: Option<&Type>, span: Span) -> HExpr {
        match op {
            UnOp::Neg => {
                if let A::Int(v) = inner.kind {
                    return self.int_literal(-(v as i128), true, expected, span);
                }
                if let A::Float(v) = inner.kind {
                    let ty = match expected.map(|t| t.deref().non_null()) {
                        Some(Type::Float(f)) => Type::Float(f),
                        _ => Type::Float(FloatTy::F64),
                    };
                    let v = match &ty {
                        Type::Float(f) => f.round(-v),
                        _ => -v,
                    };
                    return HExpr::new(H::Lit(Lit::Float(v)), ty, span);
                }
                let x = self.expr(inner, expected);
                if !x.ty.is_numeric() && !x.ty.is_error() {
                    let n = self.tname(&x.ty);
                    self.err(span, format!("unary '-' needs a number, found {}", n));
                }
                if let Type::Int(t) = x.ty {
                    if !t.signed() {
                        self.err(span, format!("unary '-' cannot be applied to unsigned type {}", t.name()));
                    }
                }
                let ty = x.ty.clone();
                HExpr::new(H::Unary(UnaryOp::Neg, Box::new(x)), ty, span)
            }
            UnOp::Not => {
                let x = self.expr(inner, Some(&Type::Bool));
                if x.ty != Type::Bool && !x.ty.is_error() {
                    let n = self.tname(&x.ty);
                    self.err(span, format!("'not' / '!' needs a Boolean, found {} (use '~' for bitwise NOT)", n));
                }
                HExpr::new(H::Unary(UnaryOp::Not, Box::new(x)), Type::Bool, span)
            }
            UnOp::BitNot => {
                let x = self.expr(inner, expected);
                if !x.ty.is_integer() && !x.ty.is_error() {
                    let n = self.tname(&x.ty);
                    self.err(span, format!("'~' needs an integer, found {}", n));
                }
                let ty = x.ty.clone();
                HExpr::new(H::Unary(UnaryOp::BitNot, Box::new(x)), ty, span)
            }
            UnOp::Ref => {
                if self.manual() {
                    self.err(span, "'&' references cannot be used with MemoryManagement=manual");
                }
                let x = self.expr(inner, expected.map(|t| t.deref().clone()).as_ref());
                let ty = Type::Ref(false, Box::new(x.ty.clone()));
                HExpr { kind: x.kind, ty, span }
            }
            UnOp::RefMut => {
                if self.manual() {
                    self.err(span, "'*' references cannot be used with MemoryManagement=manual");
                }
                match self.place_of(inner) {
                    Some((place, ty, mutable, why)) => {
                        if !mutable {
                            self.err(span, format!("cannot take a mutable reference: {}", why));
                        }
                        if let Place::Local(id) = &place {
                            let id = *id;
                            self.cur().locals[id as usize].cell = true;
                        }
                        let rty = Type::Ref(true, Box::new(ty));
                        if let Place::Deref(id) = place {
                            // re-borrow of an existing mutable reference
                            let t = self.local_ty(id);
                            return HExpr::new(H::Local(id), t, span);
                        }
                        HExpr::new(H::RefMut(Box::new(place)), rty, span)
                    }
                    None => {
                        self.err(span, "'*' needs a variable or field");
                        HExpr::new(H::Lit(Lit::Null), Type::Error, span)
                    }
                }
            }
        }
    }

    /// Resolves an assignable location. Returns (place, type, mutable, reason-if-not).
    pub fn place_of(&mut self, e: &'a ast::Expr) -> Option<(Place, Type, bool, String)> {
        match &e.kind {
            A::Ident(name) => {
                if let Some(id) = self.lookup_local(name) {
                    let ty = self.local_ty(id);
                    return Some(match ty {
                        Type::Ref(true, t) => (Place::Deref(id), *t, true, String::new()),
                        Type::Ref(false, t) => (Place::Local(id), *t, false, format!("'{}' is a read-only reference", name)),
                        t => {
                            let imm = self.cur_ref().immutable[id as usize];
                            (Place::Local(id), t, !imm, format!("'{}' is Immutable", name))
                        }
                    });
                }
                if let Some(c) = self.current_class() {
                    if let Some(idx) = self.field_index(c, name) {
                        if self.in_static_context() {
                            return None;
                        }
                        let this = self.this_expr(e.span)?;
                        return Some(self.field_place(this, c, idx, name));
                    }
                    if let Some(s) = self.find_static(c, name) {
                        let ty = self.globals[s.global as usize].ty.clone();
                        let ok = !s.immutable || self.cur_ref().kind == FuncKind::Init;
                        return Some((Place::Global(s.global), ty, ok, format!("static field '{}' is Immutable", name)));
                    }
                }
                None
            }
            A::Member(obj, name) => {
                if let A::Ident(cn) = &obj.kind {
                    if self.lookup_local(cn).is_none() {
                        if let Some(c) = self.class_by_name(cn) {
                            if let Some(s) = self.find_static(c, name) {
                                self.check_access(s.access, c, e.span, name);
                                let ty = self.globals[s.global as usize].ty.clone();
                                return Some((Place::Global(s.global), ty, !s.immutable, format!("static field '{}' is Immutable", name)));
                            }
                        }
                    }
                }
                let o = self.expr(obj, None);
                if let Type::Class(c) = o.ty.deref().clone() {
                    if let Some(idx) = self.field_index(c, name) {
                        let fm = self.cmeta[c as usize].fields[idx as usize].clone();
                        self.check_access(fm.access, fm.owner, e.span, name);
                        return Some(self.field_place(o, c, idx, name));
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn field_place(&mut self, obj: HExpr, c: ClassId, idx: u32, name: &str) -> (Place, Type, bool, String) {
        let ty = self.classes[c as usize].fields[idx as usize].ty.clone();
        let fm = self.cmeta[c as usize].fields[idx as usize].clone();
        let is_this = matches!(obj.kind, H::Local(id) if Some(id) == self.cur_ref().this_local);
        let in_ctor = self.cur_ref().kind == FuncKind::Ctor && is_this;
        let ok = !fm.immutable || in_ctor;
        (Place::Field(Box::new(obj), idx), ty, ok, format!("field '{}' is Immutable", name))
    }

    pub fn check_access(&mut self, access: Access, owner: ClassId, span: Span, name: &str) {
        let cur = self.current_class();
        match access {
            Access::Private => {
                if cur != Some(owner) {
                    self.err(span, format!("'{}' is private", name));
                }
            }
            Access::Protected => {
                let ok = match cur {
                    Some(c) => self.is_subclass(c, owner),
                    None => false,
                } || self.cmeta[owner as usize].module == self.current_module();
                if !ok {
                    self.err(span, format!("'{}' is protected", name));
                }
            }
            _ => {}
        }
    }

    pub fn place_read(&mut self, p: &Place, ty: &Type, span: Span) -> HExpr {
        match p {
            Place::Local(id) => self.local_read(*id, span),
            Place::Deref(id) => {
                let t = self.local_ty(*id);
                HExpr::new(H::Deref(Box::new(HExpr::new(H::Local(*id), t, span))), ty.clone(), span)
            }
            Place::Field(o, idx) => HExpr::new(H::Field(o.clone(), *idx), ty.clone(), span),
            Place::Global(g) => HExpr::new(H::Global(*g), ty.clone(), span),
        }
    }

    fn binary(&mut self, op: BinOp, a: &'a ast::Expr, b: &'a ast::Expr, expected: Option<&Type>, span: Span) -> HExpr {
        match op {
            BinOp::And | BinOp::Or => {
                let x = self.expr(a, Some(&Type::Bool));
                let y = self.expr(b, Some(&Type::Bool));
                for z in [&x, &y] {
                    if z.ty != Type::Bool && !z.ty.is_error() {
                        let n = self.tname(&z.ty);
                        self.err(z.span, format!("'{}' needs Boolean operands, found {}", op.symbol(), n));
                    }
                }
                let k = if op == BinOp::And { H::And(Box::new(x), Box::new(y)) } else { H::Or(Box::new(x), Box::new(y)) };
                HExpr::new(k, Type::Bool, span)
            }
            _ => {
                // evaluate the non-literal side first so literals adapt to it
                let (x, y) = if is_literalish(a) && !is_literalish(b) {
                    let y = self.expr(b, None);
                    let yt = y.ty.clone();
                    let x = self.expr(a, Some(&yt));
                    (x, y)
                } else if !is_literalish(a) {
                    let x = self.expr(a, None);
                    let xt = x.ty.clone();
                    let y = if matches!(op, BinOp::Shl | BinOp::Shr) { self.expr(b, None) } else { self.expr(b, Some(&xt)) };
                    (x, y)
                } else {
                    let exp = expected.filter(|t| t.is_numeric());
                    let x = self.expr(a, exp);
                    let xt = x.ty.clone();
                    let y = self.expr(b, Some(&xt));
                    (x, y)
                };
                self.binary_typed(op, x, y, span)
            }
        }
    }

    /// Binary numeric promotion (Java rules): like `coerce`, but int -> float is allowed even
    /// when it may round.
    pub fn promote(&mut self, e: HExpr, to: &Type, span: Span) -> HExpr {
        let from = e.ty.deref().clone();
        if from != *to && from.is_numeric() && to.is_numeric() && !self.assignable(&from, to) && matches!(to, Type::Float(_)) {
            let e = self.coerce(e, &from, span);
            let sp = e.span;
            return HExpr::new(H::Convert(Box::new(e)), to.clone(), sp);
        }
        self.coerce(e, to, span)
    }

    pub fn binary_typed(&mut self, op: BinOp, x: HExpr, y: HExpr, span: Span) -> HExpr {
        let wrap = self.cur_ref().wrap;
        let (xt, yt) = (x.ty.deref().clone(), y.ty.deref().clone());
        if xt.is_error() || yt.is_error() {
            return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
        }
        let arith = |op: BinOp| match op {
            BinOp::Add => Some(ArithOp::Add),
            BinOp::Sub => Some(ArithOp::Sub),
            BinOp::Mul => Some(ArithOp::Mul),
            BinOp::Div => Some(ArithOp::Div),
            BinOp::Rem => Some(ArithOp::Rem),
            BinOp::Pow => Some(ArithOp::Pow),
            BinOp::BitAnd => Some(ArithOp::BitAnd),
            BinOp::BitOr => Some(ArithOp::BitOr),
            BinOp::BitXor => Some(ArithOp::BitXor),
            BinOp::Shl => Some(ArithOp::Shl),
            BinOp::Shr => Some(ArithOp::Shr),
            _ => None,
        };
        match op {
            BinOp::Add if xt == Type::Str || yt == Type::Str => {
                for z in [&x, &y] {
                    if z.ty == Type::Void {
                        self.err(z.span, "cannot concatenate a void value");
                    }
                }
                let mut parts = Vec::new();
                for z in [x, y] {
                    match z.kind {
                        H::Concat(ps) => parts.extend(ps),
                        k => parts.push(HExpr { kind: k, ty: z.ty, span: z.span }),
                    }
                }
                HExpr::new(H::Concat(parts), Type::Str, span)
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Pow => {
                let Some(ct) = common_numeric(&xt, &yt) else {
                    let (p, q) = (self.tname(&xt), self.tname(&yt));
                    self.err(span, format!("operator '{}' cannot be applied to {} and {}", op.symbol(), p, q));
                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                };
                let x = self.promote(x, &ct, span);
                let y = self.promote(y, &ct, span);
                HExpr::new(H::Arith(arith(op).unwrap(), Box::new(x), Box::new(y), wrap), ct, span)
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                if xt == Type::Bool || yt == Type::Bool {
                    let alt = if op == BinOp::BitAnd { "and" } else if op == BinOp::BitOr { "or" } else { "!=" };
                    self.err(span, format!("'{}' is a bitwise operator and cannot take Boolean operands; use '{}'", op.symbol(), alt));
                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                }
                let ct = match (&xt, &yt) {
                    (Type::Int(_) | Type::Big, Type::Int(_) | Type::Big) => common_numeric(&xt, &yt),
                    _ => None,
                };
                let Some(ct) = ct else {
                    let (p, q) = (self.tname(&xt), self.tname(&yt));
                    self.err(span, format!("operator '{}' needs integer operands, found {} and {}", op.symbol(), p, q));
                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                };
                let x = self.promote(x, &ct, span);
                let y = self.promote(y, &ct, span);
                HExpr::new(H::Arith(arith(op).unwrap(), Box::new(x), Box::new(y), wrap), ct, span)
            }
            BinOp::Shl | BinOp::Shr => {
                if !xt.is_integer() || !yt.is_integer() {
                    let (p, q) = (self.tname(&xt), self.tname(&yt));
                    self.err(span, format!("operator '{}' needs integer operands, found {} and {}", op.symbol(), p, q));
                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                }
                let x = self.coerce(x, &xt, span);
                let y = if xt == Type::Big {
                    self.coerce(y, &Type::Big, span)
                } else {
                    let yt2 = yt.clone();
                    self.coerce(y, &yt2, span)
                };
                HExpr::new(H::Arith(arith(op).unwrap(), Box::new(x), Box::new(y), wrap), xt, span)
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                let cop = match op {
                    BinOp::Eq => CmpOp::Eq,
                    BinOp::Ne => CmpOp::Ne,
                    BinOp::Lt => CmpOp::Lt,
                    BinOp::Gt => CmpOp::Gt,
                    BinOp::Le => CmpOp::Le,
                    _ => CmpOp::Ge,
                };
                let ordering = !matches!(cop, CmpOp::Eq | CmpOp::Ne);
                if let Some(ct) = common_numeric(&xt, &yt) {
                    let x = self.promote(x, &ct, span);
                    let y = self.promote(y, &ct, span);
                    return HExpr::new(H::Cmp(cop, Box::new(x), Box::new(y)), Type::Bool, span);
                }
                if ordering {
                    let ok = (xt == Type::Str && yt == Type::Str)
                        || (matches!((&xt, &yt), (Type::Class(a), Type::Class(b)) if a == b && self.classes[*a as usize].compare_fn.is_some()));
                    if !ok {
                        let (p, q) = (self.tname(&xt), self.tname(&yt));
                        self.err(span, format!("operator '{}' cannot compare {} and {}", op.symbol(), p, q));
                    }
                    return HExpr::new(H::Cmp(cop, Box::new(x), Box::new(y)), Type::Bool, span);
                }
                // equality
                let compatible = xt == yt
                    || xt == Type::Null
                    || yt == Type::Null
                    || self.assignable(&xt, &yt)
                    || self.assignable(&yt, &xt)
                    || matches!(xt, Type::Dyn | Type::Union(_))
                    || matches!(yt, Type::Dyn | Type::Union(_));
                if !compatible {
                    let (p, q) = (self.tname(&xt), self.tname(&yt));
                    self.err(span, format!("cannot compare {} with {} using '{}'", p, q, op.symbol()));
                }
                if (xt == Type::Null && !yt.is_nullable() && !matches!(yt, Type::Union(_))) || (yt == Type::Null && !xt.is_nullable() && !matches!(xt, Type::Union(_))) {
                    let n = self.tname(if xt == Type::Null { &yt } else { &xt });
                    self.warn(span, format!("comparison of non-nullable {} with null is always {}", n, cop == CmpOp::Ne));
                }
                HExpr::new(H::Cmp(cop, Box::new(x), Box::new(y)), Type::Bool, span)
            }
            BinOp::And | BinOp::Or => unreachable!(),
        }
    }

    fn dict_literal(&mut self, entries: &'a [(ast::Expr, ast::Expr)], expected: Option<&Type>, span: Span) -> HExpr {
        let exp = match expected.map(|t| t.deref().non_null()) {
            Some(Type::Union(us)) => us.into_iter().find(|u| matches!(u, Type::Dict(_, _))).or(Some(Type::Dyn)),
            other => other,
        };
        let (kt, vt) = match exp {
            Some(Type::Dict(k, v)) => (*k, *v),
            Some(Type::Dyn) | None => (Type::Dyn, Type::Dyn),
            Some(other) => {
                let n = self.tname(&other);
                self.err(span, format!("a dictionary literal cannot have type {}", n));
                (Type::Dyn, Type::Dyn)
            }
        };
        let mut out = Vec::new();
        for (k, v) in entries {
            let kx = self.expr(k, Some(&kt));
            let kx = self.coerce(kx, &kt, k.span);
            let kx = self.consume(kx);
            let vx = self.expr(v, Some(&vt));
            let vx = self.coerce(vx, &vt, v.span);
            let vx = self.consume(vx);
            out.push((kx, vx));
        }
        HExpr::new(H::Dict(out), Type::Dict(Box::new(kt), Box::new(vt)), span)
    }

    fn index(&mut self, base: &'a ast::Expr, idx: &'a [ast::Expr], span: Span) -> HExpr {
        if let A::Ident(n) = &base.kind {
            if self.lookup_local(n).is_none() && (self.class_decls.contains_key(n) || self.lookup_funcs(self.current_module(), n).is_some()) {
                self.err(span, format!("type arguments for '{}' must be followed by a call", n));
                return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
            }
        }
        let b = self.expr(base, None);
        if idx.len() != 1 {
            self.err(span, "expected exactly one index");
            return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
        }
        match b.ty.deref().clone() {
            Type::Array(et) => {
                let i = self.index_arg(&idx[0]);
                HExpr::new(H::Builtin(Builtin::ArrAt, vec![b, i]), *et, span)
            }
            Type::Dict(kt, vt) => {
                let k = self.expr(&idx[0], Some(&kt));
                let k = self.coerce(k, &kt, idx[0].span);
                HExpr::new(H::Builtin(Builtin::DictGet, vec![b, k]), *vt, span)
            }
            Type::Error => b,
            other => {
                let n = self.tname(&other);
                self.err(span, format!("type {} cannot be indexed", n));
                HExpr::new(H::Lit(Lit::Null), Type::Error, span)
            }
        }
    }

    pub fn index_arg(&mut self, a: &'a ast::Expr) -> HExpr {
        let i = self.expr(a, Some(&Type::int64()));
        if let Type::Int(t) = i.ty {
            if !t.signed() {
                self.err(a.span, format!("unsigned type {} cannot be used as an index (spec 13.1)", t.name()));
                return HExpr::new(H::Lit(Lit::Int(0)), Type::int64(), a.span);
            }
        }
        self.coerce(i, &Type::int64(), a.span)
    }

    fn member(&mut self, obj: &'a ast::Expr, name: &str, span: Span) -> HExpr {
        // static access: Class.field / Type.CONST
        if let A::Ident(cn) = &obj.kind {
            if self.lookup_local(cn).is_none() {
                if let Some(c) = self.class_by_name(cn) {
                    if let Some(s) = self.find_static(c, name) {
                        self.check_access(s.access, c, span, name);
                        let ty = self.globals[s.global as usize].ty.clone();
                        return HExpr::new(H::Global(s.global), ty, span);
                    }
                    self.err(span, format!("class '{}' has no static field '{}'", cn, name));
                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                }
                if is_builtin_type_name(cn) {
                    if let Some(e) = self.builtin_static_const(cn, name, span) {
                        return e;
                    }
                    self.err(span, format!("'{}' has no constant '{}'", cn, name));
                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                }
                if self.modules[self.current_module()].imports.contains_key(cn) {
                    self.err(span, format!("'{}.{}' must be called", cn, name));
                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                }
            }
        }
        let o = self.expr(obj, None);
        match o.ty.deref().clone() {
            Type::Class(c) => {
                if let Some(idx) = self.field_index(c, name) {
                    let fm = self.cmeta[c as usize].fields[idx as usize].clone();
                    self.check_access(fm.access, fm.owner, span, name);
                    let ty = self.classes[c as usize].fields[idx as usize].ty.clone();
                    return HExpr::new(H::Field(Box::new(o), idx), ty, span);
                }
                let cn = self.classes[c as usize].name.clone();
                self.err(span, format!("class '{}' has no field '{}' (call methods with '()')", cn, name));
            }
            Type::Error => return o,
            other => {
                let n = self.tname(&other);
                self.err(span, format!("type {} has no field '{}'", n, name));
            }
        }
        HExpr::new(H::Lit(Lit::Null), Type::Error, span)
    }

    // ------------------------------------------------------------------ lambdas
    fn lambda(
        &mut self,
        params: &'a [ast::Param],
        _ret: Option<&TypeExpr>,
        body: &'a ast::LambdaBody,
        is_move: bool,
        expected: Option<&Type>,
        span: Span,
    ) -> HExpr {
        let (exp_params, exp_ret) = match expected {
            Some(Type::Func(ps, r)) => (Some(ps.clone()), Some((**r).clone())),
            _ => (None, None),
        };
        if let Some(ps) = &exp_params {
            if ps.len() != params.len() {
                self.err(span, format!("lambda has {} parameter(s) but {} were expected", params.len(), ps.len()));
            }
        }
        let module = self.current_module();
        let subst = self.cur_ref().subst.clone();
        let wrap = self.cur_ref().wrap;
        let mut ptys = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let t = match &p.ty {
                TypeExpr::Wildcard(_) => match exp_params.as_ref().and_then(|ps| ps.get(i)) {
                    Some(t) => t.clone(),
                    None => {
                        self.err(p.span, format!("cannot infer the type of lambda parameter '{}'", p.name));
                        Type::Error
                    }
                },
                te => self.resolve_type(te, module, &subst),
            };
            ptys.push(t);
        }
        self.lambda_counter += 1;
        let name = format!("<lambda#{}>", self.lambda_counter);
        let sig = FuncSig { name: name.clone(), params: ptys.clone(), param_names: params.iter().map(|p| p.name.clone()).collect(), ret: exp_ret.clone().unwrap_or(Type::Void), throws: vec![] };
        let fid = self.new_func(name, FuncKind::Lambda, sig, wrap, None, span);
        let mut ctx = FnCtx::new(FuncKind::Lambda, module, wrap, subst);
        ctx.ret = exp_ret.clone();
        ctx.is_move_lambda = is_move;
        ctx.class = None;
        ctx.is_static = true;
        self.fstack.push(ctx);
        let mut pids = Vec::new();
        for (p, t) in params.iter().zip(ptys.iter()) {
            let id = self.declare(&p.name, t.clone(), p.span, p.mods.immutable, p.mods.copied, true);
            pids.push(id);
        }
        let body_stmts = match body {
            ast::LambdaBody::Block(b) => self.block_stmts(b),
            ast::LambdaBody::Expr(e) => {
                let x = self.expr(e, exp_ret.as_ref().filter(|t| **t != Type::Void));
                let rt = match &self.cur_ref().ret {
                    Some(t) => t.clone(),
                    None => x.ty.clone(),
                };
                if rt == Type::Void {
                    vec![hir::Stmt { kind: StmtKind::Expr(x), span: e.span }]
                } else {
                    self.cur().ret = Some(rt.clone());
                    let x = self.coerce(x, &rt, e.span);
                    let x = self.consume(x);
                    vec![hir::Stmt { kind: StmtKind::Return(Some(x)), span: e.span }]
                }
            }
        };
        let owned = self.pop_scope();
        let ctx = self.fstack.pop().unwrap();
        let ret = ctx.ret.clone().unwrap_or(Type::Void);
        let f = &mut self.funcs[fid as usize];
        f.params = pids;
        f.ret = ret.clone();
        f.locals = ctx.locals.clone();
        f.body = vec![hir::Stmt { kind: StmtKind::Block { body: body_stmts, drops: owned }, span }];
        f.captures = ctx.captures.iter().map(|(_, inner, by_ref)| Capture { inner: *inner, by_ref: *by_ref }).collect();
        self.sigs[fid as usize].ret = ret.clone();
        let mut srcs = Vec::new();
        for (outer, _, by_ref) in &ctx.captures {
            if *by_ref {
                srcs.push(CaptureSrc::Ref(Place::Local(*outer)));
            } else {
                let oty = self.local_ty(*outer);
                let read = HExpr::new(H::Local(*outer), oty.clone(), span);
                let v = if is_move && !matches!(oty, Type::Ref(..)) { self.consume(read) } else { read };
                srcs.push(CaptureSrc::Value(v));
            }
        }
        HExpr::new(H::Lambda(fid, srcs), Type::Func(ptys, Box::new(ret)), span)
    }

    // ------------------------------------------------------------------ calls
    /// Checks call arguments against a list of candidate signatures and picks the most specific.
    pub fn select_overload(&mut self, cands: &[Cand], args: &'a [ast::Arg], span: Span, what: &str) -> Option<(usize, Vec<HExpr>)> {
        let named = args.iter().filter(|a| a.name.is_some()).count();
        if named > 0 && named != args.len() {
            self.err(span, "named and positional arguments cannot be mixed in one call");
            return None;
        }
        let shape_ok = |c: &Cand| -> bool {
            if c.params.len() != args.len() {
                return false;
            }
            if named > 0 {
                return args.iter().zip(c.names.iter()).all(|(a, n)| a.name.as_deref() == Some(n.as_str()));
            }
            true
        };
        let viable: Vec<usize> = (0..cands.len()).filter(|&i| shape_ok(&cands[i])).collect();
        if viable.is_empty() {
            if named > 0 {
                if let Some(c) = cands.iter().find(|c| c.params.len() == args.len()) {
                    self.err(span, format!("argument labels of {} must exactly match its parameter names in order: ({})", what, c.names.join(", ")));
                    return None;
                }
            }
            let arities: Vec<String> = cands.iter().map(|c| c.params.len().to_string()).collect();
            self.err(span, format!("{} takes {} argument(s) but {} were given", what, arities.join(" or "), args.len()));
            return None;
        }
        if viable.len() == 1 {
            let c = &cands[viable[0]];
            let params = c.params.clone();
            let out = self.check_args_against(&params, args);
            return Some((viable[0], out));
        }
        // multiple candidates: type non-literal arguments first
        let mut pre: Vec<Option<HExpr>> = Vec::new();
        for a in args {
            if is_literalish(&a.value) {
                pre.push(None);
            } else {
                let x = self.arg_expr(&a.value);
                pre.push(Some(x));
            }
        }
        let mut applicable = Vec::new();
        for &ci in &viable {
            let c = &cands[ci];
            let ok = c.params.iter().enumerate().all(|(i, p)| match &pre[i] {
                Some(x) => self.assignable(&x.ty, p) || (matches!(p, Type::Ref(true, _)) && x.ty == *p),
                None => self.literal_fits(&args[i].value, p),
            });
            if ok {
                applicable.push(ci);
            }
        }
        if applicable.is_empty() {
            let tys: Vec<String> = pre.iter().map(|p| p.as_ref().map(|x| self.tname(&x.ty)).unwrap_or("literal".into())).collect();
            self.err(span, format!("no overload of {} accepts ({})", what, tys.join(", ")));
            return None;
        }
        let mut best = applicable[0];
        if applicable.len() > 1 {
            let more_specific = |s: &Self, a: usize, b: usize| cands[a].params.iter().zip(cands[b].params.iter()).all(|(x, y)| s.assignable(x, y));
            let mut winners: Vec<usize> = applicable.iter().copied().filter(|&a| applicable.iter().all(|&b| a == b || more_specific(self, a, b))).collect();
            if winners.len() != 1 {
                // tie-break on literal arguments: prefer the literal's own type family
                let rank = |p: &Type, e: &ast::Expr| -> u32 {
                    let is_int = matches!(e.kind, A::Int(_)) || matches!(&e.kind, A::Unary(UnOp::Neg, i) if matches!(i.kind, A::Int(_)));
                    match (p.deref().non_null(), is_int) {
                        (Type::Int(IntTy::I32), true) => 0,
                        (Type::Int(_), true) => 1,
                        (Type::Float(FloatTy::F64), false) => 0,
                        (Type::Float(_), _) => 2,
                        (Type::Big, _) => 3,
                        _ => 4,
                    }
                };
                let score = |ci: usize| -> u32 {
                    args.iter().enumerate().filter(|(i, _)| pre[*i].is_none()).map(|(i, a)| rank(&cands[ci].params[i], &a.value)).sum()
                };
                let best_score = applicable.iter().map(|&c| score(c)).min().unwrap_or(0);
                let lit_winners: Vec<usize> = applicable.iter().copied().filter(|&c| score(c) == best_score).collect();
                if lit_winners.len() == 1 {
                    winners = lit_winners;
                }
            }
            if winners.len() != 1 {
                self.err(span, format!("call to {} is ambiguous", what));
                return None;
            }
            best = winners[0];
        }
        let params = cands[best].params.clone();
        let mut out = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let x = match pre[i].take() {
                Some(x) => x,
                None => self.expr(&a.value, Some(&params[i])),
            };
            out.push(self.pass_arg(x, &params[i], a.value.span));
        }
        Some((best, out))
    }

    fn arg_expr(&mut self, e: &'a ast::Expr) -> HExpr {
        // a `*T` local passed on as a reference
        if let A::Ident(n) = &e.kind {
            if let Some(id) = self.lookup_local(n) {
                if let Type::Ref(true, _) = self.local_ty(id) {
                    let t = self.local_ty(id);
                    return HExpr::new(H::Local(id), t, e.span);
                }
            }
        }
        self.expr(e, None)
    }

    pub fn check_args_against(&mut self, params: &[Type], args: &'a [ast::Arg]) -> Vec<HExpr> {
        let mut out = Vec::new();
        for (a, p) in args.iter().zip(params.iter()) {
            let x = if matches!(p, Type::Ref(true, _)) { self.arg_expr(&a.value) } else { self.expr(&a.value, Some(p)) };
            out.push(self.pass_arg(x, p, a.value.span));
        }
        out
    }

    pub fn pass_arg(&mut self, x: HExpr, p: &Type, span: Span) -> HExpr {
        let x = self.coerce(x, p, span);
        if matches!(p, Type::Ref(..)) {
            x
        } else {
            self.consume(x)
        }
    }

    fn literal_fits(&self, e: &ast::Expr, p: &Type) -> bool {
        let p = p.deref();
        match &e.kind {
            A::Int(v) => self.int_lit_fits(*v as i128, p),
            A::Unary(UnOp::Neg, inner) => match inner.kind {
                A::Int(v) => self.int_lit_fits(-(v as i128), p),
                A::Float(_) => matches!(p.non_null(), Type::Float(_) | Type::Dyn) || matches!(p, Type::Union(us) if us.iter().any(|u| matches!(u, Type::Float(_)))),
                _ => false,
            },
            A::Float(_) => matches!(p.non_null(), Type::Float(_) | Type::Dyn) || matches!(p, Type::Union(us) if us.iter().any(|u| matches!(u, Type::Float(_)))),
            A::Null => p.is_nullable(),
            A::Dict(_) => matches!(p.non_null(), Type::Dict(_, _) | Type::Dyn),
            A::Lambda { params, .. } => matches!(p, Type::Func(ps, _) if ps.len() == params.len()),
            A::Tuple(items) => matches!(p, Type::Tuple(ts) if ts.len() == items.len()),
            _ => false,
        }
    }

    fn int_lit_fits(&self, v: i128, p: &Type) -> bool {
        match p.non_null() {
            Type::Int(t) => t.fits(v),
            Type::Float(_) | Type::Big | Type::Dyn => true,
            Type::Union(us) => us.iter().any(|u| self.int_lit_fits(v, u)),
            _ => false,
        }
    }

    fn method_cands(&self, ms: &[MethodInfo]) -> Vec<Cand> {
        ms.iter().map(|m| Cand { params: m.params.clone(), names: m.param_names.clone() }).collect()
    }

    pub fn call(&mut self, callee: &'a ast::Expr, args: &'a [ast::Arg], expected: Option<&Type>, span: Span) -> HExpr {
        match &callee.kind {
            A::Ident(name) => self.call_ident(name, args, expected, span),
            A::Index(base, targs) => {
                if let A::Ident(name) = &base.kind {
                    if self.lookup_local(name).is_none() {
                        let mut tys = Vec::new();
                        let module = self.current_module();
                        let subst = self.cur_ref().subst.clone();
                        for t in targs {
                            match crate::parser::expr_to_type(t) {
                                Some(te) => tys.push(self.resolve_type(&te, module, &subst)),
                                None => {
                                    self.err(t.span, "expected a type argument");
                                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                                }
                            }
                        }
                        if self.class_decls.contains_key(name) {
                            return match self.instantiate_class(name, tys, span) {
                                Some(c) => self.construct(c, args, span),
                                None => HExpr::new(H::Lit(Lit::Null), Type::Error, span),
                            };
                        }
                        if let Some(fs) = self.lookup_funcs(module, name) {
                            let generic: Vec<FnRef<'a>> = fs.into_iter().filter(|f| f.func.is_none()).collect();
                            if generic.len() == 1 {
                                let fr = generic[0].clone();
                                if let Some(fid) = self.instantiate_generic(&fr, tys, span) {
                                    return self.call_func(fid, args, span);
                                }
                                return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                            }
                        }
                    }
                }
                let f = self.expr(callee, None);
                self.call_value(f, args, span)
            }
            A::Member(obj, name) => self.call_member(obj, name, args, expected, span),
            A::Super(None) => {
                self.err(span, "super(...) must be the first statement of a constructor");
                HExpr::new(H::Lit(Lit::Null), Type::Error, span)
            }
            _ => {
                let f = self.expr(callee, None);
                self.call_value(f, args, span)
            }
        }
    }

    fn call_value(&mut self, f: HExpr, args: &'a [ast::Arg], span: Span) -> HExpr {
        match f.ty.deref().clone() {
            Type::Func(ps, r) => {
                if args.len() != ps.len() {
                    self.err(span, format!("function value takes {} argument(s) but {} were given", ps.len(), args.len()));
                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                }
                if args.iter().any(|a| a.name.is_some()) {
                    self.err(span, "named arguments cannot be used when calling a function value");
                }
                let out = self.check_args_against(&ps, args);
                HExpr::new(H::CallClosure(Box::new(f), out), *r, span)
            }
            Type::Error => f,
            other => {
                let n = self.tname(&other);
                self.err(span, format!("type {} is not callable", n));
                HExpr::new(H::Lit(Lit::Null), Type::Error, span)
            }
        }
    }

    pub fn call_func(&mut self, fid: FuncId, args: &'a [ast::Arg], span: Span) -> HExpr {
        let sig = self.sigs[fid as usize].clone();
        let cands = vec![Cand { params: sig.params.clone(), names: sig.param_names.clone() }];
        let what = format!("'{}'", sig.name);
        match self.select_overload(&cands, args, span, &what) {
            Some((_, out)) => {
                self.note_throws(&sig.throws, span);
                HExpr::new(H::Call(fid, out), sig.ret.clone(), span)
            }
            None => HExpr::new(H::Lit(Lit::Null), sig.ret.clone(), span),
        }
    }

    fn call_ident(&mut self, name: &str, args: &'a [ast::Arg], expected: Option<&Type>, span: Span) -> HExpr {
        // local function value
        if let Some(id) = self.lookup_local(name) {
            let f = self.local_read(id, span);
            return self.call_value(f, args, span);
        }
        // unqualified method of the current class
        if let Some(c) = self.current_class() {
            let ms = self.lookup_methods(Owner::Class(c), name);
            if !ms.is_empty() {
                let this = if self.in_static_context() { None } else { self.this_expr(span) };
                return self.invoke_methods(ms, this, args, span, false);
            }
        }
        if let Some(i) = self.cur_ref().iface {
            let ms = self.lookup_methods(Owner::Iface(i), name);
            if !ms.is_empty() {
                let this = self.this_expr(span);
                return self.invoke_methods(ms, this, args, span, false);
            }
        }
        let module = self.current_module();
        if let Some(fs) = self.lookup_funcs(module, name) {
            return self.call_overloads(fs, args, span, name);
        }
        if self.class_decls.contains_key(name) {
            let (_, decl) = self.class_decls[name];
            if decl.type_params.is_empty() {
                let c = self.class_by_name(name).unwrap();
                return self.construct(c, args, span);
            }
            // infer generic class arguments from the expected type
            if let Some(Type::Class(c)) = expected.map(|t| t.deref().non_null()) {
                if self.cmeta[c as usize].template == name {
                    return self.construct(c, args, span);
                }
            }
            return self.construct_generic_inferred(name, args, span);
        }
        if name == "free" {
            return self.free_call(args, span);
        }
        if self.iface_decls.contains_key(name) {
            self.err(span, format!("interface '{}' cannot be instantiated", name));
        } else {
            self.err(span, format!("cannot find function '{}'", name));
        }
        HExpr::new(H::Lit(Lit::Null), Type::Error, span)
    }

    fn free_call(&mut self, args: &'a [ast::Arg], span: Span) -> HExpr {
        if !self.manual() {
            self.err(span, "free() is only available with MemoryManagement=manual (ownership mode frees automatically)");
        }
        if args.len() != 1 {
            self.err(span, "free() takes one argument");
            return HExpr::new(H::Lit(Lit::Void), Type::Void, span);
        }
        let x = self.expr(&args[0].value, None);
        HExpr::new(H::Seq(vec![hir::Stmt { kind: StmtKind::Free(x), span }], Box::new(HExpr::new(H::Lit(Lit::Void), Type::Void, span))), Type::Void, span)
    }

    pub fn call_overloads(&mut self, fs: Vec<FnRef<'a>>, args: &'a [ast::Arg], span: Span, name: &str) -> HExpr {
        let concrete: Vec<FnRef<'a>> = fs.iter().filter(|f| f.func.is_some()).cloned().collect();
        let generic: Vec<FnRef<'a>> = fs.iter().filter(|f| f.func.is_none()).cloned().collect();
        if !concrete.is_empty() && (generic.is_empty() || self.any_applicable(&concrete, args)) {
            let cands: Vec<Cand> = concrete
                .iter()
                .map(|f| {
                    let s = &self.sigs[f.func.unwrap() as usize];
                    Cand { params: s.params.clone(), names: s.param_names.clone() }
                })
                .collect();
            let what = format!("'{}'", name);
            return match self.select_overload(&cands, args, span, &what) {
                Some((i, out)) => {
                    let fid = concrete[i].func.unwrap();
                    let sig = self.sigs[fid as usize].clone();
                    self.note_throws(&sig.throws, span);
                    HExpr::new(H::Call(fid, out), sig.ret, span)
                }
                None => HExpr::new(H::Lit(Lit::Null), Type::Error, span),
            };
        }
        if generic.len() > 1 {
            self.err(span, format!("overloaded generic functions '{}' are not supported", name));
            return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
        }
        let fr = generic[0].clone();
        // infer type arguments from argument types
        let tparams = generic_params(fr.decl);
        let names: Vec<String> = tparams.iter().map(|t| t.0.clone()).collect();
        let decl_params = wildcard_to_params(&fr.decl.params);
        if decl_params.len() != args.len() {
            self.err(span, format!("'{}' takes {} argument(s) but {} were given", name, decl_params.len(), args.len()));
            return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
        }
        let mut bind = Subst::new();
        let mut pre = Vec::new();
        for (a, p) in args.iter().zip(decl_params.iter()) {
            let x = self.arg_expr(&a.value);
            self.unify(&p.ty, &x.ty, &names, &mut bind);
            pre.push(x);
        }
        let mut targs = Vec::new();
        for n in &names {
            match bind.get(n) {
                Some(t) => targs.push(t.clone()),
                None => {
                    self.err(span, format!("cannot infer type argument '{}' of '{}'; give it explicitly: {}[...](...)", n, name, name));
                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                }
            }
        }
        let Some(fid) = self.instantiate_generic(&fr, targs, span) else {
            return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
        };
        let sig = self.sigs[fid as usize].clone();
        let mut out = Vec::new();
        for (x, p) in pre.into_iter().zip(sig.params.iter()) {
            let sp = x.span;
            out.push(self.pass_arg(x, p, sp));
        }
        self.note_throws(&sig.throws, span);
        HExpr::new(H::Call(fid, out), sig.ret, span)
    }

    fn any_applicable(&self, fs: &[FnRef<'a>], args: &'a [ast::Arg]) -> bool {
        fs.iter().any(|f| self.sigs[f.func.unwrap() as usize].params.len() == args.len())
    }

    /// Binds type parameters by matching a parameter type expression against an argument type.
    pub fn unify(&mut self, te: &TypeExpr, ty: &Type, names: &[String], bind: &mut Subst) {
        let ty = ty.deref();
        match te {
            TypeExpr::Named { name, args, .. } => {
                if names.contains(name) && args.is_empty() {
                    if !bind.contains_key(name) && !ty.is_error() && *ty != Type::Null {
                        bind.insert(name.clone(), ty.clone());
                    }
                    return;
                }
                if name == "Dictionary" && args.len() == 2 {
                    if let Type::Dict(k, v) = ty {
                        self.unify(&args[0], k, names, bind);
                        self.unify(&args[1], v, names, bind);
                    }
                    return;
                }
                if let Type::Class(c) = ty {
                    if self.cmeta[*c as usize].template == *name {
                        let targs = self.cmeta[*c as usize].targs.clone();
                        for (a, t) in args.iter().zip(targs.iter()) {
                            self.unify(a, t, names, bind);
                        }
                    }
                }
                if let Type::Iface(i) = ty {
                    if self.imeta[*i as usize].template == *name {
                        let targs = self.imeta[*i as usize].targs.clone();
                        for (a, t) in args.iter().zip(targs.iter()) {
                            self.unify(a, t, names, bind);
                        }
                    }
                }
            }
            TypeExpr::Array(e) => {
                if let Type::Array(t) = ty {
                    self.unify(e, t, names, bind);
                }
            }
            TypeExpr::Nullable(e) => self.unify(e, &ty.non_null(), names, bind),
            TypeExpr::Ref { inner, .. } => self.unify(inner, ty, names, bind),
            TypeExpr::Tuple(es) => {
                if let Type::Tuple(ts) = ty {
                    for (e, t) in es.iter().zip(ts.iter()) {
                        self.unify(e, t, names, bind);
                    }
                }
            }
            TypeExpr::Func(ps, r) => {
                if let Type::Func(tps, tr) = ty {
                    for (e, t) in ps.iter().zip(tps.iter()) {
                        self.unify(e, t, names, bind);
                    }
                    self.unify(r, tr, names, bind);
                }
            }
            _ => {}
        }
    }

    fn construct_generic_inferred(&mut self, name: &str, args: &'a [ast::Arg], span: Span) -> HExpr {
        let (_, decl) = self.class_decls[name];
        let names: Vec<String> = decl.type_params.iter().map(|t| t.name.clone()).collect();
        let ctor = decl.ctors.iter().find(|c| c.params.len() == args.len());
        let Some(ctor) = ctor else {
            self.err(span, format!("cannot infer type arguments of '{}'; write {}[...](...)", name, name));
            return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
        };
        let mut bind = Subst::new();
        for (a, p) in args.iter().zip(ctor.params.iter()) {
            if is_literalish(&a.value) && !matches!(a.value.kind, A::Int(_) | A::Float(_)) {
                continue;
            }
            // type the argument in a throw-away way only for simple expressions
            let x = self.expr_probe(&a.value);
            if let Some(t) = x {
                self.unify(&p.ty, &t, &names, &mut bind);
            }
        }
        let mut targs = Vec::new();
        for n in &names {
            match bind.get(n) {
                Some(t) => targs.push(t.clone()),
                None => {
                    self.err(span, format!("cannot infer type argument '{}' of '{}'; write {}[...](...)", n, name, name));
                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                }
            }
        }
        match self.instantiate_class(name, targs, span) {
            Some(c) => self.construct(c, args, span),
            None => HExpr::new(H::Lit(Lit::Null), Type::Error, span),
        }
    }

    /// Computes the natural type of a side-effect-free expression without emitting HIR.
    fn expr_probe(&mut self, e: &'a ast::Expr) -> Option<Type> {
        match &e.kind {
            A::Int(v) => Some(if IntTy::I32.fits(*v as i128) { Type::int32() } else { Type::int64() }),
            A::Float(_) => Some(Type::Float(FloatTy::F64)),
            A::Str(_) | A::FStr(_) => Some(Type::Str),
            A::Bool(_) => Some(Type::Bool),
            A::Ident(n) => {
                let id = self.lookup_local(n)?;
                Some(self.local_ty(id).deref().clone())
            }
            _ => {
                let nd = self.diags.len();
                let nf = self.funcs.len();
                let x = self.expr(e, None);
                // discard: only safe when nothing was created
                if self.funcs.len() != nf {
                    return None;
                }
                self.diags.truncate(nd);
                Some(x.ty)
            }
        }
    }

    pub fn construct(&mut self, c: ClassId, args: &'a [ast::Arg], span: Span) -> HExpr {
        let ctors = self.cmeta[c as usize].ctors.clone();
        let cands: Vec<Cand> = ctors.iter().map(|(_, p, n, _, _)| Cand { params: p.clone(), names: n.clone() }).collect();
        let cname = self.classes[c as usize].name.clone();
        let what = format!("constructor of '{}'", cname);
        match self.select_overload(&cands, args, span, &what) {
            Some((i, out)) => {
                let (fid, _, _, access, throws) = ctors[i].clone();
                self.check_access(access, c, span, &cname);
                self.note_throws(&throws, span);
                HExpr::new(H::New(c, fid, out), Type::Class(c), span)
            }
            None => HExpr::new(H::Lit(Lit::Null), Type::Class(c), span),
        }
    }

    /// Calls one of the given methods (overload set) on `recv` (None for static calls).
    pub fn invoke_methods(&mut self, ms: Vec<MethodInfo>, recv: Option<HExpr>, args: &'a [ast::Arg], span: Span, direct: bool) -> HExpr {
        let cands = self.method_cands(&ms);
        let what = format!("method '{}'", ms[0].name);
        let Some((i, out)) = self.select_overload(&cands, args, span, &what) else {
            return HExpr::new(H::Lit(Lit::Null), ms[0].ret.clone(), span);
        };
        let m = ms[i].clone();
        if let Owner::Class(oc) = m.owner {
            self.check_access(m.access, oc, span, &m.name);
        }
        if m.name == "drop" && m.params.is_empty() {
            self.err(span, "drop() is called automatically and cannot be called directly");
        }
        self.note_throws(&m.throws, span);
        if m.is_static {
            let f = m.func.unwrap();
            return HExpr::new(H::Call(f, out), m.ret.clone(), span);
        }
        let Some(recv) = recv else {
            self.err(span, format!("instance method '{}' cannot be called from a static context", m.name));
            return HExpr::new(H::Lit(Lit::Null), m.ret.clone(), span);
        };
        let mut all = vec![recv];
        all.extend(out);
        match (m.selector, direct) {
            (Some(sel), false) => HExpr::new(H::CallVirtual(sel, all), m.ret.clone(), span),
            _ => match m.func {
                Some(f) => HExpr::new(H::Call(f, all), m.ret.clone(), span),
                None => {
                    self.err(span, format!("method '{}' has no implementation", m.name));
                    HExpr::new(H::Lit(Lit::Null), m.ret.clone(), span)
                }
            },
        }
    }

    fn call_member(&mut self, obj: &'a ast::Expr, name: &str, args: &'a [ast::Arg], expected: Option<&Type>, span: Span) -> HExpr {
        // super.method(...) / super(Iface).method(...)
        if let A::Super(which) = &obj.kind {
            let this = match self.this_expr(span) {
                Some(t) => t,
                None => {
                    self.err(span, "'super' is not available here");
                    return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                }
            };
            let Some(c) = self.current_class() else {
                self.err(span, "'super' is only available in classes");
                return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
            };
            let ms = match which {
                None => match self.classes[c as usize].parent {
                    Some(p) => self.lookup_methods(Owner::Class(p), name),
                    None => Vec::new(),
                },
                Some(iname) => {
                    let module = self.current_module();
                    match self.resolve_type(&TypeExpr::named(iname, span), module, &Subst::new()) {
                        Type::Iface(i) => self.lookup_methods(Owner::Iface(i), name).into_iter().filter(|m| m.func.is_some()).collect(),
                        _ => {
                            self.err(span, format!("'{}' is not an interface", iname));
                            return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                        }
                    }
                }
            };
            if ms.is_empty() {
                self.err(span, format!("no inherited method '{}' to call", name));
                return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
            }
            return self.invoke_methods(ms, Some(this), args, span, true);
        }
        if let A::Ident(n) = &obj.kind {
            if self.lookup_local(n).is_none() {
                let module = self.current_module();
                // module-qualified call
                if let Some(imp) = self.modules[module].imports.get(n).copied() {
                    if self.current_class().and_then(|c| self.field_index(c, n)).is_none() {
                        return match imp {
                            Import::Stdio => self.stdio_call(name, args, span),
                            Import::Module(mi) => {
                                if let Some(fs) = self.lookup_funcs(mi, name) {
                                    let fs: Vec<FnRef<'a>> = fs;
                                    self.call_overloads(fs, args, span, name)
                                } else if let Some(c) = self.class_by_name(name) {
                                    self.construct(c, args, span)
                                } else {
                                    self.err(span, format!("module '{}' has no function '{}'", n, name));
                                    HExpr::new(H::Lit(Lit::Null), Type::Error, span)
                                }
                            }
                        };
                    }
                }
                // static method call on a class
                if self.class_decls.contains_key(n) && self.current_class().and_then(|c| self.field_index(c, n)).is_none() {
                    if let Some(c) = self.class_by_name(n) {
                        let ms: Vec<MethodInfo> = self.lookup_methods(Owner::Class(c), name).into_iter().filter(|m| m.is_static).collect();
                        if ms.is_empty() {
                            if name == "array" {
                                return self.array_new(Type::Class(c), args, span);
                            }
                            self.err(span, format!("class '{}' has no static method '{}'", n, name));
                            return HExpr::new(H::Lit(Lit::Null), Type::Error, span);
                        }
                        return self.invoke_methods(ms, None, args, span, true);
                    }
                }
                if self.iface_decls.contains_key(n) {
                    if let Some(i) = self.iface_inst.get(&(n.to_string(), vec![])).copied() {
                        let ms: Vec<MethodInfo> = self.lookup_methods(Owner::Iface(i), name).into_iter().filter(|m| m.is_static).collect();
                        if !ms.is_empty() {
                            return self.invoke_methods(ms, None, args, span, true);
                        }
                    }
                }
                if is_builtin_type_name(n) {
                    return self.builtin_static_call(n, name, args, expected, span);
                }
            }
        }
        // `.format(...)` on a literal: placeholders only from the literal text (spec 11.1)
        if name == "format" && matches!(obj.kind, A::Str(_) | A::FStr(_)) {
            return self.literal_format(obj, args, span);
        }
        let recv = self.expr(obj, None);
        self.method_on_value(obj, recv, name, args, expected, span)
    }

    pub fn method_on_value(&mut self, obj: &'a ast::Expr, recv: HExpr, name: &str, args: &'a [ast::Arg], expected: Option<&Type>, span: Span) -> HExpr {
        let rty = recv.ty.deref().clone();
        match &rty {
            Type::Class(c) => {
                let ms = self.lookup_methods(Owner::Class(*c), name);
                if !ms.is_empty() {
                    let ms: Vec<MethodInfo> = ms.into_iter().filter(|m| !m.is_static || true).collect();
                    return self.invoke_methods(ms, Some(recv), args, span, false);
                }
            }
            Type::Iface(i) => {
                let ms = self.lookup_methods(Owner::Iface(*i), name);
                if !ms.is_empty() {
                    return self.invoke_methods(ms, Some(recv), args, span, false);
                }
            }
            Type::Func(ps, r) if name == "run" => {
                let (ps, r) = (ps.clone(), (**r).clone());
                if args.len() != ps.len() {
                    self.err(span, format!("function value takes {} argument(s) but {} were given", ps.len(), args.len()));
                    return HExpr::new(H::Lit(Lit::Null), r, span);
                }
                let out = self.check_args_against(&ps, args);
                return HExpr::new(H::CallClosure(Box::new(recv), out), r, span);
            }
            Type::Error => return recv,
            _ => {}
        }
        if let Some(e) = self.builtin_method(obj, recv.clone(), name, args, expected, span) {
            return e;
        }
        let n = self.tname(&rty);
        self.err(span, format!("type {} has no method '{}'", n, name));
        HExpr::new(H::Lit(Lit::Null), Type::Error, span)
    }
}
