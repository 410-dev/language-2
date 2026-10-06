//! Statement checking, function bodies and desugaring.

use super::*;
use crate::ast::{BinOp, ExprKind as A, StmtKind as S};
use crate::hir::{Expr as HExpr, ExprKind as H, Stmt as HStmt};
use l2_runtime::ops::{ArithOp, CmpOp};
use l2_runtime::Builtin;

fn st(kind: StmtKind, span: Span) -> HStmt {
    HStmt { kind, span }
}

impl<'a> Checker<'a> {
    pub fn check_job(&mut self, job: Job<'a>) {
        let sig = self.sigs[job.func as usize].clone();
        let kind = self.funcs[job.func as usize].kind;
        let wrap = self.modules[job.module].wrap;
        let mut ctx = FnCtx::new(kind, job.module, wrap, job.subst.clone());
        ctx.class = job.class;
        ctx.iface = job.iface;
        ctx.is_static = job.is_static;
        ctx.ret = Some(sig.ret.clone());
        ctx.declared_throws = sig.throws.clone();
        self.fstack.push(ctx);
        let span = self.funcs[job.func as usize].span;
        let mut params = Vec::new();
        if !job.is_static {
            let this_ty = match (job.class, job.iface) {
                (Some(c), _) => Some(Type::Class(c)),
                (None, Some(i)) => Some(Type::Iface(i)),
                _ => None,
            };
            if let Some(t) = this_ty {
                let id = self.declare("this", t, span, true, false, false);
                self.cur().this_local = Some(id);
                params.push(id);
            }
        }
        let decl_params: Option<&'a [ast::Param]> = match &job.kind {
            JobKind::Func(d) => Some(&d.params),
            JobKind::Ctor(Some(cd)) => Some(&cd.params),
            _ => None,
        };
        for (i, t) in sig.params.iter().enumerate() {
            let (imm, copied, pspan) = match decl_params.and_then(|ps| ps.get(i)) {
                Some(p) => (p.mods.immutable, p.mods.copied, p.span),
                None => (false, false, span),
            };
            let name = sig.param_names[i].clone();
            let id = self.declare(&name, t.clone(), pspan, imm, copied, true);
            params.push(id);
        }
        let body: Vec<HStmt> = match job.kind {
            JobKind::Func(d) => match &d.body {
                Some(b) => self.block_stmts(b),
                None => Vec::new(),
            },
            JobKind::Ctor(cd) => self.ctor_body(job.class.unwrap(), cd, span),
            JobKind::Getter(idx) => {
                let this = self.this_expr(span).unwrap();
                let c = job.class.unwrap();
                let fty = self.classes[c as usize].fields[idx as usize].ty.clone();
                let read = HExpr::new(H::Field(Box::new(this), idx), fty, span);
                vec![st(StmtKind::Return(Some(read)), span)]
            }
            JobKind::Setter(idx, chain) => {
                let this = self.this_expr(span).unwrap();
                let v = params[1];
                let vty = self.local_ty(v);
                let val = self.consume(HExpr::new(H::Local(v), vty, span));
                let mut out = vec![st(StmtKind::Assign(Place::Field(Box::new(this.clone()), idx), val), span)];
                if chain {
                    let rt = sig.ret.clone();
                    out.push(st(StmtKind::Return(Some(HExpr { kind: this.kind, ty: rt, span })), span));
                }
                out
            }
            JobKind::Delegate(df) => {
                let mut args = Vec::new();
                for &p in &params {
                    let t = self.local_ty(p);
                    let e = HExpr::new(H::Local(p), t, span);
                    let e = if p == params[0] { e } else { self.consume(e) };
                    args.push(e);
                }
                let call = HExpr::new(H::Call(df, args), sig.ret.clone(), span);
                if sig.ret == Type::Void {
                    vec![st(StmtKind::Expr(call), span)]
                } else {
                    vec![st(StmtKind::Return(Some(call)), span)]
                }
            }
        };
        let owned = self.pop_scope();
        let ctx = self.fstack.pop().unwrap();
        let f = &mut self.funcs[job.func as usize];
        f.params = params;
        f.locals = ctx.locals;
        f.body = vec![st(StmtKind::Block { body, drops: owned }, span)];
    }

    fn ctor_body(&mut self, c: ClassId, cd: Option<&'a ast::CtorDecl>, span: Span) -> Vec<HStmt> {
        let mut out = Vec::new();
        let stmts: &'a [ast::Stmt] = match cd {
            Some(cd) => &cd.body.stmts,
            None => &[],
        };
        let mut rest = stmts;
        let this = self.this_expr(span).unwrap();
        let parent = self.classes[c as usize].parent;
        let mut explicit_super = None;
        if let Some(first) = stmts.first() {
            if let S::Expr(e) = &first.kind {
                if let A::Call(callee, args) = &e.kind {
                    if matches!(callee.kind, A::Super(None)) {
                        explicit_super = Some((args, first.span));
                        rest = &stmts[1..];
                    }
                }
            }
        }
        if let Some(p) = parent {
            let ctors = self.cmeta[p as usize].ctors.clone();
            let cands: Vec<expr::Cand> = ctors.iter().map(|(_, ps, ns, _, _)| expr::Cand { params: ps.clone(), names: ns.clone() }).collect();
            let pname = self.classes[p as usize].name.clone();
            let (args, sspan): (&'a [ast::Arg], Span) = match explicit_super {
                Some((a, s)) => (a.as_slice(), s),
                None => (&[], span),
            };
            if explicit_super.is_none() && !ctors.iter().any(|(_, ps, _, _, _)| ps.is_empty()) {
                self.err(span, format!("superclass '{}' has no no-argument constructor; call super(...) explicitly as the first statement", pname));
            } else if let Some((i, a)) = self.select_overload(&cands, args, sspan, &format!("constructor of '{}'", pname)) {
                let (fid, _, _, access, throws) = ctors[i].clone();
                if access == Access::Private {
                    self.err(sspan, format!("constructor of '{}' is private", pname));
                }
                self.note_throws(&throws, sspan);
                let mut all = vec![this.clone()];
                all.extend(a);
                out.push(st(StmtKind::Expr(HExpr::new(H::Call(fid, all), Type::Void, sspan)), sspan));
            }
        } else if let Some((_, s)) = explicit_super {
            self.err(s, "super(...) called in a class without a superclass");
        }
        // field initialisers
        let decl = self.cmeta[c as usize].decl;
        if let Some(decl) = decl {
            for f in &decl.fields {
                if f.mods.is_static {
                    continue;
                }
                if let Some(init) = &f.init {
                    let idx = self.field_index(c, &f.name).unwrap();
                    let ty = self.classes[c as usize].fields[idx as usize].ty.clone();
                    let x = self.expr(init, Some(&ty));
                    let x = self.coerce(x, &ty, init.span);
                    let x = self.consume(x);
                    out.push(st(StmtKind::Assign(Place::Field(Box::new(this.clone()), idx), x), f.span));
                }
            }
        }
        for s in rest {
            let v = self.stmt(s);
            out.extend(v);
        }
        out
    }

    /// Checks statements in the current scope.
    pub fn block_stmts(&mut self, b: &'a ast::Block) -> Vec<HStmt> {
        self.cur().narrow.push(HashMap::new());
        let mut out = Vec::new();
        for s in &b.stmts {
            let v = self.stmt(s);
            out.extend(v);
        }
        self.cur().narrow.pop();
        out
    }

    /// Checks a block in a new scope.
    pub fn block(&mut self, b: &'a ast::Block) -> Vec<HStmt> {
        self.push_scope();
        let body = self.block_stmts(b);
        let owned = self.pop_scope();
        vec![st(StmtKind::Block { body, drops: owned }, b.span)]
    }

    fn invalidate_narrowing(&mut self, id: LocalId) {
        for m in self.cur().narrow.iter_mut() {
            m.remove(&id);
        }
    }

    pub fn stmt(&mut self, s: &'a ast::Stmt) -> Vec<HStmt> {
        let span = s.span;
        match &s.kind {
            S::VarDecl { mods, ty, name, init } => self.var_decl(mods, ty, name, init.as_ref(), span),
            S::MultiAssign { targets, value } => self.multi_assign(targets, value, span),
            S::Assign { target, op, value } => self.assign(target, *op, value, span),
            S::Expr(e) => {
                let x = self.expr(e, None);
                vec![st(StmtKind::Expr(x), span)]
            }
            S::If { cond, then, els } => {
                let c = self.expr(cond, Some(&Type::Bool));
                let c = self.cond(c);
                let (tn, en) = self.null_checks(cond);
                let tn: Vec<(LocalId, Type)> = tn.into_iter().filter(|(id, _)| !assigns_local(&then.stmts, &self.cur_ref().locals[*id as usize].name)).collect();
                self.cur().narrow.push(tn.into_iter().collect());
                let t = self.block(then);
                self.cur().narrow.pop();
                let else_assigns = |s: &Option<Box<ast::Stmt>>, n: &str| match s {
                    Some(x) => assigns_local(std::slice::from_ref(&**x), n),
                    None => false,
                };
                let en: Vec<(LocalId, Type)> = en.into_iter().filter(|(id, _)| !else_assigns(els, &self.cur_ref().locals[*id as usize].name)).collect();
                self.cur().narrow.push(en.iter().cloned().collect());
                let e = match els {
                    Some(x) => self.stmt(x),
                    None => Vec::new(),
                };
                self.cur().narrow.pop();
                // `if (x == null) { return }` narrows x for the rest of the block
                if els.is_none() && always_exits(&then.stmts) {
                    if let Some(top) = self.cur().narrow.last_mut() {
                        for (id, t) in en {
                            top.insert(id, t);
                        }
                    }
                }
                vec![st(StmtKind::If(c, t, e), span)]
            }
            S::ForC { init, cond, step, body } => {
                self.push_scope();
                let mut pre = Vec::new();
                if let Some(i) = init {
                    pre.extend(self.stmt(i));
                }
                let c = cond.as_ref().map(|c| {
                    let x = self.expr(c, Some(&Type::Bool));
                    self.cond(x)
                });
                self.cur().loops += 1;
                let b = self.block(body);
                let stp = match step {
                    Some(s) => self.stmt(s),
                    None => Vec::new(),
                };
                self.cur().loops -= 1;
                let owned = self.pop_scope();
                pre.push(st(StmtKind::Loop { cond: c, body: b, step: stp }, span));
                vec![st(StmtKind::Block { body: pre, drops: owned }, span)]
            }
            S::ForEach { vars, iter, body } => self.for_each(vars, iter, body, span),
            S::Switch { value, alias, cases } => {
                self.push_scope();
                let mut pre = Vec::new();
                let v = self.expr(value, None);
                let vty = v.ty.clone();
                match alias {
                    Some(a) => {
                        let decl_ty = if vty.is_copy() { vty.clone() } else { Type::Ref(false, Box::new(vty.clone())) };
                        let id = self.declare(a, decl_ty, span, true, false, false);
                        pre.push(st(StmtKind::Let(id, Some(v)), span));
                    }
                    None => {
                        let t = self.temp(vty, span);
                        pre.push(st(StmtKind::Let(t, Some(v)), span));
                    }
                }
                self.cur().switches += 1;
                let mut hc = Vec::new();
                let mut defaults = 0;
                for c in cases {
                    let cond = match &c.cond {
                        Some(e) => {
                            let x = self.expr(e, Some(&Type::Bool));
                            Some(self.cond(x))
                        }
                        None => {
                            defaults += 1;
                            if defaults > 1 {
                                self.err(c.span, "switch has more than one default");
                            }
                            None
                        }
                    };
                    self.push_scope();
                    self.cur().narrow.push(HashMap::new());
                    let mut body = Vec::new();
                    for s in &c.body {
                        body.extend(self.stmt(s));
                    }
                    self.cur().narrow.pop();
                    let owned = self.pop_scope();
                    hc.push(Case { cond, body: vec![st(StmtKind::Block { body, drops: owned }, c.span)], fallthrough: c.fallthrough });
                }
                self.cur().switches -= 1;
                let owned = self.pop_scope();
                pre.push(st(StmtKind::Switch { cases: hc }, span));
                vec![st(StmtKind::Block { body: pre, drops: owned }, span)]
            }
            S::Break => {
                if self.cur_ref().loops + self.cur_ref().switches == 0 {
                    self.err(span, "'break' outside of a loop or switch");
                }
                vec![st(StmtKind::Break, span)]
            }
            S::Continue => {
                if self.cur_ref().loops == 0 {
                    self.err(span, "'continue' outside of a loop");
                }
                vec![st(StmtKind::Continue, span)]
            }
            S::Fallthrough => {
                self.err(span, "'fallthrough' can only be the last statement of a switch case");
                Vec::new()
            }
            S::Return(e) => self.ret(e.as_ref(), span),
            S::Throw(e) => {
                let x = self.expr(e, None);
                match x.ty.deref().clone() {
                    Type::Class(c) if self.classes[c as usize].is_throwable => {
                        self.note_throws(&[c], span);
                    }
                    Type::Error => {}
                    other => {
                        let n = self.tname(&other);
                        self.err(span, format!("can only throw Throwable objects, found {}", n));
                    }
                }
                let x = self.consume(x);
                vec![st(StmtKind::Throw(x), span)]
            }
            S::Try { body, catches, finally } => {
                let mut groups = Vec::new();
                let mut all = Vec::new();
                let module = self.cur_ref().module;
                for c in catches {
                    let mut cls = Vec::new();
                    for t in &c.types {
                        match self.resolve_type(&TypeExpr::named(t, c.span), module, &Subst::new()) {
                            Type::Class(k) if self.classes[k as usize].is_throwable => cls.push(k),
                            Type::Error => {}
                            _ => self.err(c.span, format!("'{}' is not an exception class", t)),
                        }
                    }
                    all.extend(cls.iter().copied());
                    groups.push(cls);
                }
                self.cur().catch_stack.push(all);
                let b = self.block(body);
                self.cur().catch_stack.pop();
                let mut hc = Vec::new();
                for (c, cls) in catches.iter().zip(groups) {
                    // the variable type is the most derived common superclass
                    let mut common = cls.first().copied();
                    for &k in cls.iter().skip(1) {
                        let mut cur = common;
                        while let Some(cc) = cur {
                            if self.is_subclass(k, cc) {
                                break;
                            }
                            cur = self.classes[cc as usize].parent;
                        }
                        common = cur;
                    }
                    let ty = common.map(Type::Class).unwrap_or(Type::Error);
                    self.push_scope();
                    let local = self.declare(&c.name, ty, c.span, false, false, true);
                    let body = self.block_stmts(&c.body);
                    let owned = self.pop_scope();
                    hc.push(Catch { classes: cls, local, body: vec![st(StmtKind::Block { body, drops: owned }, c.span)] });
                }
                let f = finally.as_ref().map(|fb| self.block(fb));
                vec![st(StmtKind::Try { body: b, catches: hc, finally: f }, span)]
            }
            S::Block(b) => self.block(b),
            S::Using { module, alias } => {
                if let Some(imp) = self.resolve_import_pub(module, span) {
                    let m = self.cur_ref().module;
                    self.modules[m].imports.insert(alias.clone(), imp);
                }
                Vec::new()
            }
        }
    }

    pub fn resolve_import_pub(&mut self, module: &str, span: Span) -> Option<Import> {
        if module == "stdio" {
            return Some(Import::Stdio);
        }
        for (i, m) in self.modules.iter().enumerate() {
            if m.name == module && !m.is_prelude {
                return Some(Import::Module(i));
            }
        }
        self.err(span, format!("unknown module '{}'", module));
        None
    }

    /// Locals proven non-null in the then-branch and in the else-branch of a condition.
    fn null_checks(&mut self, cond: &'a ast::Expr) -> (Vec<(LocalId, Type)>, Vec<(LocalId, Type)>) {
        let local_nullable = |s: &mut Self, e: &ast::Expr| -> Option<(LocalId, Type)> {
            if let A::Ident(n) = &e.kind {
                let id = s.lookup_local(n)?;
                if let Type::Nullable(t) = s.local_ty(id) {
                    return Some((id, *t));
                }
            }
            None
        };
        match &cond.kind {
            A::Binary(op @ (BinOp::Ne | BinOp::Eq), a, b) => {
                let target = if matches!(b.kind, A::Null) {
                    local_nullable(self, a)
                } else if matches!(a.kind, A::Null) {
                    local_nullable(self, b)
                } else {
                    None
                };
                match target {
                    Some(t) if *op == BinOp::Ne => (vec![t], vec![]),
                    Some(t) => (vec![], vec![t]),
                    None => (vec![], vec![]),
                }
            }
            A::Binary(BinOp::And, a, b) => {
                let (mut t1, _) = self.null_checks(a);
                let (t2, _) = self.null_checks(b);
                t1.extend(t2);
                (t1, vec![])
            }
            A::Binary(BinOp::Or, a, b) => {
                let (_, mut e1) = self.null_checks(a);
                let (_, e2) = self.null_checks(b);
                e1.extend(e2);
                (vec![], e1)
            }
            _ => (vec![], vec![]),
        }
    }

    fn var_decl(&mut self, mods: &ast::DeclMods, ty: &'a TypeExpr, name: &str, init: Option<&'a ast::Expr>, span: Span) -> Vec<HStmt> {
        let is_st = matches!(ty, TypeExpr::Named { name, args, .. } if name == "STVariable" && args.is_empty());
        let (dty, val) = if is_st {
            let Some(init) = init else {
                self.err(span, "an STVariable must be initialized");
                return Vec::new();
            };
            let x = self.expr(init, None);
            let t = match &x.ty {
                Type::Null => {
                    self.err(span, "cannot infer a type from null; declare the type explicitly");
                    Type::Error
                }
                Type::Void => {
                    self.err(span, "cannot declare a variable of type void");
                    Type::Error
                }
                Type::Ref(_, t) => (**t).clone(),
                t => t.clone(),
            };
            let x = self.consume(x);
            (t, Some(x))
        } else {
            let module = self.cur_ref().module;
            let subst = self.cur_ref().subst.clone();
            let t = self.resolve_type(ty, module, &subst);
            if t == Type::Void {
                self.err(span, "cannot declare a variable of type void");
            }
            if self.manual() && matches!(t, Type::Ref(..)) {
                self.err(span, "references cannot be used with MemoryManagement=manual");
            }
            let v = init.map(|e| {
                let x = if matches!(t, Type::Ref(true, _)) {
                    match &e.kind {
                        A::Unary(ast::UnOp::RefMut, _) => self.expr(e, Some(&t)),
                        A::Ident(_) => {
                            let x = self.expr(e, Some(&t));
                            // `*T r = otherRef` copies the reference
                            if let H::Deref(inner) = x.kind {
                                *inner
                            } else {
                                x
                            }
                        }
                        _ => self.expr(e, Some(&t)),
                    }
                } else {
                    self.expr(e, Some(&t))
                };
                let x = self.coerce(x, &t, e.span);
                if matches!(t, Type::Ref(..)) {
                    x
                } else {
                    self.consume(x)
                }
            });
            (t, v)
        };
        let id = self.declare(name, dty, span, mods.immutable, mods.copied, true);
        vec![st(StmtKind::Let(id, val), span)]
    }

    fn multi_assign(&mut self, targets: &'a [Option<ast::Expr>], value: &'a ast::Expr, span: Span) -> Vec<HStmt> {
        let v = self.expr(value, None);
        let ts = match &v.ty {
            Type::Tuple(ts) => ts.clone(),
            Type::Error => return Vec::new(),
            other => {
                let n = self.tname(other);
                self.err(span, format!("multiple assignment needs a function returning multiple values, found {}", n));
                return Vec::new();
            }
        };
        if ts.len() != targets.len() {
            self.err(span, format!("expected {} target(s) for {} value(s)", ts.len(), targets.len()));
            return Vec::new();
        }
        let tmp = self.temp(v.ty.clone(), span);
        let tty = v.ty.clone();
        let mut out = vec![st(StmtKind::Let(tmp, Some(v)), span)];
        for (i, (t, ety)) in targets.iter().zip(ts.iter()).enumerate() {
            let Some(t) = t else { continue };
            let get = HExpr::new(H::TupleGet(Box::new(HExpr::new(H::Local(tmp), tty.clone(), span)), i as u32), ety.clone(), t.span);
            if let A::Ident(n) = &t.kind {
                if self.lookup_local(n).is_none() && self.place_of(t).is_none() {
                    if *ety == Type::Null {
                        self.err(t.span, format!("cannot infer the type of '{}' from null", n));
                    }
                    let id = self.declare(n, ety.clone(), t.span, false, false, true);
                    out.push(st(StmtKind::Let(id, Some(get)), t.span));
                    continue;
                }
            }
            out.extend(self.assign_value(t, get, t.span));
        }
        out
    }

    fn assign_value(&mut self, target: &'a ast::Expr, value: HExpr, span: Span) -> Vec<HStmt> {
        match self.place_of(target) {
            Some((place, ty, mutable, why)) => {
                if !mutable {
                    self.err(span, format!("cannot assign: {}", why));
                }
                if let Place::Local(id) | Place::Deref(id) = &place {
                    let id = *id;
                    self.invalidate_narrowing(id);
                }
                let v = self.coerce(value, &ty, span);
                let v = self.consume(v);
                vec![st(StmtKind::Assign(place, v), span)]
            }
            None => {
                self.err(span, "invalid assignment target");
                Vec::new()
            }
        }
    }

    fn assign(&mut self, target: &'a ast::Expr, op: Option<BinOp>, value: &'a ast::Expr, span: Span) -> Vec<HStmt> {
        // indexed assignment: arr[i] = v  ->  arr.set(i, v)
        if let A::Index(base, idx) = &target.kind {
            if idx.len() != 1 {
                self.err(span, "expected exactly one index");
                return Vec::new();
            }
            let Some((place, bty, mutable, why)) = self.place_of(base) else {
                self.err(span, "indexed assignment needs a variable or field");
                return Vec::new();
            };
            if !mutable {
                self.err(span, format!("cannot modify: {}", why));
            }
            let mut pre = Vec::new();
            return match bty.deref().clone() {
                Type::Array(et) => {
                    let i = self.index_arg(&idx[0]);
                    let ti = self.temp(Type::int64(), span);
                    pre.push(st(StmtKind::Let(ti, Some(i)), span));
                    let iread = HExpr::new(H::Local(ti), Type::int64(), span);
                    let val = match op {
                        Some(op) => {
                            let cur_arr = self.place_read(&place, &bty, span);
                            let cur = HExpr::new(H::Builtin(Builtin::ArrAt, vec![cur_arr, iread.clone()]), (*et).clone(), span);
                            let rhs = self.expr(value, Some(&et));
                            self.binary_typed(op, cur, rhs, span)
                        }
                        None => self.expr(value, Some(&et)),
                    };
                    let val = self.coerce(val, &et, value.span);
                    let val = self.consume(val);
                    pre.push(st(StmtKind::Expr(HExpr::new(H::BuiltinMut(Builtin::ArrSet, Box::new(place), vec![iread, val]), Type::Void, span)), span));
                    pre
                }
                Type::Dict(kt, vt) => {
                    let k = self.expr(&idx[0], Some(&kt));
                    let k = self.coerce(k, &kt, idx[0].span);
                    let k = self.consume(k);
                    let tk = self.temp((*kt).clone(), span);
                    pre.push(st(StmtKind::Let(tk, Some(k)), span));
                    let kread = HExpr::new(H::Local(tk), (*kt).clone(), span);
                    let val = match op {
                        Some(op) => {
                            let cur_d = self.place_read(&place, &bty, span);
                            let cur = HExpr::new(H::Builtin(Builtin::DictGet, vec![cur_d, kread.clone()]), (*vt).clone(), span);
                            let rhs = self.expr(value, Some(&vt));
                            self.binary_typed(op, cur, rhs, span)
                        }
                        None => self.expr(value, Some(&vt)),
                    };
                    let val = self.coerce(val, &vt, value.span);
                    let val = self.consume(val);
                    let kmove = if kt.is_copy() { kread } else { HExpr::new(H::Move(tk), (*kt).clone(), span) };
                    pre.push(st(StmtKind::Expr(HExpr::new(H::BuiltinMut(Builtin::DictSet, Box::new(place), vec![kmove, val]), Type::Void, span)), span));
                    pre
                }
                Type::Error => Vec::new(),
                other => {
                    let n = self.tname(&other);
                    self.err(span, format!("type {} does not support indexed assignment", n));
                    Vec::new()
                }
            };
        }
        let Some((place, ty, mutable, why)) = self.place_of(target) else {
            self.err(span, "invalid assignment target");
            return Vec::new();
        };
        if !mutable {
            self.err(span, format!("cannot assign: {}", why));
        }
        if let Place::Local(id) | Place::Deref(id) = &place {
            let id = *id;
            self.invalidate_narrowing(id);
        }
        let v = match op {
            Some(op) => {
                let cur = self.place_read(&place, &ty, span);
                let rhs = if expr::is_literalish(value) { self.expr(value, Some(&ty)) } else { self.expr(value, None) };
                let combined = self.binary_typed(op, cur, rhs, span);
                if combined.ty != ty && combined.ty.is_numeric() && ty.is_numeric() && !self.assignable(&combined.ty, &ty) {
                    let (a, b) = (self.tname(&combined.ty), self.tname(&ty));
                    self.err(span, format!("compound assignment produces {} which does not fit in {}; use .castTo()", a, b));
                }
                self.coerce(combined, &ty, span)
            }
            None => {
                let x = self.expr(value, Some(&ty));
                self.coerce(x, &ty, value.span)
            }
        };
        let v = self.consume(v);
        vec![st(StmtKind::Assign(place, v), span)]
    }

    fn ret(&mut self, e: Option<&'a ast::Expr>, span: Span) -> Vec<HStmt> {
        let ret = self.cur_ref().ret.clone();
        match e {
            Some(e) => {
                let x = self.expr(e, ret.as_ref().filter(|t| **t != Type::Void));
                let rt = match ret {
                    Some(t) => t,
                    None => {
                        let t = match &x.ty {
                            Type::Null => Type::Dyn,
                            t => t.clone(),
                        };
                        self.cur().ret = Some(t.clone());
                        t
                    }
                };
                if rt == Type::Void {
                    self.err(span, "cannot return a value from a void function");
                    return vec![st(StmtKind::Return(None), span)];
                }
                let x = self.coerce(x, &rt, e.span);
                let x = if matches!(rt, Type::Ref(..)) {
                    self.check_ref_return(&x, span);
                    x
                } else {
                    self.consume(x)
                };
                vec![st(StmtKind::Return(Some(x)), span)]
            }
            None => {
                match ret {
                    Some(Type::Void) => {}
                    None => self.cur().ret = Some(Type::Void),
                    Some(t) => {
                        let n = self.tname(&t);
                        self.err(span, format!("missing return value of type {}", n));
                    }
                }
                vec![st(StmtKind::Return(None), span)]
            }
        }
    }

    /// References may only be returned when borrowed from `this` (spec 9.5).
    fn check_ref_return(&mut self, x: &HExpr, span: Span) {
        let this = self.cur_ref().this_local;
        fn rooted(e: &HExpr, this: Option<LocalId>) -> bool {
            match &e.kind {
                H::Local(id) => Some(*id) == this,
                H::Field(o, _) => rooted(o, this),
                H::Convert(i) | H::Unwrap(i) => rooted(i, this),
                H::Call(_, args) | H::CallVirtual(_, args) => args.first().map(|a| rooted(a, this)).unwrap_or(false) && matches!(e.ty, Type::Ref(..)),
                H::Builtin(Builtin::ArrAt | Builtin::ArrFirst | Builtin::ArrLast | Builtin::DictGet, args) => rooted(&args[0], this),
                _ => false,
            }
        }
        if !rooted(x, this) {
            self.err(span, "a reference can only be returned if it is borrowed from 'this' (spec 9.5); return an owned value (e.g. .clone()) instead");
        }
    }

    fn for_each(&mut self, vars: &'a [String], iter: &'a ast::Expr, body: &'a ast::Block, span: Span) -> Vec<HStmt> {
        self.push_scope();
        let mut pre = Vec::new();
        // numeric ranges: for (i in T.range(a, b, step))
        if let A::Call(callee, args) = &iter.kind {
            if let A::Member(obj, m) = &callee.kind {
                if m == "range" {
                    if let A::Ident(tn) = &obj.kind {
                        if is_builtin_type_name(tn) && self.lookup_local(tn).is_none() {
                            let module = self.cur_ref().module;
                            let ty = self.resolve_type(&TypeExpr::named(tn, span), module, &Subst::new());
                            if ty.is_numeric() && (args.len() == 2 || args.len() == 3) && vars.len() == 1 {
                                let r = self.range_loop(&vars[0], ty, args, body, span);
                                let owned = self.pop_scope();
                                return vec![st(StmtKind::Block { body: r, drops: owned }, span)];
                            }
                        }
                    }
                }
            }
        }
        // x.kv()
        let mut kv = false;
        let mut src: &'a ast::Expr = iter;
        if let A::Call(callee, args) = &iter.kind {
            if let A::Member(obj, m) = &callee.kind {
                if m == "kv" && args.is_empty() {
                    kv = true;
                    src = obj;
                }
            }
        }
        let it = self.expr(src, None);
        let ity = it.ty.deref().clone();
        let owned_temp = !matches!(it.kind, H::Local(_) | H::Field(..) | H::Global(_) | H::Deref(_) | H::Unwrap(_));
        let itl = self.add_local("$it", ity.clone(), span, true, false);
        if owned_temp && !ity.is_copy() {
            self.cur().scopes.last_mut().unwrap().owned.push(itl);
        }
        pre.push(st(StmtKind::Let(itl, Some(it)), span));
        let it_read = HExpr::new(H::Local(itl), ity.clone(), span);
        let il = self.add_local("$i", Type::int64(), span, false, false);
        pre.push(st(StmtKind::Let(il, Some(HExpr::new(H::Lit(Lit::Int(0)), Type::int64(), span))), span));
        let i_read = HExpr::new(H::Local(il), Type::int64(), span);
        let (len_b, keys) = match &ity {
            Type::Array(_) => (Builtin::ArrLength, None),
            Type::Dict(kt, _) => {
                let kl = self.add_local("$keys", Type::Array(kt.clone()), span, true, false);
                pre.push(st(StmtKind::Let(kl, Some(HExpr::new(H::Builtin(Builtin::DictKeys, vec![it_read.clone()]), Type::Array(kt.clone()), span))), span));
                (Builtin::DictLength, Some((kl, (**kt).clone())))
            }
            Type::Error => {
                self.pop_scope();
                return Vec::new();
            }
            other => {
                let n = self.tname(other);
                self.err(iter.span, format!("cannot iterate over {}", n));
                self.pop_scope();
                return Vec::new();
            }
        };
        let nl = self.add_local("$n", Type::int64(), span, true, false);
        pre.push(st(StmtKind::Let(nl, Some(HExpr::new(H::Builtin(len_b, vec![it_read.clone()]), Type::int64(), span))), span));
        let cond = HExpr::new(H::Cmp(CmpOp::Lt, Box::new(i_read.clone()), Box::new(HExpr::new(H::Local(nl), Type::int64(), span))), Type::Bool, span);
        // loop body scope with the loop variables
        self.push_scope();
        let mut inner = Vec::new();
        let elem_decl = |t: &Type| if t.is_copy() { t.clone() } else { Type::Ref(false, Box::new(t.clone())) };
        match (&ity, keys) {
            (Type::Array(et), None) => {
                let at = HExpr::new(H::Builtin(Builtin::ArrAt, vec![it_read.clone(), i_read.clone()]), (**et).clone(), span);
                if vars.len() == 1 && !kv {
                    let id = self.declare(&vars[0], elem_decl(et), span, false, false, false);
                    inner.push(st(StmtKind::Let(id, Some(at)), span));
                } else if vars.len() == 2 && kv {
                    let a = self.declare(&vars[0], Type::int64(), span, false, false, false);
                    inner.push(st(StmtKind::Let(a, Some(i_read.clone())), span));
                    let b = self.declare(&vars[1], elem_decl(et), span, false, false, false);
                    inner.push(st(StmtKind::Let(b, Some(at)), span));
                } else if vars.len() == 1 && kv {
                    let tt = Type::Tuple(vec![Type::int64(), (**et).clone()]);
                    let id = self.declare(&vars[0], tt.clone(), span, false, false, false);
                    inner.push(st(StmtKind::Let(id, Some(HExpr::new(H::Tuple(vec![i_read.clone(), at]), tt, span))), span));
                } else {
                    self.err(span, "iterating an array binds one variable (or two with .kv())");
                }
            }
            (Type::Dict(_, vt), Some((kl, kt))) => {
                let k_at = HExpr::new(H::Builtin(Builtin::ArrAt, vec![HExpr::new(H::Local(kl), Type::Array(Box::new(kt.clone())), span), i_read.clone()]), kt.clone(), span);
                let kid = self.declare(&vars[0], elem_decl(&kt), span, false, false, false);
                inner.push(st(StmtKind::Let(kid, Some(k_at)), span));
                if vars.len() == 2 {
                    let kread = HExpr::new(H::Local(kid), kt.clone(), span);
                    let v = HExpr::new(H::Builtin(Builtin::DictGet, vec![it_read.clone(), kread]), (**vt).clone(), span);
                    let vid = self.declare(&vars[1], elem_decl(vt), span, false, false, false);
                    inner.push(st(StmtKind::Let(vid, Some(v)), span));
                } else if vars.len() != 1 {
                    self.err(span, "iterating a Dictionary binds one (key) or two (key, value) variables");
                }
            }
            _ => {}
        }
        self.cur().loops += 1;
        let b = self.block(body);
        self.cur().loops -= 1;
        inner.extend(b);
        let owned_inner = self.pop_scope();
        let step = vec![st(
            StmtKind::Assign(
                Place::Local(il),
                HExpr::new(H::Arith(ArithOp::Add, Box::new(i_read.clone()), Box::new(HExpr::new(H::Lit(Lit::Int(1)), Type::int64(), span)), true), Type::int64(), span),
            ),
            span,
        )];
        pre.push(st(StmtKind::Loop { cond: Some(cond), body: vec![st(StmtKind::Block { body: inner, drops: owned_inner }, span)], step }, span));
        let owned = self.pop_scope();
        vec![st(StmtKind::Block { body: pre, drops: owned }, span)]
    }

    fn range_loop(&mut self, var: &str, ty: Type, args: &'a [ast::Arg], body: &'a ast::Block, span: Span) -> Vec<HStmt> {
        let mut pre = Vec::new();
        let mut vals = Vec::new();
        for a in args {
            if a.name.is_some() {
                self.err(a.value.span, "range does not take named arguments");
            }
            let x = self.expr(&a.value, Some(&ty));
            vals.push(self.coerce(x, &ty, a.value.span));
        }
        let wrap = self.cur_ref().wrap;
        let start = vals.remove(0);
        let end = vals.remove(0);
        let el = self.add_local("$end", ty.clone(), span, true, false);
        pre.push(st(StmtKind::Let(el, Some(end)), span));
        let step_lit: Option<f64> = match args.get(2).map(|a| &a.value.kind) {
            None => Some(1.0),
            Some(A::Int(v)) => Some(*v as f64),
            Some(A::Float(v)) => Some(*v),
            Some(A::Unary(ast::UnOp::Neg, inner)) => match inner.kind {
                A::Int(v) => Some(-(v as f64)),
                A::Float(v) => Some(-v),
                _ => None,
            },
            _ => None,
        };
        let step_expr = if vals.is_empty() { self.one(&ty, span) } else { vals.remove(0) };
        let sl = self.add_local("$step", ty.clone(), span, true, false);
        pre.push(st(StmtKind::Let(sl, Some(step_expr)), span));
        if step_lit == Some(0.0) {
            self.err(span, "range step must not be 0");
        }
        let vid = self.declare(var, ty.clone(), span, false, false, false);
        pre.push(st(StmtKind::Let(vid, Some(start)), span));
        let v = HExpr::new(H::Local(vid), ty.clone(), span);
        let e = HExpr::new(H::Local(el), ty.clone(), span);
        let s = HExpr::new(H::Local(sl), ty.clone(), span);
        let zero = self.zero(&ty, span);
        let up = HExpr::new(H::Cmp(CmpOp::Lt, Box::new(v.clone()), Box::new(e.clone())), Type::Bool, span);
        let down = HExpr::new(H::Cmp(CmpOp::Gt, Box::new(v.clone()), Box::new(e.clone())), Type::Bool, span);
        let cond = match step_lit {
            Some(x) if x > 0.0 => up,
            Some(_) => down,
            None => {
                let pos = HExpr::new(H::Cmp(CmpOp::Gt, Box::new(s.clone()), Box::new(zero.clone())), Type::Bool, span);
                let neg = HExpr::new(H::Cmp(CmpOp::Lt, Box::new(s.clone()), Box::new(zero)), Type::Bool, span);
                let a = HExpr::new(H::And(Box::new(pos), Box::new(up)), Type::Bool, span);
                let b = HExpr::new(H::And(Box::new(neg), Box::new(down)), Type::Bool, span);
                HExpr::new(H::Or(Box::new(a), Box::new(b)), Type::Bool, span)
            }
        };
        self.cur().loops += 1;
        let b = self.block(body);
        self.cur().loops -= 1;
        let step = vec![st(StmtKind::Assign(Place::Local(vid), HExpr::new(H::Arith(ArithOp::Add, Box::new(v), Box::new(s), wrap), ty, span)), span)];
        pre.push(st(StmtKind::Loop { cond: Some(cond), body: b, step }, span));
        pre
    }

    fn zero(&mut self, ty: &Type, span: Span) -> HExpr {
        let lit = match ty {
            Type::Float(_) => Lit::Float(0.0),
            Type::Big => Lit::Big("0".into()),
            _ => Lit::Int(0),
        };
        HExpr::new(H::Lit(lit), ty.clone(), span)
    }

    fn one(&mut self, ty: &Type, span: Span) -> HExpr {
        let lit = match ty {
            Type::Float(_) => Lit::Float(1.0),
            Type::Big => Lit::Big("1".into()),
            _ => Lit::Int(1),
        };
        HExpr::new(H::Lit(lit), ty.clone(), span)
    }
}

