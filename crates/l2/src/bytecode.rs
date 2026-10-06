//! Bytecode compiler: lowers HIR functions to a compact stack-machine instruction set.

use crate::hir::*;
use crate::types::*;
use l2_runtime::ops::{ArithOp, CmpOp};
use l2_runtime::{BigInt, Builtin, FloatTy, IntTy, RtType, Value};
use std::fmt::Write as _;
use std::rc::Rc;

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Const(u32),
    Pop,
    Dup,
    LoadLocal(u32),
    /// Load a local, bypassing cell indirection (the raw `Ref` for `*T` locals).
    LoadLocalRaw(u32),
    StoreLocal(u32, bool),
    /// Declaration: like StoreLocal, but cell locals get a fresh cell.
    InitLocal(u32),
    MoveLocal(u32),
    /// Push a reference to a local's storage (cells).
    LocalRef(u32),
    LoadGlobal(u32),
    StoreGlobal(u32, bool),
    GlobalRef(u32),
    GetField(u32),
    /// stack: obj, value
    SetField(u32, bool),
    /// stack: obj -> ref
    FieldRef(u32),
    /// stack: parent ref, key -> element ref
    ElemRef,
    /// stack: value, ref
    StoreRef(bool),
    Deref,
    /// stack: value; writes through the `*T` held in the local
    StoreDeref(u32, bool),
    TupleGet(u32),
    MakeTuple(u32),
    MakeDict(u32),
    Arith(ArithOp, bool),
    Neg(bool),
    BitNot,
    Not,
    Cmp(CmpOp),
    Concat(u32),
    Jump(u32),
    JumpIfFalse(u32),
    JumpIfTrue(u32),
    /// If top is not null jump (keeping it), else pop it.
    JumpIfNotNull(u32),
    NonNull(bool),
    Convert(u32),
    Cast(u32, bool),
    Call(u32, u32),
    CallVirtual(u32, u32),
    CallClosure(u32),
    New(u32, u32, u32),
    Builtin(Builtin, u32),
    BuiltinMutLocal(Builtin, u32, u32),
    /// stack: ref, args...
    BuiltinMutRef(Builtin, u32),
    MakeClosure(u32, u32),
    FuncRef(u32),
    Return,
    ReturnVoid,
    Throw,
    /// Peek the exception and push whether it is an instance of one of the listed classes.
    CatchMatch(u32),
    DropLocal(u32),
    DropTop,
    Free,
}

#[derive(Clone, Debug)]
pub struct Handler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
    /// Operand stack height to restore before pushing the exception.
    pub base: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Chunk {
    pub name: String,
    pub code: Vec<Op>,
    pub consts: Vec<Value>,
    pub types: Vec<RtType>,
    pub lists: Vec<Vec<ClassId>>,
    pub handlers: Vec<Handler>,
    pub nlocals: u32,
    pub params: Vec<LocalId>,
    pub cells: Vec<bool>,
    pub captures: Vec<(LocalId, bool)>,
    pub wrap: bool,
}

pub struct Module {
    pub chunks: Vec<Chunk>,
}

enum Cleanup<'h> {
    Finally(&'h [Stmt]),
    Drops(Vec<LocalId>),
}

struct Breakable {
    is_loop: bool,
    cleanup_depth: usize,
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

/// A protected code region; it may be split into several ranges when cleanup code that must
/// not be covered by the handler is emitted inside it.
struct Region {
    ranges: Vec<(u32, u32)>,
    open: Option<u32>,
    cleanup_depth: usize,
}

struct FnCompiler<'h> {
    p: &'h Program,
    f: &'h Func,
    c: Chunk,
    cleanups: Vec<Cleanup<'h>>,
    /// region index of each cleanup entry (the region that protects the code inside it)
    regions: Vec<Region>,
    breakables: Vec<Breakable>,
    ownership: bool,
}

pub fn compile(p: &Program) -> Module {
    let chunks = p.funcs.iter().map(|f| compile_func(p, f)).collect();
    Module { chunks }
}

