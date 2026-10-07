//! Array methods added on top of the core set (spec 13.3): growing and shrinking, slicing and
//! searching, and the higher-order methods (`map`, `filter`, `reduce`, ...), which are lowered
//! to loops that call the function argument.

use super::*;
use crate::ast::ExprKind as A;
use crate::hir::{Expr as HExpr, ExprKind as H, Stmt as HStmt};
use l2_runtime::ops::{ArithOp, CmpOp};
use l2_runtime::Builtin;

fn st(kind: StmtKind, span: Span) -> HStmt {
    HStmt { kind, span }
}

/// How an element reaches the function argument.
#[derive(Clone, Copy, PartialEq)]
enum Pass {
    /// A copy-type element by value.
    Val,
    /// A move-type element cloned into a by-value parameter.
    Clone,
    /// Borrowed (`&T` parameter, or an untyped parameter for move-type elements).
    Ref,
}

pub const ARRAY_METHODS: &[&str] = &[
    "add", "append", "push", "addAll", "pop", "clear", "slice", "indexOf", "lastIndexOf", "concat", "join", "map", "filter", "reduce", "forEach", "find", "findIndex", "any", "all", "count", "sortBy", "swap", "__permute",
];

impl<'a> Checker<'a> {
    fn lit_i64(&self, v: i128, span: Span) -> HExpr {
        HExpr::new(H::Lit(Lit::Int(v)), Type::int64(), span)
    }

    pub fn array_method(&mut self, obj: &'a ast::Expr, recv: HExpr, et: Type, name: &str, args: &'a [ast::Arg], span: Span) -> Option<HExpr> {
        let aty = Type::Array(Box::new(et.clone()));
        let what = format!("{}.{}", self.tname(&aty), name);
        let b = |b: Builtin, args: Vec<HExpr>, ty: Type| HExpr::new(H::Builtin(b, args), ty, span);
        Some(match name {
            "add" | "append" | "push" => {
                let a = self.bargs(args, &[members::P::Val(et.clone())], 1, &what, span)?;
                let place = self.mut_place_pub(obj, name, span)?;
                HExpr::new(H::BuiltinMut(Builtin::ArrPush, Box::new(place), a), Type::Void, span)
            }
            "addAll" => {
                let a = self.bargs(args, &[members::P::Borrow(aty.clone())], 1, &what, span)?;
                let place = self.mut_place_pub(obj, name, span)?;
                HExpr::new(H::BuiltinMut(Builtin::ArrAddAll, Box::new(place), a), Type::Void, span)
            }
            "pop" | "clear" => {
                self.bargs(args, &[], 0, &what, span)?;
                let place = self.mut_place_pub(obj, name, span)?;
                let (bi, ty) = if name == "pop" { (Builtin::ArrPop, et) } else { (Builtin::ArrClear, Type::Void) };
                HExpr::new(H::BuiltinMut(bi, Box::new(place), vec![]), ty, span)
            }
            "slice" => {
                let mut a = self.bargs(args, &[members::P::Idx, members::P::Idx], 1, &what, span)?;
                a.insert(0, recv);
                b(Builtin::ArrSlice, a, aty)
            }
            "indexOf" | "lastIndexOf" => {
                let mut a = self.bargs(args, &[members::P::Borrow(et)], 1, &what, span)?;
                a.insert(0, recv);
                a.push(HExpr::new(H::Lit(Lit::Bool(name == "lastIndexOf")), Type::Bool, span));
                b(Builtin::ArrIndexOf, a, Type::int64())
            }
            "concat" => {
                let mut a = self.bargs(args, &[members::P::Borrow(aty.clone())], 1, &what, span)?;
                a.insert(0, recv);
                b(Builtin::ArrConcat, a, aty)
            }
            "join" => {
                let mut a = self.bargs(args, &[members::P::Borrow(Type::Str)], 0, &what, span)?;
                a.insert(0, recv);
                if a.len() == 1 {
                    a.push(HExpr::new(H::Lit(Lit::Str(", ".into())), Type::Str, span));
                }
                b(Builtin::ArrJoin, a, Type::Str)
            }
            "sortBy" => return self.sort_by(obj, et, args, span, &what),
            "swap" => {
                let a = self.bargs(args, &[members::P::Idx, members::P::Idx], 2, &what, span)?;
                let place = self.mut_place_pub(obj, name, span)?;
                HExpr::new(H::BuiltinMut(Builtin::ArrSwap, Box::new(place), a), Type::Void, span)
            }
            "__permute" => {
                let a = self.bargs(args, &[members::P::Borrow(Type::Array(Box::new(Type::int64())))], 1, &what, span)?;
                let place = self.mut_place_pub(obj, name, span)?;
                HExpr::new(H::BuiltinMut(Builtin::ArrPermute, Box::new(place), a), Type::Void, span)
            }
            _ => return self.array_hof(recv, et, name, args, span, &what),
        })
    }

