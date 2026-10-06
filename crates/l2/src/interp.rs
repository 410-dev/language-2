//! Tree-walking interpreter over HIR — the reference implementation (spec 15.3).

use crate::hir::*;
use crate::types::*;
use l2_runtime::builtins;
use l2_runtime::ops;
use l2_runtime::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const MAX_DEPTH: usize = 3000;

/// An in-flight exception (a Throwable object).
pub struct Exc(pub Value);

enum Ctrl {
    Throw(Value),
    Break,
    Continue,
    Return(Value),
}

impl From<Exc> for Ctrl {
    fn from(e: Exc) -> Ctrl {
        Ctrl::Throw(e.0)
    }
}

type R<T> = Result<T, Exc>;

struct Frame {
    func: FuncId,
    locals: Vec<Value>,
}

pub struct Interp<'p> {
    p: &'p Program,
    globals: Vec<Rc<RefCell<Value>>>,
    depth: usize,
    ownership: bool,
    rt_cache: std::collections::HashMap<*const Type, RtType>,
}

/// Creates an exception object of a prelude class with the given message.
pub fn make_exception(p: &Program, kind: ExcKind, msg: String) -> Value {
    let c = p.exc_class(kind);
    Value::Object(Rc::new(Object { class: c, fields: RefCell::new(vec![Value::str(msg)]), freed: Cell::new(false) }))
}