fn compile_func(p: &Program, f: &Func) -> Chunk {
    let mut fc = FnCompiler {
        p,
        f,
        c: Chunk {
            name: f.name.clone(),
            nlocals: f.locals.len() as u32,
            params: f.params.clone(),
            cells: f.locals.iter().map(|l| l.cell).collect(),
            captures: f.captures.iter().map(|c| (c.inner, c.by_ref)).collect(),
            wrap: f.wrap,
            ..Default::default()
        },
        cleanups: Vec::new(),
        regions: Vec::new(),
        breakables: Vec::new(),
        ownership: p.config.memory == MemoryMode::Ownership,
    };
    fc.stmts(&f.body);
    fc.emit(Op::ReturnVoid);
    let mut c = fc.c;
    compute_depths(&mut c);
    c
}

/// Stack effect of an instruction: (pops, pushes).
fn effect(op: &Op) -> (u32, u32) {
    match *op {
        Op::Const(_) | Op::Dup | Op::LoadLocal(_) | Op::LoadLocalRaw(_) | Op::MoveLocal(_) | Op::LocalRef(_) | Op::LoadGlobal(_) | Op::GlobalRef(_) | Op::FuncRef(_) => (0, 1),
        Op::Pop | Op::StoreLocal(..) | Op::InitLocal(_) | Op::StoreGlobal(..) | Op::StoreDeref(..) | Op::JumpIfFalse(_) | Op::JumpIfTrue(_) | Op::DropTop | Op::Free | Op::Return | Op::Throw => (1, 0),
        Op::SetField(..) | Op::StoreRef(_) => (2, 0),
        Op::ElemRef => (2, 1),
        Op::GetField(_) | Op::FieldRef(_) | Op::Deref | Op::TupleGet(_) | Op::Neg(_) | Op::BitNot | Op::Not | Op::NonNull(_) | Op::Convert(_) | Op::Cast(..) => (1, 1),
        Op::MakeTuple(n) | Op::Concat(n) | Op::Call(_, n) | Op::CallVirtual(_, n) | Op::New(_, _, n) | Op::Builtin(_, n) | Op::BuiltinMutLocal(_, _, n) | Op::MakeClosure(_, n) => (n, 1),
        Op::MakeDict(n) => (2 * n, 1),
        Op::Arith(..) | Op::Cmp(_) => (2, 1),
        Op::CallClosure(n) | Op::BuiltinMutRef(_, n) => (n + 1, 1),
        Op::CatchMatch(_) => (0, 1),
        Op::Jump(_) | Op::JumpIfNotNull(_) | Op::DropLocal(_) | Op::ReturnVoid => (0, 0),
    }
}