    /// Element passing for the element parameter of a lambda / function value.
    fn elem_pass(&self, et: &Type, declared: Option<&TypeExpr>, actual: Option<&Type>) -> Pass {
        let by_ref = match (declared, actual) {
            (Some(TypeExpr::Ref { .. }), _) => true,
            (Some(TypeExpr::Wildcard(_)), _) => !et.is_copy(),
            (Some(_), _) => false,
            (None, Some(t)) => matches!(t, Type::Ref(..)),
            (None, None) => !et.is_copy(),
        };
        if by_ref {
            Pass::Ref
        } else if et.is_copy() {
            Pass::Val
        } else {
            Pass::Clone
        }
    }

    /// Checks the function argument of a higher-order method. `front` are the leading
    /// parameter types (the accumulator of `reduce`); the function takes `(front.., value)` or
    /// `(front.., index, value)`. Returns the closure, whether it takes the index, and how the
    /// element is passed.
    fn hof_function(&mut self, fa: &'a ast::Expr, front: &[Type], et: &Type, what: &str) -> Option<(HExpr, bool, Pass)> {
        let base = front.len();
        let shape = |n: usize| -> Option<bool> {
            if n == base + 1 {
                Some(false)
            } else if n == base + 2 {
                Some(true)
            } else {
                None
            }
        };
        let elem_ty = |p: Pass| if p == Pass::Ref { Type::Ref(false, Box::new(et.clone())) } else { et.clone() };
        let shape_err = |s: &mut Self, sp: Span| {
            let acc = if base > 0 { "accumulator, " } else { "" };
            s.err(sp, format!("the function of {} takes ({}value) or ({}index, value)", what, acc, acc));
        };
        if let A::Lambda { params, .. } = &fa.kind {
            let Some(with_index) = shape(params.len()) else {
                shape_err(self, fa.span);
                return None;
            };
            let pass = self.elem_pass(et, Some(&params[params.len() - 1].ty), None);
            let mut ps = front.to_vec();
            if with_index {
                ps.push(Type::int64());
            }
            ps.push(elem_ty(pass));
            let f = self.lambda_with_params(fa, ps);
            return Some((f, with_index, pass));
        }
        let f = self.expr(fa, None);
        let Type::Func(ps, _) = f.ty.deref().clone() else {
            if !f.ty.is_error() {
                let n = self.tname(&f.ty);
                self.err(fa.span, format!("{} needs a function, found {}", what, n));
            }
            return None;
        };
        let Some(with_index) = shape(ps.len()) else {
            shape_err(self, fa.span);
            return None;
        };
        let pass = self.elem_pass(et, None, ps.last());
        Some((f, with_index, pass))
    }

