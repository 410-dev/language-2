//! Bytecode virtual machine.

use crate::bytecode::{Chunk, Module, Op};
use crate::hir::*;
use crate::interp::{default_object_string, make_exception, Exc, MAX_DEPTH};
use l2_runtime::builtins;
use l2_runtime::ops;
use l2_runtime::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct Vm<'p> {
    p: &'p Program,
    m: &'p Module,
    globals: Vec<Rc<RefCell<Value>>>,
    depth: usize,
}

impl<'p> Host for Vm<'p> {
    type Err = Exc;
    fn throw(&mut self, kind: ExcKind, msg: String) -> Exc {
        Exc(make_exception(self.p, kind, msg))
    }
    fn obj_to_string(&mut self, o: &Rc<Object>) -> Result<String, Exc> {
        if o.freed.get() {
            return Err(self.throw(ExcKind::UseAfterFree, "use of freed object".into()));
        }
        if let Some(f) = self.p.classes[o.class as usize].to_string_fn {
            let v = self.call(f, vec![Value::Object(o.clone())], None)?;
            return Ok(v.as_str().to_string());
        }
        let p = self.p;
        default_object_string(p, o, self)
    }
    fn obj_default_string(&mut self, o: &Rc<Object>) -> Result<String, Exc> {
        let p = self.p;
        default_object_string(p, o, self)
    }
    fn obj_equals(&mut self, a: &Rc<Object>, b: &Rc<Object>) -> Result<bool, Exc> {
        if let Some(f) = self.p.classes[a.class as usize].equals_fn {
            return Ok(self.call(f, vec![Value::Object(a.clone()), Value::Object(b.clone())], None)?.as_bool());
        }
        ops::fields_equal(a, b, self)
    }
    fn obj_compare(&mut self, a: &Rc<Object>, b: &Rc<Object>) -> Result<i32, Exc> {
        match self.p.classes[a.class as usize].compare_fn {
            Some(f) => Ok(self.call(f, vec![Value::Object(a.clone()), Value::Object(b.clone())], None)?.as_int() as i32),
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

/// Compiles the program to bytecode and runs it. Returns the exit code.
pub fn run(p: &Program, args: Vec<String>) -> i32 {
    let m = crate::bytecode::compile(p);
    run_module(p, &m, args)
}

pub fn run_module(p: &Program, m: &Module, args: Vec<String>) -> i32 {
    let mut vm = Vm { p, m, globals: p.globals.iter().map(|_| Rc::new(RefCell::new(Value::Void))).collect(), depth: 0 };
    let code = match vm.call(p.init, vec![], None) {
        Err(e) => vm.uncaught(e),
        Ok(_) => {
            let argv = if p.main_takes_args { vec![Value::array(args.into_iter().map(Value::str).collect(), false)] } else { vec![] };
            match vm.call(p.main, argv, None) {
                Ok(Value::Int(_, c)) => c as i32,
                Ok(_) => 0,
                Err(e) => vm.uncaught(e),
            }
        }
    };
    builtins::flush_out();
    code
}

fn cell(v: Value) -> Value {
    Value::Ref(RefTarget::Cell(Rc::new(RefCell::new(v))))
}

impl<'p> Vm<'p> {
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

    fn object(&mut self, v: &Value) -> Result<Rc<Object>, Exc> {
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

    fn drop_value(&mut self, v: Value) -> Result<(), Exc> {
        match v {
            Value::Object(o) => {
                if o.freed.get() {
                    return Ok(());
                }
                if let Some(df) = self.p.classes[o.class as usize].drop_fn {
                    self.call(df, vec![Value::Object(o.clone())], None)?;
                }
                let fields: Vec<Value> = o.fields.borrow().clone();
                for f in fields.into_iter().rev() {
                    self.drop_value(f)?;
                }
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

    fn free(&mut self, v: Value) -> Result<(), Exc> {
        if let Value::Object(o) = &v {
            if o.freed.get() {
                return Err(self.throw(ExcKind::UseAfterFree, "double free of an object".into()));
            }
        }
        self.free_rec(v)
    }

    fn free_rec(&mut self, v: Value) -> Result<(), Exc> {
        match &v {
            Value::Object(o) => {
                if o.freed.get() {
                    return Ok(());
                }
                if let Some(df) = self.p.classes[o.class as usize].drop_fn {
                    self.call(df, vec![v.clone()], None)?;
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

    pub fn call(&mut self, fid: u32, args: Vec<Value>, caps: Option<&[Value]>) -> Result<Value, Exc> {
        self.invoke(fid, None, args.into_iter(), caps)
    }

    /// Calls a function, moving the arguments directly into the callee's locals.
    fn invoke(&mut self, fid: u32, first: Option<Value>, args: impl Iterator<Item = Value>, caps: Option<&[Value]>) -> Result<Value, Exc> {
        if self.depth >= MAX_DEPTH {
            return Err(self.throw(ExcKind::StackOverflow, "stack depth limit exceeded".into()));
        }
        let m = self.m;
        let c = &m.chunks[fid as usize];
        let mut locals = vec![Value::Void; c.nlocals as usize];
        for (pid, v) in c.params.iter().zip(first.into_iter().chain(args)) {
            locals[*pid as usize] = if c.cells[*pid as usize] { cell(v) } else { v };
        }
        if let Some(caps) = caps {
            for ((inner, by_ref), v) in c.captures.iter().zip(caps.iter()) {
                locals[*inner as usize] = if *by_ref || !c.cells[*inner as usize] { v.clone() } else { cell(v.clone()) };
            }
        }
        self.depth += 1;
        let r = self.run(c, &mut locals);
        self.depth -= 1;
        r
    }

    fn handler_for(c: &Chunk, pc: usize) -> Option<(u32, u32)> {
        let pc = pc as u32;
        c.handlers.iter().find(|h| pc >= h.start && pc < h.end).map(|h| (h.target, h.base))
    }

    fn run(&mut self, c: &'p Chunk, locals: &mut [Value]) -> Result<Value, Exc> {
        let mut stack: Vec<Value> = Vec::with_capacity(16);
        let mut pc = 0usize;
        macro_rules! tri {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(exc) => {
                        match Self::handler_for(c, pc - 1) {
                            Some((target, base)) => {
                                stack.truncate(base as usize);
                                stack.push(exc.0);
                                pc = target as usize;
                                continue;
                            }
                            None => return Err(exc),
                        }
                    }
                }
            };
        }
        macro_rules! pop {
            () => {
                stack.pop().expect("operand stack underflow")
            };
        }
        loop {
            let op = c.code[pc];
            pc += 1;
            match op {
                Op::Const(k) => stack.push(c.consts[k as usize].clone()),
                Op::Pop => {
                    stack.pop();
                }
                Op::Dup => {
                    let v = stack.last().unwrap().clone();
                    stack.push(v);
                }
                Op::LoadLocal(id) => {
                    let v = &locals[id as usize];
                    let v = if c.cells[id as usize] {
                        match v {
                            Value::Ref(r) => r.get(),
                            v => v.clone(),
                        }
                    } else {
                        v.clone()
                    };
                    stack.push(v);
                }
                Op::LoadLocalRaw(id) => stack.push(locals[id as usize].clone()),
                Op::StoreLocal(id, drop_old) => {
                    let v = pop!();
                    if drop_old {
                        let old = take_local(c, locals, id);
                        tri!(self.drop_value(old));
                    }
                    set_local(c, locals, id, v);
                }
                Op::InitLocal(id) => {
                    let v = pop!();
                    locals[id as usize] = if c.cells[id as usize] { cell(v) } else { v };
                }
                Op::MoveLocal(id) => {
                    let v = take_local(c, locals, id);
                    stack.push(v);
                }
                Op::LocalRef(id) => {
                    let i = id as usize;
                    if !matches!(locals[i], Value::Ref(_)) || !c.cells[i] {
                        let v = std::mem::replace(&mut locals[i], Value::Void);
                        locals[i] = cell(v);
                    }
                    stack.push(locals[i].clone());
                }
                Op::LoadGlobal(g) => stack.push(self.globals[g as usize].borrow().clone()),
                Op::StoreGlobal(g, drop_old) => {
                    let v = pop!();
                    if drop_old {
                        let old = std::mem::replace(&mut *self.globals[g as usize].borrow_mut(), Value::Void);
                        tri!(self.drop_value(old));
                    }
                    *self.globals[g as usize].borrow_mut() = v;
                }
                Op::GlobalRef(g) => stack.push(Value::Ref(RefTarget::Cell(self.globals[g as usize].clone()))),
                Op::GetField(idx) => {
                    let o = pop!();
                    let obj = tri!(self.object(&o));
                    let v = obj.fields.borrow()[idx as usize].clone();
                    stack.push(v);
                }
                Op::SetField(idx, drop_old) => {
                    let v = pop!();
                    let o = pop!();
                    let obj = tri!(self.object(&o));
                    if drop_old {
                        let old = std::mem::replace(&mut obj.fields.borrow_mut()[idx as usize], Value::Void);
                        tri!(self.drop_value(old));
                    }
                    obj.fields.borrow_mut()[idx as usize] = v;
                }
                Op::FieldRef(idx) => {
                    let o = pop!();
                    let obj = tri!(self.object(&o));
                    stack.push(Value::Ref(RefTarget::Field(obj, idx as usize)));
                }
                Op::ElemRef => {
                    let key = pop!();
                    let parent = match pop!() {
                        Value::Ref(t) => t,
                        other => RefTarget::Cell(Rc::new(RefCell::new(other))),
                    };
                    let r = tri!(builtins::elem_ref(parent, key, self));
                    stack.push(Value::Ref(r));
                }
                Op::StoreRef(drop_old) => {
                    let r = pop!();
                    let v = pop!();
                    if let Value::Ref(t) = r {
                        if drop_old {
                            let old = t.get();
                            t.set(Value::Void);
                            tri!(self.drop_value(old));
                        }
                        t.set(v);
                    }
                }
                Op::Deref => {
                    let v = pop!();
                    stack.push(v.deref());
                }
                Op::StoreDeref(id, drop_old) => {
                    let v = pop!();
                    if let Value::Ref(r) = &locals[id as usize] {
                        let r = r.clone();
                        if drop_old {
                            let old = r.get();
                            r.set(Value::Void);
                            tri!(self.drop_value(old));
                        }
                        r.set(v);
                    }
                }
                Op::TupleGet(i) => {
                    let t = pop!().deref();
                    match t {
                        Value::Tuple(items) => stack.push(items[i as usize].clone()),
                        Value::Null => {
                            tri!(Err(self.throw(ExcKind::NullPointer, "tuple is null".into())));
                        }
                        other => stack.push(other),
                    }
                }
                Op::MakeTuple(n) => {
                    let items = stack.split_off(stack.len() - n as usize);
                    stack.push(Value::Tuple(Rc::new(items)));
                }
                Op::MakeArray(n) => {
                    let items = stack.split_off(stack.len() - n as usize);
                    stack.push(Value::array(items, false));
                }
                Op::MakeDict(n) => {
                    let items = stack.split_off(stack.len() - 2 * n as usize);
                    let mut d = DictVal::default();
                    let mut it = items.into_iter();
                    while let (Some(k), Some(v)) = (it.next(), it.next()) {
                        d.insert(k, v);
                    }
                    stack.push(Value::Dict(Rc::new(d)));
                }
                Op::Arith(aop, wrap) => {
                    let b = pop!();
                    let a = pop!();
                    let r = tri!(ops::arith(aop, &a, &b, wrap, self));
                    stack.push(r);
                }
                Op::Neg(wrap) => {
                    let a = pop!();
                    let r = tri!(ops::negate(&a, wrap, self));
                    stack.push(r);
                }
                Op::BitNot => {
                    let a = pop!();
                    stack.push(ops::bit_not(&a));
                }
                Op::Not => {
                    let a = pop!();
                    stack.push(Value::Bool(!a.as_bool()));
                }
                Op::Cmp(cop) => {
                    let b = pop!();
                    let a = pop!();
                    let r = tri!(ops::compare(cop, &a, &b, self));
                    stack.push(Value::Bool(r));
                }
                Op::Concat(n) => {
                    let parts = stack.split_off(stack.len() - n as usize);
                    let mut s = String::new();
                    let mut err = None;
                    for p in &parts {
                        match ops::to_display(p, self) {
                            Ok(x) => s.push_str(&x),
                            Err(e) => {
                                err = Some(e);
                                break;
                            }
                        }
                    }
                    if let Some(e) = err {
                        tri!(Err(e));
                    }
                    stack.push(Value::str(s));
                }
                Op::Jump(t) => pc = t as usize,
                Op::JumpIfFalse(t) => {
                    if !pop!().as_bool() {
                        pc = t as usize;
                    }
                }
                Op::JumpIfTrue(t) => {
                    if pop!().as_bool() {
                        pc = t as usize;
                    }
                }
                Op::JumpIfNotNull(t) => {
                    if stack.last().map(|v| v.is_null()).unwrap_or(true) {
                        stack.pop();
                    } else {
                        pc = t as usize;
                    }
                }
                Op::NonNull(tuple) => {
                    let v = stack.last().unwrap();
                    let bad = v.is_null() || (tuple && matches!(v, Value::Tuple(items) if items.iter().any(|x| x.is_null())));
                    if bad {
                        tri!(Err(self.throw(ExcKind::NullPointer, "non-null assertion failed: value is null".into())));
                    }
                }
                Op::Convert(t) => {
                    let v = pop!();
                    stack.push(ops::widen(v, &c.types[t as usize]));
                }
                Op::Cast(t, wrap) => {
                    let v = pop!();
                    let r = tri!(ops::cast(&v, &c.types[t as usize], wrap, self));
                    stack.push(r);
                }
                Op::Call(f, n) => {
                    let at = stack.len() - n as usize;
                    let r = self.invoke(f, None, stack.drain(at..), None);
                    let r = tri!(r);
                    stack.push(r);
                }
                Op::CallVirtual(sel, n) => {
                    let at = stack.len() - n as usize;
                    let obj = tri!(self.object(&stack[at]));
                    let f = match self.p.classes[obj.class as usize].vtable.get(&sel) {
                        Some(f) => *f,
                        None => {
                            let name = self.p.selectors[sel as usize].name.clone();
                            tri!(Err(self.throw(ExcKind::UnsupportedOperation, format!("method {} not implemented", name))))
                        }
                    };
                    let r = self.invoke(f, None, stack.drain(at..), None);
                    let r = tri!(r);
                    stack.push(r);
                }
                Op::CallClosure(n) => {
                    let args = stack.split_off(stack.len() - n as usize);
                    let cv = pop!().deref();
                    let r = match cv {
                        Value::Closure(cl) => {
                            let caps = cl.captures.clone();
                            self.call(cl.func as u32, args, Some(&caps))
                        }
                        Value::Null => Err(self.throw(ExcKind::NullPointer, "call of a null function".into())),
                        other => Err(self.throw(ExcKind::ClassCast, format!("{} is not callable", other.type_name()))),
                    };
                    let r = tri!(r);
                    stack.push(r);
                }
                Op::New(cl, ctor, n) => {
                    let at = stack.len() - n as usize;
                    let ci = &self.p.classes[cl as usize];
                    let fields: Vec<Value> = ci.fields.iter().map(|f| if f.ty.is_nullable() { Value::Null } else { Value::Void }).collect();
                    let obj = Value::Object(Rc::new(Object { class: cl, fields: RefCell::new(fields), freed: Cell::new(false) }));
                    let r = self.invoke(ctor, Some(obj.clone()), stack.drain(at..), None);
                    tri!(r);
                    stack.push(obj);
                }
                Op::Builtin(b, n) => {
                    let args = stack.split_off(stack.len() - n as usize);
                    let r = tri!(builtins::call(b, args, self));
                    stack.push(r);
                }
                Op::BuiltinMutLocal(b, id, n) => {
                    let args = stack.split_off(stack.len() - n as usize);
                    let mut slot = std::mem::replace(&mut locals[id as usize], Value::Void);
                    let r = builtins::call_mut(b, &mut slot, args, self);
                    locals[id as usize] = slot;
                    let r = tri!(r);
                    stack.push(r);
                }
                Op::BuiltinMutRef(b, n) => {
                    let args = stack.split_off(stack.len() - n as usize);
                    let target = pop!();
                    let r = match target {
                        Value::Ref(rt) => rt.with_mut(|v| builtins::call_mut(b, v, args, self)),
                        mut other => builtins::call_mut(b, &mut other, args, self),
                    };
                    let r = tri!(r);
                    stack.push(r);
                }
                Op::MakeClosure(f, n) => {
                    let caps = stack.split_off(stack.len() - n as usize);
                    stack.push(Value::Closure(Rc::new(Closure { func: f as usize, captures: caps })));
                }
                Op::FuncRef(f) => stack.push(Value::Closure(Rc::new(Closure { func: f as usize, captures: vec![] }))),
                Op::Return => return Ok(pop!()),
                Op::ReturnVoid => return Ok(Value::Void),
                Op::Throw => {
                    let v = pop!();
                    let v = if v.is_null() { make_exception(self.p, ExcKind::NullPointer, "throw null".into()) } else { v };
                    tri!(Err(Exc(v)));
                }
                Op::CatchMatch(li) => {
                    let cls = match stack.last() {
                        Some(Value::Object(o)) => o.class,
                        _ => u32::MAX,
                    };
                    let ok = cls != u32::MAX && c.lists[li as usize].iter().any(|&k| self.p.is_subclass(cls, k));
                    stack.push(Value::Bool(ok));
                }
                Op::DropLocal(id) => {
                    let v = take_local(c, locals, id);
                    if !v.is_void() {
                        tri!(self.drop_value(v));
                    }
                }
                Op::DropTop => {
                    let v = pop!();
                    tri!(self.drop_value(v));
                }
                Op::Free => {
                    let v = pop!();
                    tri!(self.free(v));
                }
            }
        }
    }
}

fn take_local(c: &Chunk, locals: &mut [Value], id: u32) -> Value {
    let i = id as usize;
    if c.cells[i] {
        if let Value::Ref(r) = &locals[i] {
            let v = r.get();
            r.set(Value::Void);
            return v;
        }
    }
    std::mem::replace(&mut locals[i], Value::Void)
}

fn set_local(c: &Chunk, locals: &mut [Value], id: u32, v: Value) {
    let i = id as usize;
    if c.cells[i] {
        if let Value::Ref(r) = &locals[i] {
            r.set(v);
            return;
        }
        locals[i] = cell(v);
        return;
    }
    locals[i] = v;
}