/// Computes the operand stack height at every handler's protected region.
fn compute_depths(c: &mut Chunk) {
    let n = c.code.len();
    let mut depth: Vec<Option<u32>> = vec![None; n + 1];
    let mut work = vec![(0usize, 0u32)];
    let mut handler_done = vec![false; c.handlers.len()];
    loop {
        while let Some((pc, d)) = work.pop() {
            if pc >= n || depth[pc].is_some() {
                continue;
            }
            depth[pc] = Some(d);
            let op = c.code[pc];
            match op {
                Op::Jump(t) => work.push((t as usize, d)),
                Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => {
                    work.push((t as usize, d - 1));
                    work.push((pc + 1, d - 1));
                }
                Op::JumpIfNotNull(t) => {
                    work.push((t as usize, d));
                    work.push((pc + 1, d - 1));
                }
                Op::Return | Op::ReturnVoid | Op::Throw => {}
                _ => {
                    let (pops, pushes) = effect(&op);
                    work.push((pc + 1, d - pops + pushes));
                }
            }
        }
        let mut progressed = false;
        for (i, h) in c.handlers.iter_mut().enumerate() {
            if handler_done[i] {
                continue;
            }
            if let Some(d) = (h.start..h.end).filter_map(|pc| depth[pc as usize]).min() {
                h.base = d;
                handler_done[i] = true;
                work.push((h.target as usize, d + 1));
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
}

impl<'h> FnCompiler<'h> {
    fn emit(&mut self, op: Op) -> usize {
        self.c.code.push(op);
        self.c.code.len() - 1
    }
    fn here(&self) -> u32 {
        self.c.code.len() as u32
    }
    fn patch(&mut self, at: usize, target: u32) {
        self.c.code[at] = match self.c.code[at] {
            Op::Jump(_) => Op::Jump(target),
            Op::JumpIfFalse(_) => Op::JumpIfFalse(target),
            Op::JumpIfTrue(_) => Op::JumpIfTrue(target),
            Op::JumpIfNotNull(_) => Op::JumpIfNotNull(target),
            other => other,
        };
    }
    fn konst(&mut self, v: Value) -> u32 {
        self.c.consts.push(v);
        (self.c.consts.len() - 1) as u32
    }
    fn rt(&mut self, t: &Type) -> u32 {
        let r = rt_type(t);
        if let Some(i) = self.c.types.iter().position(|x| *x == r) {
            return i as u32;
        }
        self.c.types.push(r);
        (self.c.types.len() - 1) as u32
    }
    fn needs_drop(&self, t: &Type) -> bool {
        self.ownership && self.p.needs_drop(t)
    }

    // ------------------------------------------------------------------ regions
    fn open_region(&mut self) -> usize {
        let start = self.here();
        self.regions.push(Region { ranges: Vec::new(), open: Some(start), cleanup_depth: self.cleanups.len() });
        self.regions.len() - 1
    }
    fn close_region(&mut self, idx: usize) -> Vec<(u32, u32)> {
        let here = self.here();
        let r = &mut self.regions[idx];
        if let Some(s) = r.open.take() {
            if here > s {
                r.ranges.push((s, here));
            }
        }
        std::mem::take(&mut r.ranges)
    }
    fn add_handler(&mut self, ranges: Vec<(u32, u32)>, target: u32) {
        for (s, e) in ranges {
            self.c.handlers.push(Handler { start: s, end: e, target, base: 0 });
        }
    }

    /// Emits cleanup code (finally blocks / drops) for leaving to cleanup depth `depth`.
    fn emit_cleanups(&mut self, depth: usize) {
        let n = self.cleanups.len();
        if n <= depth {
            return;
        }
        // regions opened at or above `depth` must not cover the cleanup code
        let here = self.here();
        let suspended: Vec<usize> = (0..self.regions.len()).filter(|&i| self.regions[i].cleanup_depth >= depth && self.regions[i].open.is_some()).collect();
        for &i in &suspended {
            let s = self.regions[i].open.take().unwrap();
            if here > s {
                self.regions[i].ranges.push((s, here));
            }
        }
        for i in (depth..n).rev() {
            match &self.cleanups[i] {
                Cleanup::Finally(body) => {
                    let body: &'h [Stmt] = body;
                    // run the finally block with the cleanup stack truncated to its level
                    let saved: Vec<Cleanup<'h>> = self.cleanups.drain(i..).collect();
                    self.stmts(body);
                    self.cleanups.extend(saved);
                }
                Cleanup::Drops(ls) => {
                    for l in ls.clone().iter().rev() {
                        self.emit(Op::DropLocal(*l));
                    }
                }
            }
        }
        let here = self.here();
        for i in suspended {
            self.regions[i].open = Some(here);
        }
    }

    // ------------------------------------------------------------------ statements
    fn stmts(&mut self, ss: &'h [Stmt]) {
        for s in ss {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &'h Stmt) {
        match &s.kind {
            StmtKind::Let(id, init) => {
                match init {
                    Some(e) => self.expr(e),
                    None => {
                        let k = self.konst(Value::Void);
                        self.emit(Op::Const(k));
                    }
                }
                self.emit(Op::InitLocal(*id));
            }
            StmtKind::Assign(place, e) => {
                let d = self.needs_drop(&e.ty);
                match place {
                    Place::Local(id) => {
                        self.expr(e);
                        self.emit(Op::StoreLocal(*id, d));
                    }
                    Place::Deref(id) => {
                        self.expr(e);
                        self.emit(Op::StoreDeref(*id, d));
                    }
                    Place::Field(o, idx) => {
                        self.expr(o);
                        self.expr(e);
                        self.emit(Op::SetField(*idx, d));
                    }
                    Place::Global(g) => {
                        self.expr(e);
                        self.emit(Op::StoreGlobal(*g, d));
                    }
                    Place::Elem(..) => {
                        self.expr(e);
                        self.place_ref(place);
                        self.emit(Op::StoreRef(d));
                    }
                }
            }
            StmtKind::Expr(e) => {
                self.expr(e);
                if self.needs_drop(&e.ty) && !matches!(e.kind, ExprKind::Local(_) | ExprKind::Field(..)) {
                    self.emit(Op::DropTop);
                } else {
                    self.emit(Op::Pop);
                }
            }
            StmtKind::Free(e) => {
                self.expr(e);
                self.emit(Op::Free);
            }
            StmtKind::If(c, a, b) => {
                self.expr(c);
                let jf = self.emit(Op::JumpIfFalse(0));
                self.stmts(a);
                if b.is_empty() {
                    let h = self.here();
                    self.patch(jf, h);
                } else {
                    let je = self.emit(Op::Jump(0));
                    let h = self.here();
                    self.patch(jf, h);
                    self.stmts(b);
                    let h = self.here();
                    self.patch(je, h);
                }
            }
            StmtKind::Loop { cond, body, step } => {
                let top = self.here();
                let mut exit_jump = None;
                if let Some(c) = cond {
                    self.expr(c);
                    exit_jump = Some(self.emit(Op::JumpIfFalse(0)));
                }
                self.breakables.push(Breakable { is_loop: true, cleanup_depth: self.cleanups.len(), breaks: Vec::new(), continues: Vec::new() });
                self.stmts(body);
                let b = self.breakables.pop().unwrap();
                let cont = self.here();
                for j in b.continues {
                    self.patch(j, cont);
                }
                self.stmts(step);
                self.emit(Op::Jump(top));
                let exit = self.here();
                if let Some(j) = exit_jump {
                    self.patch(j, exit);
                }
                for j in b.breaks {
                    self.patch(j, exit);
                }
            }
            StmtKind::Break => {
                let idx = self.breakables.len() - 1;
                let depth = self.breakables[idx].cleanup_depth;
                self.emit_cleanups(depth);
                let j = self.emit(Op::Jump(0));
                self.breakables[idx].breaks.push(j);
            }
            StmtKind::Continue => {
                let idx = self.breakables.iter().rposition(|b| b.is_loop).unwrap();
                let depth = self.breakables[idx].cleanup_depth;
                self.emit_cleanups(depth);
                let j = self.emit(Op::Jump(0));
                self.breakables[idx].continues.push(j);
            }
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e);
                    self.emit_cleanups(0);
                    self.emit(Op::Return);
                } else {
                    self.emit_cleanups(0);
                    self.emit(Op::ReturnVoid);
                }
            }
            StmtKind::Throw(e) => {
                self.expr(e);
                self.emit(Op::Throw);
            }
            StmtKind::Block { body, drops } => {
                if drops.is_empty() {
                    self.stmts(body);
                    return;
                }
                let region = self.open_region();
                self.cleanups.push(Cleanup::Drops(drops.clone()));
                self.stmts(body);
                self.cleanups.pop();
                let ranges = self.close_region(region);
                self.regions.pop();
                for l in drops.iter().rev() {
                    self.emit(Op::DropLocal(*l));
                }
                let skip = self.emit(Op::Jump(0));
                // exceptional exit: drop and rethrow
                let target = self.here();
                self.add_handler(ranges, target);
                for l in drops.iter().rev() {
                    self.emit(Op::DropLocal(*l));
                }
                self.emit(Op::Throw);
                let h = self.here();
                self.patch(skip, h);
            }
            StmtKind::Try { body, catches, finally } => self.try_stmt(body, catches, finally.as_deref()),
            StmtKind::Switch { cases } => {
                let mut jumps = Vec::new();
                for (i, c) in cases.iter().enumerate() {
                    if let Some(cond) = &c.cond {
                        self.expr(cond);
                        jumps.push((i, self.emit(Op::JumpIfTrue(0))));
                    }
                }
                let default_jump = self.emit(Op::Jump(0));
                self.breakables.push(Breakable { is_loop: false, cleanup_depth: self.cleanups.len(), breaks: Vec::new(), continues: Vec::new() });
                let mut starts = Vec::new();
                let mut end_jumps = Vec::new();
                for c in cases {
                    starts.push(self.here());
                    self.stmts(&c.body);
                    if !c.fallthrough {
                        end_jumps.push(self.emit(Op::Jump(0)));
                    }
                }
                let b = self.breakables.pop().unwrap();
                let end = self.here();
                for (i, j) in jumps {
                    self.patch(j, starts[i]);
                }
                match cases.iter().position(|c| c.cond.is_none()) {
                    Some(d) => self.patch(default_jump, starts[d]),
                    None => self.patch(default_jump, end),
                }
                for j in end_jumps.into_iter().chain(b.breaks) {
                    self.patch(j, end);
                }
            }
        }
    }

    fn try_stmt(&mut self, body: &'h [Stmt], catches: &'h [Catch], finally: Option<&'h [Stmt]>) {
        // finally cleanup is active for the body and the catch handlers
        let fin_region = finally.map(|fb| {
            let r = self.open_region();
            self.cleanups.push(Cleanup::Finally(fb));
            r
        });
        let body_region = self.open_region();
        self.stmts(body);
        let body_ranges = self.close_region(body_region);
        self.regions.pop();
        // normal completion: run finally (outside the fin region)
        let mut end_jumps = Vec::new();
        if let Some(fb) = finally {
            let depth = self.cleanups.len() - 1;
            self.emit_cleanups(depth);
            let _ = fb;
        }
        end_jumps.push(self.emit(Op::Jump(0)));
        // catch dispatch
        if !catches.is_empty() {
            let target = self.here();
            self.add_handler(body_ranges, target);
            for c in catches {
                self.c.lists.push(c.classes.clone());
                let li = (self.c.lists.len() - 1) as u32;
                self.emit(Op::CatchMatch(li));
                let next = self.emit(Op::JumpIfFalse(0));
                self.emit(Op::StoreLocal(c.local, false));
                self.stmts(&c.body);
                if finally.is_some() {
                    let depth = self.cleanups.len() - 1;
                    self.emit_cleanups(depth);
                }
                end_jumps.push(self.emit(Op::Jump(0)));
                let h = self.here();
                self.patch(next, h);
            }
            // no catch matched: rethrow (through the finally handler, if any)
            self.emit(Op::Throw);
        } else if finally.is_some() {
            // body exceptions go straight to the finally handler
            if let Some(r) = fin_region {
                let _ = r;
            }
            self.regions_push_ranges(fin_region, body_ranges);
        }
        if let (Some(fb), Some(r)) = (finally, fin_region) {
            self.cleanups.pop();
            let ranges = self.close_region(r);
            self.regions.pop();
            let target = self.here();
            self.add_handler(ranges, target);
            // exceptional path: exception on the stack; run finally then rethrow
            let tmp = self.f.locals.len() as u32 + self.c.handlers.len() as u32 + 1_000_000;
            let _ = tmp;
            self.stmts(fb);
            self.emit(Op::Throw);
        }
        let end = self.here();
        for j in end_jumps {
            self.patch(j, end);
        }
    }

    fn regions_push_ranges(&mut self, r: Option<usize>, ranges: Vec<(u32, u32)>) {
        if let Some(r) = r {
            self.regions[r].ranges.extend(ranges);
        }
    }

    // ------------------------------------------------------------------ expressions
    fn lit(&mut self, l: &Lit, ty: &Type) {
        let v = match l {
            Lit::Int(v) => match ty {
                Type::Int(t) => Value::Int(*t, *v),
                Type::Float(f) => Value::Float(*f, *v as f64),
                Type::Big => Value::Big(Rc::new(BigInt::from_i128(*v))),
                _ => Value::Int(IntTy::I32, *v),
            },
            Lit::Float(v) => match ty {
                Type::Float(f) => Value::Float(*f, *v),
                _ => Value::Float(FloatTy::F64, *v),
            },
            Lit::Bool(b) => Value::Bool(*b),
            Lit::Str(s) => Value::str(s.as_str()),
            Lit::Big(s) => Value::Big(Rc::new(BigInt::parse(s).unwrap_or_else(BigInt::zero))),
            Lit::Null => Value::Null,
            Lit::Void => Value::Void,
        };
        let k = self.konst(v);
        self.emit(Op::Const(k));
    }

    fn place_ref(&mut self, p: &'h Place) {
        match p {
            Place::Local(id) => {
                self.emit(Op::LocalRef(*id));
            }
            Place::Deref(id) => {
                self.emit(Op::LoadLocalRaw(*id));
            }
            Place::Field(o, idx) => {
                self.expr(o);
                self.emit(Op::FieldRef(*idx));
            }
            Place::Global(g) => {
                self.emit(Op::GlobalRef(*g));
            }
            Place::Elem(b, k) => {
                self.place_ref(b);
                self.expr(k);
                self.emit(Op::ElemRef);
            }
        }
    }

    fn args(&mut self, xs: &'h [Expr]) -> u32 {
        for x in xs {
            self.expr(x);
        }
        xs.len() as u32
    }

    fn expr(&mut self, e: &'h Expr) {
        match &e.kind {
            ExprKind::Lit(l) => self.lit(l, &e.ty),
            ExprKind::Local(id) => {
                if matches!(self.f.locals[*id as usize].ty, Type::Ref(true, _)) && matches!(e.ty, Type::Ref(true, _)) {
                    self.emit(Op::LoadLocalRaw(*id));
                } else {
                    self.emit(Op::LoadLocal(*id));
                }
            }
            ExprKind::Move(id) => {
                if self.ownership {
                    self.emit(Op::MoveLocal(*id));
                } else {
                    self.emit(Op::LoadLocal(*id));
                }
            }
            ExprKind::Deref(inner) => {
                if let ExprKind::Local(id) = inner.kind {
                    self.emit(Op::LoadLocalRaw(id));
                } else {
                    self.expr(inner);
                }
                self.emit(Op::Deref);
            }
            ExprKind::Global(g) => {
                self.emit(Op::LoadGlobal(*g));
            }
            ExprKind::Field(o, idx) => {
                self.expr(o);
                self.emit(Op::GetField(*idx));
            }
            ExprKind::TupleGet(t, i) => {
                self.expr(t);
                self.emit(Op::TupleGet(*i));
            }
            ExprKind::Unary(op, a) => {
                self.expr(a);
                match op {
                    UnaryOp::Neg => self.emit(Op::Neg(self.f.wrap)),
                    UnaryOp::BitNot => self.emit(Op::BitNot),
                    UnaryOp::Not => self.emit(Op::Not),
                };
            }
            ExprKind::Arith(op, a, b, wrap) => {
                self.expr(a);
                self.expr(b);
                self.emit(Op::Arith(*op, *wrap));
            }
            ExprKind::Concat(parts) => {
                let n = self.args(parts);
                self.emit(Op::Concat(n));
            }
            ExprKind::Cmp(op, a, b) => {
                self.expr(a);
                self.expr(b);
                self.emit(Op::Cmp(*op));
            }
            ExprKind::And(a, b) => {
                self.expr(a);
                self.emit(Op::Dup);
                let j = self.emit(Op::JumpIfFalse(0));
                self.emit(Op::Pop);
                self.expr(b);
                let h = self.here();
                self.patch(j, h);
            }
            ExprKind::Or(a, b) => {
                self.expr(a);
                self.emit(Op::Dup);
                let j = self.emit(Op::JumpIfTrue(0));
                self.emit(Op::Pop);
                self.expr(b);
                let h = self.here();
                self.patch(j, h);
            }
            ExprKind::Ternary(c, a, b) => {
                self.expr(c);
                let jf = self.emit(Op::JumpIfFalse(0));
                self.expr(a);
                let je = self.emit(Op::Jump(0));
                let h = self.here();
                self.patch(jf, h);
                self.expr(b);
                let h = self.here();
                self.patch(je, h);
            }
            ExprKind::Coalesce(a, b) => {
                self.expr(a);
                let j = self.emit(Op::JumpIfNotNull(0));
                self.expr(b);
                let h = self.here();
                self.patch(j, h);
            }
            ExprKind::NonNull(a) => {
                self.expr(a);
                let tuple = matches!(a.ty, Type::Tuple(_));
                self.emit(Op::NonNull(tuple));
            }
            ExprKind::Unwrap(a) => self.expr(a),
            ExprKind::Convert(a) => {
                self.expr(a);
                let t = self.rt(&e.ty);
                self.emit(Op::Convert(t));
            }
            ExprKind::Cast(a, wrap) => {
                self.expr(a);
                let t = self.rt(&e.ty);
                self.emit(Op::Cast(t, *wrap));
            }
            ExprKind::Call(f, args) => {
                let n = self.args(args);
                self.emit(Op::Call(*f, n));
            }
            ExprKind::CallVirtual(sel, args) => {
                let n = self.args(args);
                self.emit(Op::CallVirtual(*sel, n));
            }
            ExprKind::CallClosure(c, args) => {
                self.expr(c);
                let n = self.args(args);
                self.emit(Op::CallClosure(n));
            }
            ExprKind::New(c, ctor, args) => {
                let n = self.args(args);
                self.emit(Op::New(*c, *ctor, n));
            }
            ExprKind::Builtin(b, args) => {
                let n = self.args(args);
                self.emit(Op::Builtin(*b, n));
            }
            ExprKind::BuiltinMut(b, place, args) => match &**place {
                Place::Local(id) if !self.f.locals[*id as usize].cell => {
                    let n = self.args(args);
                    self.emit(Op::BuiltinMutLocal(*b, *id, n));
                }
                p => {
                    self.place_ref(p);
                    let n = self.args(args);
                    self.emit(Op::BuiltinMutRef(*b, n));
                }
            },
            ExprKind::Lambda(f, caps) => {
                for c in caps {
                    match c {
                        CaptureSrc::Ref(p) => self.place_ref(p),
                        CaptureSrc::Value(x) => self.expr(x),
                    }
                }
                self.emit(Op::MakeClosure(*f, caps.len() as u32));
            }
            ExprKind::FuncRef(f) => {
                self.emit(Op::FuncRef(*f));
            }
            ExprKind::Dict(kv) => {
                for (k, v) in kv {
                    self.expr(k);
                    self.expr(v);
                }
                self.emit(Op::MakeDict(kv.len() as u32));
            }
            ExprKind::Tuple(xs) => {
                let n = self.args(xs);
                self.emit(Op::MakeTuple(n));
            }
            ExprKind::RefMut(p) => self.place_ref(p),
            ExprKind::Seq(stmts, x) => {
                self.stmts(stmts);
                self.expr(x);
            }
        }
    }
}

/// Human-readable listing of a compiled module.
pub fn disassemble(p: &Program, m: &Module) -> String {
    let mut s = String::new();
    for (i, c) in m.chunks.iter().enumerate() {
        let _ = writeln!(s, "fn #{} {} (locals={}, params={:?})", i, c.name, c.nlocals, c.params);
        for (pc, op) in c.code.iter().enumerate() {
            let extra = match op {
                Op::Const(k) => format!("    ; {:?}", c.consts[*k as usize]),
                Op::Call(f, _) => format!("    ; {}", p.funcs[*f as usize].name),
                Op::CallVirtual(sel, _) => format!("    ; {}", p.selectors[*sel as usize].name),
                Op::New(cl, _, _) => format!("    ; {}", p.classes[*cl as usize].name),
                _ => String::new(),
            };
            let _ = writeln!(s, "  {:4}  {:?}{}", pc, op, extra);
        }
        for h in &c.handlers {
            let _ = writeln!(s, "  handler [{}, {}) -> {}", h.start, h.end, h.target);
        }
    }
    s
}