    /// `map`, `filter`, `reduce`, `forEach`, `find`, `findIndex`, `any`, `all`, `count`.
    fn array_hof(&mut self, recv: HExpr, et: Type, name: &str, args: &'a [ast::Arg], span: Span, what: &str) -> Option<HExpr> {
        if let Some(a) = args.iter().find(|a| a.name.is_some()) {
            self.err(a.value.span, format!("{} does not take named arguments", what));
            return None;
        }
        let want = if name == "reduce" { 2 } else { 1 };
        if args.len() != want {
            self.err(span, format!("{} takes {} argument(s) but {} were given", what, want, args.len()));
            return None;
        }
        self.push_scope();
        let mut pre: Vec<HStmt> = Vec::new();
        // the array, borrowed for the loop (like for-each)
        let aty = recv.ty.deref().clone();
        let owned_temp = !matches!(recv.kind, H::Local(_) | H::Field(..) | H::Global(_) | H::Deref(_) | H::Unwrap(_));
        let a_decl = if matches!(recv.kind, H::Local(_)) { Type::Ref(false, Box::new(aty.clone())) } else { aty.clone() };
        let al = self.add_local("$arr", a_decl, span, true, false);
        if owned_temp {
            self.cur().scopes.last_mut().unwrap().owned.push(al);
        }
        pre.push(st(StmtKind::Let(al, Some(recv)), span));
        let a_read = HExpr::new(H::Local(al), aty.clone(), span);
        // accumulator of reduce: its type comes from the initial value (a numeric literal takes
        // the element type)
        let mut front = Vec::new();
        let mut acc = None;
        if name == "reduce" {
            let init_e = &args[0].value;
            let is_int = matches!(init_e.kind, A::Int(_)) || matches!(&init_e.kind, A::Unary(_, i) if matches!(i.kind, A::Int(_)));
            let is_float = matches!(init_e.kind, A::Float(_)) || matches!(&init_e.kind, A::Unary(_, i) if matches!(i.kind, A::Float(_)));
            let x = if is_int || is_float {
                // a numeric literal takes the element type, or the widest common type
                let t = match (&et, is_int) {
                    (Type::Int(_), true) | (Type::Float(_), false) => et.clone(),
                    (Type::Float(_), true) => et.clone(),
                    (_, true) => Type::int64(),
                    _ => Type::Float(FloatTy::F64),
                };
                let x = self.expr(init_e, Some(&t));
                self.coerce(x, &t, init_e.span)
            } else {
                let x = self.expr(init_e, None);
                self.consume(x)
            };
            let ut = x.ty.clone();
            let accl = self.add_local("$acc", ut.clone(), span, false, false);
            pre.push(st(StmtKind::Let(accl, Some(x)), span));
            front.push(ut.clone());
            acc = Some((accl, ut));
        }
        let fa = &args[args.len() - 1].value;
        let Some((f, with_index, pass)) = self.hof_function(fa, &front, &et, what) else {
            self.pop_scope();
            return None;
        };
        let fty = f.ty.clone();
        let Type::Func(ptys, fret) = fty.clone() else {
            self.pop_scope();
            return None;
        };
        let fret = (*fret).clone();
        let fl = self.add_local("$fn", fty.clone(), span, true, false);
        self.cur().scopes.last_mut().unwrap().owned.push(fl);
        pre.push(st(StmtKind::Let(fl, Some(f)), span));
        // result
        let (res_ty, init): (Type, Option<HExpr>) = match name {
            "map" => {
                if fret == Type::Void {
                    self.err(fa.span, "the function of map must return a value");
                }
                let t = Type::Array(Box::new(fret.clone()));
                (t.clone(), Some(HExpr::new(H::ArrayLit(vec![]), t, span)))
            }
            "filter" => (aty.clone(), Some(HExpr::new(H::ArrayLit(vec![]), aty.clone(), span))),
            "find" => {
                let t = if et.is_nullable() { et.clone() } else { Type::Nullable(Box::new(et.clone())) };
                (t.clone(), Some(HExpr::new(H::Lit(Lit::Null), t, span)))
            }
            "findIndex" => (Type::int64(), Some(self.lit_i64(-1, span))),
            "count" => (Type::int64(), Some(self.lit_i64(0, span))),
            "any" => (Type::Bool, Some(HExpr::new(H::Lit(Lit::Bool(false)), Type::Bool, span))),
            "all" => (Type::Bool, Some(HExpr::new(H::Lit(Lit::Bool(true)), Type::Bool, span))),
            "reduce" => (acc.as_ref().unwrap().1.clone(), None),
            "forEach" => (Type::Void, None),
            _ => unreachable!(),
        };
        if matches!(name, "filter" | "find" | "findIndex" | "count" | "any" | "all") && fret != Type::Bool && !fret.is_error() {
            let n = self.tname(&fret);
            self.err(fa.span, format!("the function of {} must return Boolean, found {}", what, n));
        }
        if name == "reduce" {
            let ut = &acc.as_ref().unwrap().1;
            if !self.assignable(&fret, ut) && !fret.is_error() {
                let (x, y) = (self.tname(&fret), self.tname(ut));
                self.err(fa.span, format!("the function of {} must return the accumulator type {}, found {}", what, y, x));
            }
        }
        // the result lives outside the loop scope
        let rl = init.as_ref().map(|_| self.add_local("$res", res_ty.clone(), span, false, false));
        let il = self.add_local("$i", Type::int64(), span, false, false);
        pre.push(st(StmtKind::Let(il, Some(self.lit_i64(0, span))), span));
        let i_read = HExpr::new(H::Local(il), Type::int64(), span);
        let nl = self.add_local("$n", Type::int64(), span, true, false);
        pre.push(st(StmtKind::Let(nl, Some(HExpr::new(H::Builtin(Builtin::ArrLength, vec![a_read.clone()]), Type::int64(), span))), span));
        let cond = HExpr::new(H::Cmp(CmpOp::Lt, Box::new(i_read.clone()), Box::new(HExpr::new(H::Local(nl), Type::int64(), span))), Type::Bool, span);
        // loop body
        self.push_scope();
        let mut body = Vec::new();
        let x_decl = if et.is_copy() { et.clone() } else { Type::Ref(false, Box::new(et.clone())) };
        let xl = self.add_local("$x", x_decl.clone(), span, false, false);
        body.push(st(StmtKind::Let(xl, Some(HExpr::new(H::Builtin(Builtin::ArrAt, vec![a_read.clone(), i_read.clone()]), et.clone(), span))), span));
        let x_read = HExpr::new(H::Local(xl), x_decl, span);
        let owned_x = |s: &mut Self| -> HExpr {
            let _ = s;
            if et.is_copy() {
                HExpr::new(H::Local(xl), et.clone(), span)
            } else {
                HExpr::new(H::Builtin(Builtin::Clone, vec![HExpr::new(H::Local(xl), et.clone(), span)]), et.clone(), span)
            }
        };
        let mut cargs = Vec::new();
        if let Some((accl, ut)) = &acc {
            let r = HExpr::new(H::Local(*accl), ut.clone(), span);
            cargs.push(self.pass_arg(r, &ptys[0], span));
        }
        if with_index {
            let p = &ptys[cargs.len()];
            cargs.push(self.coerce(i_read.clone(), p, span));
        }
        let ep = ptys[cargs.len()].clone();
        let ex = match pass {
            Pass::Ref => x_read.clone(),
            Pass::Val => HExpr::new(H::Local(xl), et.clone(), span),
            Pass::Clone => owned_x(self),
        };
        cargs.push(self.pass_arg(ex, &ep, span));
        let call = HExpr::new(H::CallClosure(Box::new(HExpr::new(H::Local(fl), fty.clone(), span)), cargs), fret.clone(), span);
        let res_place = rl.map(Place::Local);
        let set_res = |v: HExpr| st(StmtKind::Assign(res_place.clone().unwrap(), v), span);
        match name {
            "map" => body.push(st(StmtKind::Expr(HExpr::new(H::BuiltinMut(Builtin::ArrPush, Box::new(res_place.clone().unwrap()), vec![call]), Type::Void, span)), span)),
            "filter" => {
                let push = HExpr::new(H::BuiltinMut(Builtin::ArrPush, Box::new(res_place.clone().unwrap()), vec![owned_x(self)]), Type::Void, span);
                body.push(st(StmtKind::If(call, vec![st(StmtKind::Expr(push), span)], vec![]), span));
            }
            "find" => {
                let v = owned_x(self);
                let v = self.coerce(v, &res_ty, span);
                body.push(st(StmtKind::If(call, vec![set_res(v), st(StmtKind::Break, span)], vec![]), span));
            }
            "findIndex" => body.push(st(StmtKind::If(call, vec![set_res(i_read.clone()), st(StmtKind::Break, span)], vec![]), span)),
            "any" => body.push(st(StmtKind::If(call, vec![set_res(HExpr::new(H::Lit(Lit::Bool(true)), Type::Bool, span)), st(StmtKind::Break, span)], vec![]), span)),
            "all" => body.push(st(StmtKind::If(call, vec![], vec![set_res(HExpr::new(H::Lit(Lit::Bool(false)), Type::Bool, span)), st(StmtKind::Break, span)]), span)),
            "count" => {
                let r = HExpr::new(H::Local(rl.unwrap()), Type::int64(), span);
                let inc = HExpr::new(H::Arith(ArithOp::Add, Box::new(r), Box::new(self.lit_i64(1, span)), true), Type::int64(), span);
                body.push(st(StmtKind::If(call, vec![set_res(inc)], vec![]), span));
            }
            "reduce" => {
                let (accl, ut) = acc.clone().unwrap();
                let v = self.coerce(call, &ut, span);
                body.push(st(StmtKind::Assign(Place::Local(accl), v), span));
            }
            _ => body.push(st(StmtKind::Expr(call), span)),
        }
        let owned_inner = self.pop_scope();
        let step = vec![st(StmtKind::Assign(Place::Local(il), HExpr::new(H::Arith(ArithOp::Add, Box::new(i_read.clone()), Box::new(self.lit_i64(1, span)), true), Type::int64(), span)), span)];
        pre.push(st(StmtKind::Loop { cond: Some(cond), body: vec![st(StmtKind::Block { body, drops: owned_inner }, span)], step }, span));
        let owned = self.pop_scope();
        let mut outer = Vec::new();
        if let (Some(r), Some(init)) = (rl, init) {
            outer.push(st(StmtKind::Let(r, Some(init)), span));
        }
        // reduce: the accumulator is declared inside the loop scope; move it out at the end
        let result = match (name, rl, &acc) {
            ("reduce", _, Some((accl, ut))) => {
                let r2 = self.add_local("$res", ut.clone(), span, false, false);
                outer.push(st(StmtKind::Let(r2, None), span));
                let mv = if ut.is_copy() { HExpr::new(H::Local(*accl), ut.clone(), span) } else { HExpr::new(H::Move(*accl), ut.clone(), span) };
                pre.push(st(StmtKind::Assign(Place::Local(r2), mv), span));
                Some((r2, ut.clone()))
            }
            (_, Some(r), _) => Some((r, res_ty.clone())),
            _ => None,
        };
        outer.push(st(StmtKind::Block { body: pre, drops: owned }, span));
        let tail = match result {
            Some((r, t)) => {
                if t.is_copy() {
                    HExpr::new(H::Local(r), t.clone(), span)
                } else {
                    HExpr::new(H::Move(r), t.clone(), span)
                }
            }
            None => HExpr::new(H::Lit(Lit::Void), Type::Void, span),
        };
        let ty = tail.ty.clone();
        Some(HExpr::new(H::Seq(outer, Box::new(tail)), ty, span))
    }

