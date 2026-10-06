//! Flow analysis on HIR: use-after-move, definite assignment of locals, constructor field
//! initialisation (including `Immutable` exactly-once), missing returns and a simplified borrow
//! checker (spec 9.2, 9.3, 10.4).

use crate::diag::{Diag, Span};
use crate::hir::*;
use crate::types::*;
use std::collections::HashSet;

#[derive(Clone, PartialEq, Eq, Default)]
struct BitSet(Vec<u64>);

impl BitSet {
    fn has(&self, i: u32) -> bool {
        let (w, b) = ((i / 64) as usize, i % 64);
        self.0.get(w).map(|x| x & (1 << b) != 0).unwrap_or(false)
    }
    fn set(&mut self, i: u32) {
        let (w, b) = ((i / 64) as usize, i % 64);
        if self.0.len() <= w {
            self.0.resize(w + 1, 0);
        }
        self.0[w] |= 1 << b;
    }
    fn clear(&mut self, i: u32) {
        let (w, b) = ((i / 64) as usize, i % 64);
        if let Some(x) = self.0.get_mut(w) {
            *x &= !(1 << b);
        }
    }
    fn union(&mut self, o: &BitSet) {
        if self.0.len() < o.0.len() {
            self.0.resize(o.0.len(), 0);
        }
        for (a, b) in self.0.iter_mut().zip(o.0.iter()) {
            *a |= *b;
        }
    }
    fn intersect(&mut self, o: &BitSet) {
        for (i, a) in self.0.iter_mut().enumerate() {
            *a &= o.0.get(i).copied().unwrap_or(0);
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct State {
    reachable: bool,
    moved: BitSet,
    uninit: BitSet,
    fields_def: BitSet,
    fields_maybe: BitSet,
}

impl State {
    fn unreachable() -> State {
        State { reachable: false, moved: BitSet::default(), uninit: BitSet::default(), fields_def: BitSet::default(), fields_maybe: BitSet::default() }
    }
    fn merge(&mut self, o: &State) {
        if !o.reachable {
            return;
        }
        if !self.reachable {
            *self = o.clone();
            return;
        }
        self.moved.union(&o.moved);
        self.uninit.union(&o.uninit);
        self.fields_def.intersect(&o.fields_def);
        self.fields_maybe.union(&o.fields_maybe);
    }
}

struct Flow<'p> {
    p: &'p Program,
    f: &'p Func,
    errors: Vec<Diag>,
    reported: HashSet<(u32, Span)>,
    loop_stack: Vec<(State, State)>, // (break-merge, continue-merge)
    breakable: Vec<bool>,            // true = loop, false = switch
    switch_breaks: Vec<State>,
    returns: State,
    ctor_class: Option<ClassId>,
    own_fields: std::ops::Range<usize>,
    in_ctor_this: Option<LocalId>,
}

pub fn check_program(p: &mut Program) -> Vec<Diag> {
    let mut errors = Vec::new();
    for f in &p.funcs {
        let mut fl = Flow {
            p,
            f,
            errors: Vec::new(),
            reported: HashSet::new(),
            loop_stack: Vec::new(),
            breakable: Vec::new(),
            switch_breaks: Vec::new(),
            returns: State::unreachable(),
            ctor_class: None,
            own_fields: 0..0,
            in_ctor_this: None,
        };
        if f.kind == FuncKind::Ctor {
            if let Some(c) = f.this_class {
                fl.ctor_class = Some(c);
                let start = p.classes[c as usize].parent.map(|pc| p.classes[pc as usize].fields.len()).unwrap_or(0);
                fl.own_fields = start..p.classes[c as usize].fields.len();
                fl.in_ctor_this = f.params.first().copied();
            }
        }
        let mut st = State { reachable: true, moved: BitSet::default(), uninit: BitSet::default(), fields_def: BitSet::default(), fields_maybe: BitSet::default() };
        // nullable non-Immutable fields start as assigned (implicitly Null)
        if let Some(c) = fl.ctor_class {
            for i in fl.own_fields.clone() {
                let fi = &p.classes[c as usize].fields[i];
                if fi.ty.is_nullable() && !fi.immutable {
                    st.fields_def.set(i as u32);
                }
            }
        }
        fl.stmts(&f.body, &mut st);
        let mut end = st.clone();
        end.merge(&fl.returns);
        if st.reachable && !matches!(f.ret, Type::Void | Type::Never | Type::Error) {
            fl.errors.push(Diag::error(f.span, format!("missing return statement in '{}'", f.name)));
        }
        if let Some(c) = fl.ctor_class {
            if end.reachable {
                for i in fl.own_fields.clone() {
                    let fi = &p.classes[c as usize].fields[i];
                    if !end.fields_def.has(i as u32) {
                        let msg = if fi.immutable {
                            format!("Immutable field '{}' must be assigned exactly once in every constructor", fi.name)
                        } else {
                            format!("field '{}' is not initialized by this constructor", fi.name)
                        };
                        fl.errors.push(Diag::error(f.span, msg));
                    }
                }
            }
        }
        borrow_check(p, f, &f.body, &mut fl.errors);
        errors.extend(fl.errors);
    }
    errors
}

impl<'p> Flow<'p> {
    fn err(&mut self, local: u32, span: Span, msg: String) {
        if self.reported.insert((local, span)) {
            self.errors.push(Diag::error(span, msg));
        }
    }

    fn name(&self, id: LocalId) -> String {
        self.f.locals[id as usize].name.clone()
    }

    fn use_local(&mut self, id: LocalId, span: Span, st: &State) {
        if !st.reachable {
            return;
        }
        if st.uninit.has(id) {
            let n = self.name(id);
            self.err(id, span, format!("variable '{}' is used before it is definitely assigned", n));
        } else if st.moved.has(id) {
            let n = self.name(id);
            self.err(id, span, format!("use of moved value '{}' (ownership was transferred; use .clone() or declare it 'copied')", n));
        }
    }

    fn place(&mut self, p: &Place, st: &mut State, span: Span, is_write: bool) {
        match p {
            Place::Local(id) => {
                if !is_write {
                    self.use_local(*id, span, st);
                }
            }
            Place::Deref(id) => self.use_local(*id, span, st),
            Place::Field(o, _) => self.expr(o, st),
            Place::Global(_) => {}
        }
    }

    fn expr(&mut self, e: &Expr, st: &mut State) {
        match &e.kind {
            ExprKind::Lit(_) | ExprKind::Global(_) | ExprKind::FuncRef(_) => {}
            ExprKind::Local(id) => self.use_local(*id, e.span, st),
            ExprKind::Move(id) => {
                self.use_local(*id, e.span, st);
                if st.reachable {
                    st.moved.set(*id);
                }
            }
            ExprKind::Deref(a) | ExprKind::Field(a, _) | ExprKind::TupleGet(a, _) | ExprKind::Unary(_, a) | ExprKind::NonNull(a) | ExprKind::Unwrap(a) | ExprKind::Convert(a) | ExprKind::Cast(a, _) => {
                self.expr(a, st)
            }
            ExprKind::Arith(_, a, b, _) | ExprKind::Cmp(_, a, b) => {
                self.expr(a, st);
                self.expr(b, st);
            }
            ExprKind::And(a, b) | ExprKind::Or(a, b) | ExprKind::Coalesce(a, b) => {
                self.expr(a, st);
                let mut s2 = st.clone();
                self.expr(b, &mut s2);
                st.merge(&s2);
            }
            ExprKind::Ternary(c, a, b) => {
                self.expr(c, st);
                let mut s1 = st.clone();
                let mut s2 = st.clone();
                self.expr(a, &mut s1);
                self.expr(b, &mut s2);
                s1.merge(&s2);
                *st = s1;
            }
            ExprKind::Concat(xs) | ExprKind::Call(_, xs) | ExprKind::CallVirtual(_, xs) | ExprKind::New(_, _, xs) | ExprKind::Builtin(_, xs) | ExprKind::Tuple(xs) => {
                for x in xs {
                    self.expr(x, st);
                }
            }
            ExprKind::CallClosure(c, xs) => {
                self.expr(c, st);
                for x in xs {
                    self.expr(x, st);
                }
            }
            ExprKind::BuiltinMut(_, p, xs) => {
                for x in xs {
                    self.expr(x, st);
                }
                self.place(p, st, e.span, false);
            }
            ExprKind::Lambda(_, caps) => {
                for c in caps {
                    match c {
                        CaptureSrc::Ref(p) => self.place(p, st, e.span, false),
                        CaptureSrc::Value(x) => self.expr(x, st),
                    }
                }
            }
            ExprKind::Dict(kv) => {
                for (k, v) in kv {
                    self.expr(k, st);
                    self.expr(v, st);
                }
            }
            ExprKind::RefMut(p) => self.place(p, st, e.span, false),
            ExprKind::Seq(stmts, x) => {
                self.stmts(stmts, st);
                self.expr(x, st);
            }
        }
    }

    fn stmts(&mut self, ss: &[Stmt], st: &mut State) {
        for s in ss {
            self.stmt(s, st);
        }
    }

    fn stmt(&mut self, s: &Stmt, st: &mut State) {
        match &s.kind {
            StmtKind::Let(id, init) => {
                match init {
                    Some(e) => {
                        self.expr(e, st);
                        st.uninit.clear(*id);
                    }
                    None => st.uninit.set(*id),
                }
                st.moved.clear(*id);
            }
            StmtKind::Assign(p, e) => {
                self.expr(e, st);
                self.place(p, st, s.span, true);
                match p {
                    Place::Local(id) => {
                        st.moved.clear(*id);
                        st.uninit.clear(*id);
                    }
                    Place::Field(o, idx) => {
                        if let (Some(c), ExprKind::Local(l)) = (self.ctor_class, &o.kind) {
                            if Some(*l) == self.in_ctor_this && self.own_fields.contains(&(*idx as usize)) && st.reachable {
                                let fi = &self.p.classes[c as usize].fields[*idx as usize];
                                if fi.immutable && st.fields_maybe.has(*idx) {
                                    let n = fi.name.clone();
                                    self.err(u32::MAX - *idx, s.span, format!("Immutable field '{}' may be assigned more than once", n));
                                }
                                st.fields_def.set(*idx);
                                st.fields_maybe.set(*idx);
                            }
                        }
                    }
                    _ => {}
                }
            }
            StmtKind::Expr(e) | StmtKind::Free(e) => self.expr(e, st),
            StmtKind::If(c, a, b) => {
                self.expr(c, st);
                let mut s1 = st.clone();
                let mut s2 = st.clone();
                self.stmts(a, &mut s1);
                self.stmts(b, &mut s2);
                s1.merge(&s2);
                *st = s1;
            }
            StmtKind::Loop { cond, body, step } => {
                let mut entry = st.clone();
                let mut exit = State::unreachable();
                for _ in 0..3 {
                    let mut s = entry.clone();
                    if let Some(c) = cond {
                        self.expr(c, &mut s);
                    }
                    let mut after_cond = s.clone();
                    if cond.is_none() {
                        after_cond = State::unreachable();
                    }
                    self.loop_stack.push((State::unreachable(), State::unreachable()));
                    self.breakable.push(true);
                    self.stmts(body, &mut s);
                    self.breakable.pop();
                    let (brk, cont) = self.loop_stack.pop().unwrap();
                    s.merge(&cont);
                    self.stmts(step, &mut s);
                    let mut next = entry.clone();
                    next.merge(&s);
                    exit = after_cond;
                    exit.merge(&brk);
                    if next == entry {
                        break;
                    }
                    entry = next;
                }
                *st = exit;
            }
            StmtKind::Break => {
                if let Some(true) = self.breakable.last() {
                    if let Some(top) = self.loop_stack.last_mut() {
                        top.0.merge(st);
                    }
                } else if let Some(top) = self.switch_breaks.last_mut() {
                    top.merge(st);
                }
                *st = State::unreachable();
            }
            StmtKind::Continue => {
                if let Some(top) = self.loop_stack.last_mut() {
                    top.1.merge(st);
                }
                *st = State::unreachable();
            }
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e, st);
                }
                self.returns.merge(st);
                *st = State::unreachable();
            }
            StmtKind::Throw(e) => {
                self.expr(e, st);
                *st = State::unreachable();
            }
            StmtKind::Try { body, catches, finally } => {
                let entry = st.clone();
                let mut s = st.clone();
                self.stmts(body, &mut s);
                // a catch may start from any point of the try body
                let mut catch_entry = entry.clone();
                catch_entry.merge(&s);
                catch_entry.reachable = entry.reachable;
                let mut out = s.clone();
                for c in catches {
                    let mut cs = catch_entry.clone();
                    // fields assigned in the try body are only "maybe" assigned here
                    cs.fields_def = entry.fields_def.clone();
                    cs.uninit.clear(c.local);
                    cs.moved.clear(c.local);
                    self.stmts(&c.body, &mut cs);
                    out.merge(&cs);
                }
                if let Some(fb) = finally {
                    let mut fs = out.clone();
                    if !fs.reachable {
                        fs = catch_entry.clone();
                    }
                    self.stmts(fb, &mut fs);
                    if out.reachable {
                        out.moved.union(&fs.moved);
                        if !fs.reachable {
                            out = State::unreachable();
                        }
                    }
                }
                *st = out;
            }
            StmtKind::Block { body, .. } => self.stmts(body, st),
            StmtKind::Switch { cases } => {
                let entry = st.clone();
                for c in cases {
                    if let Some(e) = &c.cond {
                        self.expr(e, st);
                    }
                }
                let after_conds = st.clone();
                let mut out = State::unreachable();
                let has_default = cases.iter().any(|c| c.cond.is_none());
                if !has_default {
                    out.merge(&after_conds);
                }
                self.switch_breaks.push(State::unreachable());
                self.breakable.push(false);
                let mut prev: Option<State> = None;
                for c in cases {
                    let mut s = after_conds.clone();
                    if let Some(p) = prev.take() {
                        s.merge(&p);
                    }
                    self.stmts(&c.body, &mut s);
                    if c.fallthrough {
                        prev = Some(s);
                    } else {
                        out.merge(&s);
                    }
                }
                if let Some(p) = prev {
                    out.merge(&p);
                }
                self.breakable.pop();
                let brk = self.switch_breaks.pop().unwrap();
                out.merge(&brk);
                let _ = entry;
                *st = out;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// simplified borrow checking

fn root_local(e: &Expr) -> Option<LocalId> {
    match &e.kind {
        ExprKind::Local(id) | ExprKind::Move(id) => Some(*id),
        ExprKind::Field(o, _) | ExprKind::Deref(o) | ExprKind::Convert(o) | ExprKind::Unwrap(o) => root_local(o),
        ExprKind::RefMut(p) => match &**p {
            Place::Local(id) | Place::Deref(id) => Some(*id),
            Place::Field(o, _) => root_local(o),
            Place::Global(_) => None,
        },
        ExprKind::Call(_, args) | ExprKind::CallVirtual(_, args) if matches!(e.ty, Type::Ref(..)) => args.first().and_then(root_local),
        ExprKind::Builtin(l2_runtime::Builtin::ArrAt | l2_runtime::Builtin::ArrFirst | l2_runtime::Builtin::ArrLast | l2_runtime::Builtin::DictGet, args) => {
            args.first().and_then(root_local)
        }
        _ => None,
    }
}

fn mentions(stmts: &[Stmt], id: LocalId) -> bool {
    let mut found = false;
    for s in stmts {
        crate::hir_visit::walk_stmts(std::slice::from_ref(s), &mut |s2| {
            crate::hir_visit::walk_stmt_exprs(s2, &mut |e| {
                if matches!(e.kind, ExprKind::Local(l) | ExprKind::Move(l) if l == id) {
                    found = true;
                }
                if let ExprKind::RefMut(p) = &e.kind {
                    if matches!(**p, Place::Local(l) | Place::Deref(l) if l == id) {
                        found = true;
                    }
                }
            });
            if let StmtKind::Assign(Place::Local(l) | Place::Deref(l), _) = &s2.kind {
                if *l == id {
                    found = true;
                }
            }
        });
    }
    found
}

/// Kinds of conflicting access to a borrowed root.
fn conflicts(stmts: &[Stmt], root: LocalId, mutable_borrow: bool) -> Option<(Span, &'static str)> {
    let mut hit = None;
    crate::hir_visit::walk_stmts(stmts, &mut |s| {
        if hit.is_some() {
            return;
        }
        if let StmtKind::Assign(Place::Local(l), _) = &s.kind {
            if *l == root {
                hit = Some((s.span, "assign to"));
            }
        }
        crate::hir_visit::walk_stmt_exprs(s, &mut |e| {
            if hit.is_some() {
                return;
            }
            match &e.kind {
                ExprKind::Move(l) if *l == root => hit = Some((e.span, "move")),
                ExprKind::RefMut(p) if matches!(**p, Place::Local(l) if l == root) => hit = Some((e.span, "mutably borrow")),
                ExprKind::BuiltinMut(_, p, _) if matches!(**p, Place::Local(l) if l == root) => hit = Some((e.span, "modify")),
                ExprKind::Local(l) if *l == root && mutable_borrow => hit = Some((e.span, "use")),
                _ => {}
            }
        });
    });
    hit
}

fn borrow_check(p: &Program, f: &Func, stmts: &[Stmt], errors: &mut Vec<Diag>) {
    if p.config.memory == MemoryMode::Manual {
        return;
    }
    for (i, s) in stmts.iter().enumerate() {
        // nested statement lists
        match &s.kind {
            StmtKind::If(_, a, b) => {
                borrow_check(p, f, a, errors);
                borrow_check(p, f, b, errors);
            }
            StmtKind::Loop { body, step, .. } => {
                borrow_check(p, f, body, errors);
                borrow_check(p, f, step, errors);
            }
            StmtKind::Try { body, catches, finally } => {
                borrow_check(p, f, body, errors);
                for c in catches {
                    borrow_check(p, f, &c.body, errors);
                }
                if let Some(fb) = finally {
                    borrow_check(p, f, fb, errors);
                }
            }
            StmtKind::Block { body, .. } => borrow_check(p, f, body, errors),
            StmtKind::Switch { cases } => {
                for c in cases {
                    borrow_check(p, f, &c.body, errors);
                }
            }
            _ => {}
        }
        // a call that passes `*x` together with another use of x
        crate::hir_visit::walk_stmt_exprs(s, &mut |e| {
            let args = match &e.kind {
                ExprKind::Call(_, a) | ExprKind::CallVirtual(_, a) | ExprKind::New(_, _, a) => a,
                ExprKind::CallClosure(_, a) => a,
                _ => return,
            };
            for (ai, a) in args.iter().enumerate() {
                if let ExprKind::RefMut(pl) = &a.kind {
                    if let Place::Local(x) = &**pl {
                        for (bi, b) in args.iter().enumerate() {
                            if ai != bi && root_local(b) == Some(*x) {
                                let n = f.locals[*x as usize].name.clone();
                                errors.push(Diag::error(a.span, format!("cannot borrow '{}' mutably because it is also used by another argument of the same call", n)));
                            }
                        }
                    }
                }
            }
        });
        // reference locals: the root may not be modified while the reference is live
        if let StmtKind::Let(r, Some(init)) = &s.kind {
            if let Type::Ref(m, _) = &f.locals[*r as usize].ty {
                if let Some(root) = root_local(init) {
                    if root == *r {
                        continue;
                    }
                    let rest = &stmts[i + 1..];
                    let last = rest.iter().rposition(|s| mentions(std::slice::from_ref(s), *r));
                    if let Some(last) = last {
                        if let Some((span, what)) = conflicts(&rest[..=last], root, *m) {
                            let (rn, bn) = (f.locals[root as usize].name.clone(), f.locals[*r as usize].name.clone());
                            errors.push(Diag::error(span, format!("cannot {} '{}' while it is borrowed by '{}'", what, rn, bn)));
                        }
                    }
                }
            }
        }
    }
}