/// Whether the statements assign to a variable with this name (conservative, syntactic).
fn assigns_local(stmts: &[ast::Stmt], name: &str) -> bool {
    fn target_is(e: &ast::Expr, name: &str) -> bool {
        matches!(&e.kind, A::Ident(n) if n == name)
    }
    for s in stmts {
        let hit = match &s.kind {
            S::Assign { target, .. } => target_is(target, name),
            S::MultiAssign { targets, .. } => targets.iter().flatten().any(|t| target_is(t, name)),
            S::If { then, els, .. } => assigns_local(&then.stmts, name) || els.as_ref().map(|e| assigns_local(std::slice::from_ref(&**e), name)).unwrap_or(false),
            S::ForC { init, step, body, .. } => {
                assigns_local(&body.stmts, name)
                    || init.as_ref().map(|i| assigns_local(std::slice::from_ref(&**i), name)).unwrap_or(false)
                    || step.as_ref().map(|i| assigns_local(std::slice::from_ref(&**i), name)).unwrap_or(false)
            }
            S::ForEach { body, .. } => assigns_local(&body.stmts, name),
            S::Switch { cases, .. } => cases.iter().any(|c| assigns_local(&c.body, name)),
            S::Try { body, catches, finally } => {
                assigns_local(&body.stmts, name) || catches.iter().any(|c| assigns_local(&c.body.stmts, name)) || finally.as_ref().map(|f| assigns_local(&f.stmts, name)).unwrap_or(false)
            }
            S::Block(b) => assigns_local(&b.stmts, name),
            _ => false,
        };
        if hit {
            return true;
        }
    }
    false
}

/// Whether a statement list always leaves the enclosing block (return/throw/break/continue).
fn always_exits(stmts: &[ast::Stmt]) -> bool {
    match stmts.last().map(|s| &s.kind) {
        Some(S::Return(_)) | Some(S::Throw(_)) | Some(S::Break) | Some(S::Continue) => true,
        Some(S::Block(b)) => always_exits(&b.stmts),
        Some(S::If { then, els: Some(e), .. }) => always_exits(&then.stmts) && always_exits(std::slice::from_ref(&**e)),
        _ => false,
    }
}