    /// `arr.sortBy((a, b) -> ...)`: a stable sort by a comparator returning a negative number,
    /// zero or a positive number (merge sort in the prelude).
    fn sort_by(&mut self, obj: &'a ast::Expr, et: Type, args: &'a [ast::Arg], span: Span, what: &str) -> Option<HExpr> {
        if args.len() != 1 || args[0].name.is_some() {
            self.err(span, format!("{} takes one comparator function", what));
            return None;
        }
        let fa = &args[0].value;
        let (pass, f) = if let A::Lambda { params, .. } = &fa.kind {
            if params.len() != 2 {
                self.err(fa.span, format!("the comparator of {} takes two parameters", what));
                return None;
            }
            let pass = self.elem_pass(&et, Some(&params[0].ty), None);
            let pt = if pass == Pass::Ref { Type::Ref(false, Box::new(et.clone())) } else { et.clone() };
            (pass, self.lambda_with_params(fa, vec![pt.clone(), pt]))
        } else {
            let f = self.expr(fa, None);
            let pass = match f.ty.deref() {
                Type::Func(ps, _) if ps.len() == 2 => self.elem_pass(&et, None, ps.first()),
                Type::Error => return None,
                _ => {
                    self.err(fa.span, format!("{} needs a comparator function", what));
                    return None;
                }
            };
            (pass, f)
        };
        let Type::Func(_, r) = f.ty.deref().clone() else { return None };
        if !r.is_numeric() || !matches!(*r, Type::Int(_)) {
            if !r.is_error() {
                let n = self.tname(&r);
                self.err(fa.span, format!("the comparator of {} must return an integer, found {}", what, n));
            }
            return None;
        }
        let helper = if pass == Pass::Ref { "__arraySortByRef" } else { "__arraySortByValue" };
        let place = self.mut_place_pub(obj, "sortBy", span)?;
        let fid = self.prelude_generic(helper, vec![et.clone(), (*r).clone()], span)?;
        let sig = self.sigs[fid as usize].clone();
        let f = self.coerce(f, &sig.params[1], span);
        let arr_ref = match place {
            // re-borrow of a `*T[]` parameter
            Place::Deref(id) => HExpr::new(H::Local(id), sig.params[0].clone(), span),
            place => {
                self.mark_place_root_cell_pub(&place);
                HExpr::new(H::RefMut(Box::new(place)), sig.params[0].clone(), span)
            }
        };
        Some(HExpr::new(H::Call(fid, vec![arr_ref, f]), Type::Void, span))
    }

    /// Instantiates a generic helper function of the prelude.
    pub fn prelude_generic(&mut self, name: &str, targs: Vec<Type>, span: Span) -> Option<FuncId> {
        let pm = self.prelude_module;
        let fs = self.lookup_funcs(pm, name)?;
        let fr = fs.into_iter().find(|f| f.func.is_none())?;
        self.instantiate_generic(&fr, targs, span)
    }

    /// The (non-generic) prelude function `name`.
    pub fn prelude_func(&mut self, name: &str) -> Option<FuncId> {
        let pm = self.prelude_module;
        let fs = self.lookup_funcs(pm, name)?;
        fs.into_iter().find_map(|f| f.func)
    }
}