/// Default `toString` of an object: `Name(f=v, ...)`, or `Name: message` for exceptions.
pub fn default_object_string<H: Host>(p: &Program, o: &Rc<Object>, h: &mut H) -> Result<String, H::Err> {
    let ci = &p.classes[o.class as usize];
    if ci.is_throwable {
        let msg = match o.fields.borrow().first() {
            Some(Value::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        return Ok(if msg.is_empty() { ci.name.clone() } else { format!("{}: {}", ci.name, msg) });
    }
    let fields: Vec<Value> = o.fields.borrow().clone();
    let mut parts = Vec::new();
    for (fi, v) in ci.fields.iter().zip(fields.iter()) {
        parts.push(format!("{}={}", fi.name, ops::to_display_nested(v, h)?));
    }
    Ok(format!("{}({})", ci.name, parts.join(", ")))
}

impl<'p> Host for Interp<'p> {
    type Err = Exc;
    fn throw(&mut self, kind: ExcKind, msg: String) -> Exc {
        Exc(make_exception(self.p, kind, msg))
    }
    fn obj_to_string(&mut self, o: &Rc<Object>) -> Result<String, Exc> {
        if o.freed.get() {
            return Err(self.throw(ExcKind::UseAfterFree, "use of freed object".into()));
        }
        if let Some(f) = self.p.classes[o.class as usize].to_string_fn {
            let v = self.call(f, vec![Value::Object(o.clone())])?;
            return Ok(v.as_str().to_string());
        }
        let p = self.p;
        default_object_string(p, o, self)
    }
    fn obj_equals(&mut self, a: &Rc<Object>, b: &Rc<Object>) -> Result<bool, Exc> {
        if let Some(f) = self.p.classes[a.class as usize].equals_fn {
            let v = self.call(f, vec![Value::Object(a.clone()), Value::Object(b.clone())])?;
            return Ok(v.as_bool());
        }
        ops::fields_equal(a, b, self)
    }
    fn obj_compare(&mut self, a: &Rc<Object>, b: &Rc<Object>) -> Result<i32, Exc> {
        match self.p.classes[a.class as usize].compare_fn {
            Some(f) => Ok(self.call(f, vec![Value::Object(a.clone()), Value::Object(b.clone())])?.as_int() as i32),
            None => Err(self.throw(ExcKind::ClassCast, format!("{} is not Comparable", self.p.classes[a.class as usize].name))),
        }
    }
    fn is_subclass(&self, cls: u32, of: u32) -> bool {
        self.p.is_subclass(cls, of)
    }
    fn implements(&self, cls: u32, iface: u32) -> bool {
        self.p.implements(cls, iface)
    }
    fn class_name(&self, cls: u32) -> String {
        self.p.classes[cls as usize].name.clone()
    }
}

/// Runs a program. Returns the process exit code.
pub fn run(p: &Program, args: Vec<String>) -> i32 {
    let mut it = Interp { p, globals: p.globals.iter().map(|_| Rc::new(RefCell::new(Value::Void))).collect(), depth: 0, ownership: p.config.memory == MemoryMode::Ownership, rt_cache: Default::default() };
    let code = match it.call(p.init, vec![]) {
        Err(e) => it.uncaught(e),
        Ok(_) => {
            let argv = if p.main_takes_args { vec![Value::array(args.into_iter().map(Value::str).collect(), false)] } else { vec![] };
            match it.call(p.main, argv) {
                Ok(Value::Int(_, c)) => c as i32,
                Ok(_) => 0,
                Err(e) => it.uncaught(e),
            }
        }
    };
    builtins::flush_out();
    code
}

impl<'p> Interp<'p> {
    fn uncaught(&mut self, e: Exc) -> i32 {
        builtins::flush_out();
        let msg = match &e.0 {
            Value::Object(o) => match self.obj_to_string(o) {
                Ok(s) => s,
                Err(_) => self.p.classes[o.class as usize].name.clone(),
            },
            other => format!("{:?}", other),
        };
        eprintln!("Exception in thread \"main\" {}", msg);
        1
    }

    fn rt(&mut self, t: &Type) -> RtType {
        let key = t as *const Type;
        if let Some(r) = self.rt_cache.get(&key) {
            return r.clone();
        }
        let r = rt_type(t);
        self.rt_cache.insert(key, r.clone());
        r
    }

    pub fn call(&mut self, fid: FuncId, args: Vec<Value>) -> R<Value> {
        self.call_with(fid, args, None)
    }

    fn call_with(&mut self, fid: FuncId, args: Vec<Value>, captures: Option<&[Value]>) -> R<Value> {
        if self.depth >= MAX_DEPTH {
            return Err(self.throw(ExcKind::StackOverflow, "stack depth limit exceeded".into()));
        }
        let f = &self.p.funcs[fid as usize];
        let mut fr = Frame { func: fid, locals: vec![Value::Void; f.locals.len()] };
        for (i, (pid, v)) in f.params.iter().zip(args.into_iter()).enumerate() {
            let _ = i;
            if f.locals[*pid as usize].cell {
                fr.locals[*pid as usize] = Value::Ref(RefTarget::Cell(Rc::new(RefCell::new(v))));
            } else {
                fr.locals[*pid as usize] = v;
            }
        }
        if let Some(caps) = captures {
            for (c, v) in f.captures.iter().zip(caps.iter()) {
                if c.by_ref || !f.locals[c.inner as usize].cell {
                    fr.locals[c.inner as usize] = v.clone();
                } else {
                    fr.locals[c.inner as usize] = Value::Ref(RefTarget::Cell(Rc::new(RefCell::new(v.clone()))));
                }
            }
        }
        self.depth += 1;
        let r = self.exec_block(&mut fr, &f.body);
        self.depth -= 1;
        match r {
            Ok(()) => Ok(Value::Void),
            Err(Ctrl::Return(v)) => Ok(v),
            Err(Ctrl::Throw(e)) => Err(Exc(e)),
            Err(_) => Ok(Value::Void),
        }
    }

    // ------------------------------------------------------------------ locals
    fn is_cell(&self, fr: &Frame, id: LocalId) -> bool {
        self.p.funcs[fr.func as usize].locals[id as usize].cell
    }

    fn get_local(&self, fr: &Frame, id: LocalId) -> Value {
        let v = &fr.locals[id as usize];
        if self.is_cell(fr, id) {
            if let Value::Ref(r) = v {
                return r.get();
            }
        }
        v.clone()
    }

    fn set_local(&self, fr: &mut Frame, id: LocalId, v: Value) {
        if self.is_cell(fr, id) {
            match &fr.locals[id as usize] {
                Value::Ref(r) => {
                    r.set(v);
                    return;
                }
                _ => {
                    fr.locals[id as usize] = Value::Ref(RefTarget::Cell(Rc::new(RefCell::new(v))));
                    return;
                }
            }
        }
        fr.locals[id as usize] = v;
    }

    fn take_local(&self, fr: &mut Frame, id: LocalId) -> Value {
        if self.is_cell(fr, id) {
            if let Value::Ref(r) = &fr.locals[id as usize] {
                let v = r.get();
                r.set(Value::Void);
                return v;
            }
        }
        std::mem::replace(&mut fr.locals[id as usize], Value::Void)
    }

    fn local_cell(&self, fr: &mut Frame, id: LocalId) -> Value {
        match &fr.locals[id as usize] {
            Value::Ref(r) if self.is_cell(fr, id) => Value::Ref(r.clone()),
            _ => {
                let v = std::mem::replace(&mut fr.locals[id as usize], Value::Void);
                let r = RefTarget::Cell(Rc::new(RefCell::new(v)));
                fr.locals[id as usize] = Value::Ref(r.clone());
                Value::Ref(r)
            }
        }
    }

    fn object(&mut self, v: &Value) -> R<Rc<Object>> {
        match v {
            Value::Object(o) => {
                if o.freed.get() {
                    return Err(self.throw(ExcKind::UseAfterFree, "use of freed object".into()));
                }
                Ok(o.clone())
            }
            Value::Ref(r) => {
                let v = r.get();
                self.object(&v)
            }
            Value::Null => Err(self.throw(ExcKind::NullPointer, "member access on null".into())),
            other => Err(self.throw(ExcKind::ClassCast, format!("expected an object, got {}", other.type_name()))),
        }
    }

    // ------------------------------------------------------------------ places
    fn place_ref(&mut self, fr: &mut Frame, p: &Place) -> R<Value> {
        Ok(match p {
            Place::Local(id) => self.local_cell(fr, *id),
            Place::Deref(id) => fr.locals[*id as usize].clone(),
            Place::Field(o, idx) => {
                let ov = self.eval(fr, o)?;
                let obj = self.object(&ov)?;
                Value::Ref(RefTarget::Field(obj, *idx as usize))
            }
            Place::Global(g) => Value::Ref(RefTarget::Cell(self.globals[*g as usize].clone())),
            Place::Elem(b, k) => {
                let parent = match self.place_ref(fr, b)? {
                    Value::Ref(t) => t,
                    other => RefTarget::Cell(Rc::new(RefCell::new(other))),
                };
                let key = self.eval(fr, k)?;
                Value::Ref(builtins::elem_ref(parent, key, self)?)
            }
        })
    }

    fn with_place<T>(&mut self, fr: &mut Frame, p: &Place, f: impl FnOnce(&mut Self, &mut Value) -> R<T>) -> R<T> {
        match p {
            Place::Local(id) if !self.is_cell(fr, *id) => {
                let mut v = std::mem::replace(&mut fr.locals[*id as usize], Value::Void);
                let r = f(self, &mut v);
                fr.locals[*id as usize] = v;
                r
            }
            _ => {
                let target = match self.place_ref(fr, p)? {
                    Value::Ref(r) => r,
                    _ => unreachable!(),
                };
                let target = if let Place::Deref(_) = p {
                    target
                } else {
                    target
                };
                let mut v = target.get();
                target.set(Value::Void);
                let r = f(self, &mut v);
                target.set(v);
                r
            }
        }
    }

    fn assign(&mut self, fr: &mut Frame, p: &Place, v: Value, drop_old: bool) -> R<()> {
        if drop_old {
            let old = match p {
                Place::Local(id) => self.take_local(fr, *id),
                _ => self.with_place(fr, p, |_, slot| Ok(std::mem::replace(slot, Value::Void)))?,
            };
            self.drop_value(old)?;
        }
        match p {
            Place::Local(id) => {
                self.set_local(fr, *id, v);
                Ok(())
            }
            Place::Deref(id) => {
                if let Value::Ref(r) = &fr.locals[*id as usize] {
                    let r = r.clone();
                    r.set(v);
                }
                Ok(())
            }
            Place::Field(o, idx) => {
                let ov = self.eval(fr, o)?;
                let obj = self.object(&ov)?;
                obj.fields.borrow_mut()[*idx as usize] = v;
                Ok(())
            }
            Place::Global(g) => {
                *self.globals[*g as usize].borrow_mut() = v;
                Ok(())
            }
            Place::Elem(..) => {
                if let Value::Ref(r) = self.place_ref(fr, p)? {
                    r.set(v);
                }
                Ok(())
            }
        }
    }

    // ------------------------------------------------------------------ drop
    pub fn drop_value(&mut self, v: Value) -> R<()> {
        match v {
            Value::Object(o) => {
                if o.freed.get() {
                    return Ok(());
                }
                if let Some(df) = self.p.classes[o.class as usize].drop_fn {
                    self.call(df, vec![Value::Object(o.clone())])?;
                }
                let fields: Vec<Value> = std::mem::take(&mut *o.fields.borrow_mut());
                let n = fields.len();
                let mut kept = fields.clone();
                for f in fields.into_iter().rev() {
                    self.drop_value(f)?;
                }
                kept.truncate(n);
                *o.fields.borrow_mut() = kept;
                Ok(())
            }
            Value::Array(a) => {
                for it in a.items.iter() {
                    self.drop_value(it.clone())?;
                }
                Ok(())
            }
            Value::Dict(d) => {
                for (_, it) in d.entries.iter() {
                    self.drop_value(it.clone())?;
                }
                Ok(())
            }
            Value::Tuple(t) => {
                for it in t.iter() {
                    self.drop_value(it.clone())?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// `free(x)` (manual mode): calls `drop()` and recursively frees owned data.
    fn free(&mut self, v: Value) -> R<()> {
        if let Value::Object(o) = &v {
            if o.freed.get() {
                return Err(self.throw(ExcKind::UseAfterFree, "double free of an object".into()));
            }
        }
        self.free_rec(v)
    }

    fn free_rec(&mut self, v: Value) -> R<()> {
        match &v {
            Value::Object(o) => {
                if o.freed.get() {
                    return Ok(());
                }
                if let Some(df) = self.p.classes[o.class as usize].drop_fn {
                    self.call(df, vec![v.clone()])?;
                }
                o.freed.set(true);
                let fields: Vec<Value> = o.fields.borrow().clone();
                for f in fields.into_iter().rev() {
                    self.free_rec(f)?;
                }
            }
            Value::Array(a) => {
                for it in a.items.iter() {
                    self.free_rec(it.clone())?;
                }
            }
            Value::Dict(d) => {
                for (_, it) in d.entries.iter() {
                    self.free_rec(it.clone())?;
                }
            }
            Value::Tuple(t) => {
                for it in t.iter() {
                    self.free_rec(it.clone())?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ------------------------------------------------------------------ statements
    fn exec_block(&mut self, fr: &mut Frame, stmts: &[Stmt]) -> Result<(), Ctrl> {
        for s in stmts {
            self.exec(fr, s)?;
        }
        Ok(())
    }

    fn exec(&mut self, fr: &mut Frame, s: &Stmt) -> Result<(), Ctrl> {
        match &s.kind {
            StmtKind::Let(id, init) => {
                let v = match init {
                    Some(e) => self.eval(fr, e)?,
                    None => Value::Void,
                };
                if self.is_cell(fr, *id) {
                    fr.locals[*id as usize] = Value::Ref(RefTarget::Cell(Rc::new(RefCell::new(v))));
                } else {
                    fr.locals[*id as usize] = v;
                }
                Ok(())
            }
            StmtKind::Assign(p, e) => {
                let v = self.eval(fr, e)?;
                let drop_old = self.ownership && self.p.needs_drop(&e.ty);
                self.assign(fr, p, v, drop_old)?;
                Ok(())
            }
            StmtKind::Expr(e) => {
                let v = self.eval(fr, e)?;
                if self.ownership && self.p.any_droppable && !matches!(e.kind, ExprKind::Local(_) | ExprKind::Field(..)) && self.p.needs_drop(&e.ty) {
                    self.drop_value(v)?;
                }
                Ok(())
            }
            StmtKind::If(c, a, b) => {
                if self.eval(fr, c)?.as_bool() {
                    self.exec_block(fr, a)
                } else {
                    self.exec_block(fr, b)
                }
            }
            StmtKind::Loop { cond, body, step } => {
                loop {
                    if let Some(c) = cond {
                        if !self.eval(fr, c)?.as_bool() {
                            break;
                        }
                    }
                    match self.exec_block(fr, body) {
                        Ok(()) | Err(Ctrl::Continue) => {}
                        Err(Ctrl::Break) => break,
                        Err(e) => return Err(e),
                    }
                    self.exec_block(fr, step)?;
                }
                Ok(())
            }
            StmtKind::Break => Err(Ctrl::Break),
            StmtKind::Continue => Err(Ctrl::Continue),
            StmtKind::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(fr, e)?,
                    None => Value::Void,
                };
                Err(Ctrl::Return(v))
            }
            StmtKind::Throw(e) => {
                let v = self.eval(fr, e)?;
                if v.is_null() {
                    return Err(Ctrl::Throw(make_exception(self.p, ExcKind::NullPointer, "throw null".into())));
                }
                Err(Ctrl::Throw(v))
            }
            StmtKind::Try { body, catches, finally } => {
                let mut r = self.exec_block(fr, body);
                if let Err(Ctrl::Throw(exc)) = &r {
                    let cls = match exc {
                        Value::Object(o) => o.class,
                        _ => u32::MAX,
                    };
                    for c in catches {
                        if c.classes.iter().any(|&k| cls != u32::MAX && self.p.is_subclass(cls, k)) {
                            let exc = exc.clone();
                            self.set_local(fr, c.local, exc);
                            r = self.exec_block(fr, &c.body);
                            break;
                        }
                    }
                }
                if let Some(fb) = finally {
                    self.exec_block(fr, fb)?;
                }
                r
            }
            StmtKind::Block { body, drops } => {
                let r = self.exec_block(fr, body);
                if drops.is_empty() {
                    return r;
                }
                let mut dr: Result<(), Ctrl> = Ok(());
                for id in drops.iter().rev() {
                    let v = self.take_local(fr, *id);
                    if !v.is_void() {
                        if let Err(e) = self.drop_value(v) {
                            if dr.is_ok() {
                                dr = Err(e.into());
                            }
                        }
                    }
                }
                r.and(dr)
            }
            StmtKind::Switch { cases } => {
                let mut start = None;
                for (i, c) in cases.iter().enumerate() {
                    if let Some(cond) = &c.cond {
                        if self.eval(fr, cond)?.as_bool() {
                            start = Some(i);
                            break;
                        }
                    }
                }
                if start.is_none() {
                    start = cases.iter().position(|c| c.cond.is_none());
                }
                if let Some(mut i) = start {
                    loop {
                        match self.exec_block(fr, &cases[i].body) {
                            Ok(()) => {}
                            Err(Ctrl::Break) => break,
                            Err(e) => return Err(e),
                        }
                        if cases[i].fallthrough && i + 1 < cases.len() {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                }
                Ok(())
            }
            StmtKind::Free(e) => {
                let v = self.eval(fr, e)?;
                self.free(v)?;
                Ok(())
            }
        }
    }

    // ------------------------------------------------------------------ expressions
    fn eval_args(&mut self, fr: &mut Frame, xs: &[Expr]) -> R<Vec<Value>> {
        let mut out = Vec::with_capacity(xs.len());
        for x in xs {
            out.push(self.eval(fr, x)?);
        }
        Ok(out)
    }

    fn dispatch(&mut self, sel: SelectorId, recv: &Value) -> R<FuncId> {
        let o = self.object(recv)?;
        match self.p.classes[o.class as usize].vtable.get(&sel) {
            Some(f) => Ok(*f),
            None => {
                let name = self.p.selectors[sel as usize].name.clone();
                Err(self.throw(ExcKind::UnsupportedOperation, format!("method {} not implemented", name)))
            }
        }
    }

    fn eval(&mut self, fr: &mut Frame, e: &Expr) -> R<Value> {
        Ok(match &e.kind {
            ExprKind::Lit(l) => match l {
                Lit::Int(v) => match &e.ty {
                    Type::Int(t) => Value::Int(*t, *v),
                    Type::Float(f) => Value::Float(*f, *v as f64),
                    Type::Big => Value::Big(Rc::new(BigInt::from_i128(*v))),
                    _ => Value::Int(IntTy::I32, *v),
                },
                Lit::Float(v) => match &e.ty {
                    Type::Float(f) => Value::Float(*f, *v),
                    _ => Value::Float(FloatTy::F64, *v),
                },
                Lit::Bool(b) => Value::Bool(*b),
                Lit::Str(s) => Value::str(s.as_str()),
                Lit::Big(s) => Value::Big(Rc::new(BigInt::parse(s).unwrap_or_else(BigInt::zero))),
                Lit::Null => Value::Null,
                Lit::Void => Value::Void,
            },
            ExprKind::Local(id) => self.get_local(fr, *id),
            ExprKind::Move(id) => {
                if self.ownership {
                    self.take_local(fr, *id)
                } else {
                    self.get_local(fr, *id)
                }
            }
            ExprKind::Deref(inner) => self.eval(fr, inner)?.deref(),
            ExprKind::Global(g) => self.globals[*g as usize].borrow().clone(),
            ExprKind::Field(o, idx) => {
                let ov = self.eval(fr, o)?;
                let obj = self.object(&ov)?;
                let v = obj.fields.borrow()[*idx as usize].clone();
                v
            }
            ExprKind::TupleGet(t, i) => match self.eval(fr, t)?.deref() {
                Value::Tuple(items) => items[*i as usize].clone(),
                Value::Null => return Err(self.throw(ExcKind::NullPointer, "tuple is null".into())),
                other => other,
            },
            ExprKind::Unary(op, a) => {
                let v = self.eval(fr, a)?;
                match op {
                    UnaryOp::Neg => {
                        let wrap = self.p.funcs[fr.func as usize].wrap;
                        ops::negate(&v, wrap, self)?
                    }
                    UnaryOp::BitNot => ops::bit_not(&v),
                    UnaryOp::Not => Value::Bool(!v.as_bool()),
                }
            }
            ExprKind::Arith(op, a, b, wrap) => {
                let x = self.eval(fr, a)?;
                let y = self.eval(fr, b)?;
                ops::arith(*op, &x, &y, *wrap, self)?
            }
            ExprKind::Concat(parts) => {
                let mut s = String::new();
                for p in parts {
                    let v = self.eval(fr, p)?;
                    s.push_str(&ops::to_display(&v, self)?);
                }
                Value::str(s)
            }
            ExprKind::Cmp(op, a, b) => {
                let x = self.eval(fr, a)?;
                let y = self.eval(fr, b)?;
                Value::Bool(ops::compare(*op, &x, &y, self)?)
            }
            ExprKind::And(a, b) => Value::Bool(self.eval(fr, a)?.as_bool() && self.eval(fr, b)?.as_bool()),
            ExprKind::Or(a, b) => Value::Bool(self.eval(fr, a)?.as_bool() || self.eval(fr, b)?.as_bool()),
            ExprKind::Ternary(c, a, b) => {
                if self.eval(fr, c)?.as_bool() {
                    self.eval(fr, a)?
                } else {
                    self.eval(fr, b)?
                }
            }
            ExprKind::Coalesce(a, b) => {
                let x = self.eval(fr, a)?;
                if x.is_null() {
                    self.eval(fr, b)?
                } else {
                    x
                }
            }
            ExprKind::NonNull(a) => {
                let x = self.eval(fr, a)?;
                if x.is_null() {
                    return Err(self.throw(ExcKind::NullPointer, "non-null assertion failed: value is null".into()));
                }
                if let (Value::Tuple(items), Type::Tuple(_)) = (&x, &a.ty) {
                    if items.iter().any(|v| v.is_null()) {
                        return Err(self.throw(ExcKind::NullPointer, "non-null assertion failed: value is null".into()));
                    }
                }
                x
            }
            ExprKind::Unwrap(a) => self.eval(fr, a)?,
            ExprKind::Convert(a) => {
                let v = self.eval(fr, a)?;
                let t = self.rt(&e.ty);
                ops::widen(v, &t)
            }
            ExprKind::Cast(a, wrap) => {
                let v = self.eval(fr, a)?;
                let t = self.rt(&e.ty);
                ops::cast(&v, &t, *wrap, self)?
            }
            ExprKind::Call(f, args) => {
                let a = self.eval_args(fr, args)?;
                self.call(*f, a)?
            }
            ExprKind::CallVirtual(sel, args) => {
                let a = self.eval_args(fr, args)?;
                let f = self.dispatch(*sel, &a[0])?;
                self.call(f, a)?
            }
            ExprKind::CallClosure(c, args) => {
                let cv = self.eval(fr, c)?;
                let a = self.eval_args(fr, args)?;
                match cv.deref() {
                    Value::Closure(cl) => {
                        let caps = cl.captures.clone();
                        self.call_with(cl.func as FuncId, a, Some(&caps))?
                    }
                    Value::Null => return Err(self.throw(ExcKind::NullPointer, "call of a null function".into())),
                    other => return Err(self.throw(ExcKind::ClassCast, format!("{} is not callable", other.type_name()))),
                }
            }
            ExprKind::New(c, ctor, args) => {
                let ci = &self.p.classes[*c as usize];
                let fields: Vec<Value> = ci.fields.iter().map(|f| if f.ty.is_nullable() { Value::Null } else { Value::Void }).collect();
                let obj = Value::Object(Rc::new(Object { class: *c, fields: RefCell::new(fields), freed: Cell::new(false) }));
                let mut a = vec![obj.clone()];
                a.extend(self.eval_args(fr, args)?);
                self.call(*ctor, a)?;
                obj
            }
            ExprKind::Builtin(b, args) => {
                let a = self.eval_args(fr, args)?;
                builtins::call(*b, a, self)?
            }
            ExprKind::BuiltinMut(b, place, args) => {
                let a = self.eval_args(fr, args)?;
                let b = *b;
                self.with_place(fr, place, |s, slot| builtins::call_mut(b, slot, a, s))?
            }
            ExprKind::Lambda(f, caps) => {
                let mut cv = Vec::with_capacity(caps.len());
                for c in caps {
                    cv.push(match c {
                        CaptureSrc::Ref(p) => self.place_ref(fr, p)?,
                        CaptureSrc::Value(x) => self.eval(fr, x)?,
                    });
                }
                Value::Closure(Rc::new(Closure { func: *f as usize, captures: cv }))
            }
            ExprKind::FuncRef(f) => Value::Closure(Rc::new(Closure { func: *f as usize, captures: vec![] })),
            ExprKind::Dict(kv) => {
                let mut d = DictVal::default();
                for (k, v) in kv {
                    let kx = self.eval(fr, k)?;
                    let vx = self.eval(fr, v)?;
                    d.insert(kx, vx);
                }
                Value::Dict(Rc::new(d))
            }
            ExprKind::Tuple(xs) => Value::Tuple(Rc::new(self.eval_args(fr, xs)?)),
            ExprKind::RefMut(p) => self.place_ref(fr, p)?,
            ExprKind::Seq(stmts, x) => {
                for s in stmts {
                    match self.exec(fr, s) {
                        Ok(()) => {}
                        Err(Ctrl::Throw(v)) => return Err(Exc(v)),
                        Err(_) => {}
                    }
                }
                self.eval(fr, x)?
            }
        })
    }
}
