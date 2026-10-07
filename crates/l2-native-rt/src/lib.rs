//! C ABI runtime linked statically into native language-2 executables.
//!
//! Native code keeps numbers and Booleans unboxed; every other value is a pointer to a boxed
//! [`Value`] from `l2-runtime` (a null pointer means `Null`). Ownership protocol used by the
//! generated code:
//! * expressions yield owned boxes (or borrowed ones that are duplicated when ownership is needed);
//! * runtime functions borrow their box arguments unless documented otherwise;
//! * user functions own their parameters and return owned boxes.
//!
//! Exceptions are signalled through a pending-exception flag (`L2_PENDING`) that generated code
//! checks after every call that may throw.

#![allow(clippy::missing_safety_doc)]

use l2_runtime::builtins;
use l2_runtime::ops::{self, ArithOp, CmpOp};
use l2_runtime::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub type BoxPtr = *mut Value;

#[no_mangle]
pub static mut L2_PENDING: u8 = 0;

/// Address of the pending-exception flag, for programs linked against the shared runtime
/// (they cannot import data symbols directly).
#[no_mangle]
pub extern "C" fn l2_pending_ptr() -> *mut u8 {
    std::ptr::addr_of_mut!(L2_PENDING)
}

type ToStringFn = extern "C" fn(BoxPtr) -> BoxPtr;
type EqualsFn = extern "C" fn(BoxPtr, BoxPtr) -> bool;
type CompareFn = extern "C" fn(BoxPtr, BoxPtr) -> i32;
type DropFn = extern "C" fn(BoxPtr);

/// Class descriptor emitted by the compiler (layout must match the generated IR).
#[repr(C)]
pub struct ClassDesc {
    pub parent: i32,
    pub name: *const u8,
    pub nfields: i32,
    pub field_names: *const *const u8,
    pub field_nullable: *const u8,
    pub flags: i32,
    pub to_string: Option<ToStringFn>,
    pub equals: Option<EqualsFn>,
    pub compare: Option<CompareFn>,
    pub drop: Option<DropFn>,
    pub ifaces: *const i32,
    pub nifaces: i32,
    pub to_json: Option<ToStringFn>,
    pub from_json: Option<ToStringFn>,
}

struct Class {
    name: String,
    parent: Option<u32>,
    field_names: Vec<String>,
    field_nullable: Vec<bool>,
    throwable: bool,
    to_string: Option<ToStringFn>,
    equals: Option<EqualsFn>,
    compare: Option<CompareFn>,
    drop: Option<DropFn>,
    ifaces: Vec<u32>,
    to_json: Option<ToStringFn>,
    from_json: Option<ToStringFn>,
}

struct State {
    classes: Vec<Class>,
    exc: Vec<u32>,
    pending: Option<Value>,
    types: HashMap<usize, RtType>,
    args: Vec<String>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State { classes: Vec::new(), exc: Vec::new(), pending: None, types: HashMap::new(), args: Vec::new() });
}

unsafe fn cstr(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(p as *const std::ffi::c_char).to_string_lossy().into_owned()
}

fn set_pending(v: Value) {
    ST.with(|s| s.borrow_mut().pending = Some(v));
    unsafe { L2_PENDING = 1 };
}

fn take_pending() -> Option<Value> {
    unsafe { L2_PENDING = 0 };
    ST.with(|s| s.borrow_mut().pending.take())
}

fn exception(kind: ExcKind, msg: String) -> Value {
    let cls = ST.with(|s| s.borrow().exc.get(kind as usize).copied().unwrap_or(0));
    Value::Object(Rc::new(Object { class: cls, fields: RefCell::new(vec![Value::str(msg)]), freed: Cell::new(false) }))
}

fn rt_throw(kind: ExcKind, msg: impl Into<String>) {
    set_pending(exception(kind, msg.into()));
}

/// Moves a value into a new box (`Null`/`Void` become a null pointer).
fn bx(v: Value) -> BoxPtr {
    match v {
        Value::Null | Value::Void => std::ptr::null_mut(),
        v => Box::into_raw(Box::new(v)),
    }
}

/// Reads (clones) the value of a box without taking ownership.
unsafe fn val(p: BoxPtr) -> Value {
    if p.is_null() {
        Value::Null
    } else {
        (*p).clone()
    }
}

unsafe fn vals(p: *const BoxPtr, n: i32) -> Vec<Value> {
    (0..n as usize).map(|i| val(*p.add(i))).collect()
}

fn class_of_name(cls: u32) -> String {
    ST.with(|s| s.borrow().classes.get(cls as usize).map(|c| c.name.clone()).unwrap_or_default())
}

/// Host implementation that calls back into compiled code.
pub struct NHost;

impl NHost {
    fn check() -> Result<(), Value> {
        match take_pending() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Host for NHost {
    type Err = Value;
    fn throw(&mut self, kind: ExcKind, msg: String) -> Value {
        exception(kind, msg)
    }
    fn obj_to_string(&mut self, o: &Rc<Object>) -> Result<String, Value> {
        if o.freed.get() {
            return Err(exception(ExcKind::UseAfterFree, "use of freed object".into()));
        }
        let f = ST.with(|s| s.borrow().classes.get(o.class as usize).and_then(|c| c.to_string));
        if let Some(f) = f {
            let r = f(bx(Value::Object(o.clone())));
            Self::check()?;
            let s = unsafe { val(r) };
            unsafe { l2_free(r) };
            return Ok(match s {
                Value::Str(s) => s.to_string(),
                other => ops::to_display(&other, self)?,
            });
        }
        default_object_string(o, self)
    }
    fn obj_to_json(&mut self, o: &Rc<Object>) -> Result<Option<Value>, Value> {
        let f = ST.with(|s| s.borrow().classes.get(o.class as usize).and_then(|c| c.to_json));
        let Some(f) = f else { return Ok(None) };
        let r = f(bx(Value::Object(o.clone())));
        Self::check()?;
        let v = unsafe { val(r) };
        unsafe { l2_free(r) };
        Ok(Some(v))
    }
    fn obj_from_json(&mut self, cls: u32, v: &Value) -> Result<Option<Value>, Value> {
        let f = ST.with(|s| s.borrow().classes.get(cls as usize).and_then(|c| c.from_json));
        let Some(f) = f else { return Ok(None) };
        let r = f(bx(v.clone()));
        Self::check()?;
        let out = unsafe { val(r) };
        unsafe { l2_free(r) };
        Ok(Some(out))
    }
    fn obj_default_string(&mut self, o: &Rc<Object>) -> Result<String, Value> {
        default_object_string(o, self)
    }
    fn obj_equals(&mut self, a: &Rc<Object>, b: &Rc<Object>) -> Result<bool, Value> {
        let f = ST.with(|s| s.borrow().classes.get(a.class as usize).and_then(|c| c.equals));
        if let Some(f) = f {
            let r = f(bx(Value::Object(a.clone())), bx(Value::Object(b.clone())));
            Self::check()?;
            return Ok(r);
        }
        ops::fields_equal(a, b, self)
    }
    fn obj_compare(&mut self, a: &Rc<Object>, b: &Rc<Object>) -> Result<i32, Value> {
        let f = ST.with(|s| s.borrow().classes.get(a.class as usize).and_then(|c| c.compare));
        match f {
            Some(f) => {
                let r = f(bx(Value::Object(a.clone())), bx(Value::Object(b.clone())));
                Self::check()?;
                Ok(r)
            }
            None => Err(exception(ExcKind::ClassCast, format!("{} is not Comparable", class_of_name(a.class)))),
        }
    }
    fn is_subclass(&self, cls: u32, of: u32) -> bool {
        ST.with(|s| {
            let s = s.borrow();
            let mut c = Some(cls);
            while let Some(cc) = c {
                if cc == of {
                    return true;
                }
                c = s.classes.get(cc as usize).and_then(|k| k.parent);
            }
            false
        })
    }
    fn implements(&self, cls: u32, iface: u32) -> bool {
        ST.with(|s| s.borrow().classes.get(cls as usize).map(|c| c.ifaces.contains(&iface)).unwrap_or(false))
    }
    fn class_name(&self, cls: u32) -> String {
        class_of_name(cls)
    }
}

fn default_object_string(o: &Rc<Object>, h: &mut NHost) -> Result<String, Value> {
    let (name, throwable, names) = ST.with(|s| {
        let s = s.borrow();
        let c = &s.classes[o.class as usize];
        (c.name.clone(), c.throwable, c.field_names.clone())
    });
    if throwable {
        let msg = match o.fields.borrow().first() {
            Some(Value::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        return Ok(if msg.is_empty() { name } else { format!("{}: {}", name, msg) });
    }
    let fields: Vec<Value> = o.fields.borrow().clone();
    let mut parts = Vec::new();
    for (n, v) in names.iter().zip(fields.iter()) {
        parts.push(format!("{}={}", n, ops::to_display_nested(v, h)?));
    }
    Ok(format!("{}({})", name, parts.join(", ")))
}

/// Runs a fallible runtime operation, turning an error into a pending exception.
fn guard<T: Default>(r: Result<T, Value>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => {
            set_pending(e);
            T::default()
        }
    }
}

fn guard_box(r: Result<Value, Value>) -> BoxPtr {
    match r {
        Ok(v) => bx(v),
        Err(e) => {
            set_pending(e);
            std::ptr::null_mut()
        }
    }
}

fn rt_type(t: *const u8) -> RtType {
    let key = t as usize;
    if let Some(r) = ST.with(|s| s.borrow().types.get(&key).cloned()) {
        return r;
    }
    let r = RtType::decode(&unsafe { cstr(t) }).unwrap_or(RtType::Any);
    ST.with(|s| s.borrow_mut().types.insert(key, r.clone()));
    r
}

// =============================================================================================
// initialisation

#[no_mangle]
pub unsafe extern "C" fn l2_rt_init(classes: *const ClassDesc, n: i32, exc: *const i32, nexc: i32, argc: i32, argv: *const *const u8) {
    let mut cs = Vec::new();
    for i in 0..n as usize {
        let d = &*classes.add(i);
        let nf = d.nfields as usize;
        cs.push(Class {
            name: cstr(d.name),
            parent: if d.parent < 0 { None } else { Some(d.parent as u32) },
            field_names: (0..nf).map(|j| cstr(*d.field_names.add(j))).collect(),
            field_nullable: (0..nf).map(|j| *d.field_nullable.add(j) != 0).collect(),
            throwable: d.flags & 1 != 0,
            to_string: d.to_string,
            equals: d.equals,
            compare: d.compare,
            drop: d.drop,
            ifaces: (0..d.nifaces as usize).map(|j| *d.ifaces.add(j) as u32).collect(),
            to_json: d.to_json,
            from_json: d.from_json,
        });
    }
    let excs = (0..nexc as usize).map(|i| *exc.add(i) as u32).collect();
    let args: Vec<String> = (1..argc.max(1) as usize).map(|i| cstr(*argv.add(i))).collect();
    ST.with(|s| {
        let mut s = s.borrow_mut();
        s.classes = cs;
        s.exc = excs;
        s.args = args;
    });
}

#[no_mangle]
pub extern "C" fn l2_args() -> BoxPtr {
    let args = ST.with(|s| s.borrow().args.clone());
    bx(Value::array(args.into_iter().map(Value::str).collect(), false))
}

/// Finishes the program: reports an uncaught exception and flushes stdout.
#[no_mangle]
pub extern "C" fn l2_rt_finish(code: i32) -> i32 {
    let code = match take_pending() {
        Some(e) => {
            builtins::flush_out();
            let msg = match &e {
                Value::Object(o) => NHost.obj_to_string(o).unwrap_or_else(|_| class_of_name(o.class)),
                other => format!("{:?}", other),
            };
            eprintln!("Exception in thread \"main\" {}", msg);
            1
        }
        None => code,
    };
    builtins::flush_out();
    code
}

// =============================================================================================
// boxes

#[no_mangle]
pub extern "C" fn l2_box_int(kind: u8, v: i64) -> BoxPtr {
    let t = IntTy::from_code(kind);
    let x = if t == IntTy::U64 { v as u64 as i128 } else if t.signed() { v as i128 } else { (v as u64 & (u64::MAX >> (64 - t.bits()))) as i128 };
    bx(Value::Int(t, x))
}

#[no_mangle]
pub extern "C" fn l2_box_f64(kind: u8, v: f64) -> BoxPtr {
    bx(Value::Float(FloatTy::from_code(kind), v))
}

#[no_mangle]
pub extern "C" fn l2_box_bool(b: bool) -> BoxPtr {
    bx(Value::Bool(b))
}

#[no_mangle]
pub unsafe extern "C" fn l2_unbox_int(p: BoxPtr) -> i64 {
    match val(p).deref() {
        Value::Int(_, v) => v as i64,
        Value::Big(b) => b.to_i128().unwrap_or(0) as i64,
        Value::Float(_, f) => f as i64,
        Value::Null => {
            rt_throw(ExcKind::NullPointer, "value is null");
            0
        }
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_unbox_f64(p: BoxPtr) -> f64 {
    match val(p).deref() {
        Value::Float(_, f) => f,
        Value::Int(_, v) => v as f64,
        Value::Null => {
            rt_throw(ExcKind::NullPointer, "value is null");
            0.0
        }
        _ => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_unbox_bool(p: BoxPtr) -> bool {
    match val(p).deref() {
        Value::Bool(b) => b,
        Value::Null => {
            rt_throw(ExcKind::NullPointer, "value is null");
            false
        }
        _ => false,
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_str_lit(p: *const u8, len: i64) -> BoxPtr {
    let s = std::str::from_utf8_unchecked(std::slice::from_raw_parts(p, len as usize));
    bx(Value::str(s))
}

#[no_mangle]
pub unsafe extern "C" fn l2_big_lit(p: *const u8, len: i64) -> BoxPtr {
    let s = std::str::from_utf8_unchecked(std::slice::from_raw_parts(p, len as usize));
    bx(Value::Big(Rc::new(BigInt::parse(s).unwrap_or_else(BigInt::zero))))
}

#[no_mangle]
pub unsafe extern "C" fn l2_dup(p: BoxPtr) -> BoxPtr {
    if p.is_null() {
        return p;
    }
    bx((*p).clone())
}

#[no_mangle]
pub unsafe extern "C" fn l2_free(p: BoxPtr) {
    if !p.is_null() {
        drop(Box::from_raw(p));
    }
}

#[no_mangle]
pub extern "C" fn l2_round_f16(x: f32) -> f32 {
    round_f16(x)
}

// =============================================================================================
// cells and references

#[no_mangle]
pub unsafe extern "C" fn l2_cell_new(v: BoxPtr) -> BoxPtr {
    let x = if v.is_null() { Value::Null } else { *Box::from_raw(v) };
    bx(Value::Ref(RefTarget::Cell(Rc::new(RefCell::new(x)))))
}

unsafe fn ref_target(r: BoxPtr) -> Option<RefTarget> {
    if r.is_null() {
        return None;
    }
    match &*r {
        Value::Ref(t) => Some(t.clone()),
        _ => None,
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_ref_get(r: BoxPtr) -> BoxPtr {
    match ref_target(r) {
        Some(t) => bx(t.get()),
        None => l2_dup(r),
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_ref_take(r: BoxPtr) -> BoxPtr {
    match ref_target(r) {
        Some(t) => {
            let v = t.get();
            t.set(Value::Void);
            bx(v)
        }
        None => std::ptr::null_mut(),
    }
}

/// Stores an owned box through a reference.
#[no_mangle]
pub unsafe extern "C" fn l2_ref_set(r: BoxPtr, v: BoxPtr) {
    let x = if v.is_null() { Value::Null } else { *Box::from_raw(v) };
    if let Some(t) = ref_target(r) {
        t.set(x);
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_field_ref(o: BoxPtr, idx: i32) -> BoxPtr {
    match object(o) {
        Some(obj) => bx(Value::Ref(RefTarget::Field(obj, idx as usize))),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_elem_ref(parent: BoxPtr, key: BoxPtr) -> BoxPtr {
    let t = match ref_target(parent) {
        Some(t) => t,
        None => RefTarget::Cell(Rc::new(RefCell::new(val(parent)))),
    };
    guard_box(builtins::elem_ref(t, val(key), &mut NHost).map(Value::Ref))
}

// =============================================================================================
// objects

unsafe fn object(p: BoxPtr) -> Option<Rc<Object>> {
    match val(p).deref() {
        Value::Object(o) => {
            if o.freed.get() {
                rt_throw(ExcKind::UseAfterFree, "use of freed object");
                return None;
            }
            Some(o)
        }
        Value::Null => {
            rt_throw(ExcKind::NullPointer, "member access on null");
            None
        }
        other => {
            rt_throw(ExcKind::ClassCast, format!("expected an object, got {}", other.type_name()));
            None
        }
    }
}

#[no_mangle]
pub extern "C" fn l2_new_object(cls: i32) -> BoxPtr {
    let nullable = ST.with(|s| s.borrow().classes[cls as usize].field_nullable.clone());
    let fields = nullable.iter().map(|n| if *n { Value::Null } else { Value::Void }).collect();
    bx(Value::Object(Rc::new(Object { class: cls as u32, fields: RefCell::new(fields), freed: Cell::new(false) })))
}

#[no_mangle]
pub unsafe extern "C" fn l2_class_of(p: BoxPtr) -> i32 {
    match object(p) {
        Some(o) => o.class as i32,
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_get_field(o: BoxPtr, idx: i32) -> BoxPtr {
    match object(o) {
        Some(obj) => bx(obj.fields.borrow()[idx as usize].clone()),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_get_field_int(o: BoxPtr, idx: i32) -> i64 {
    match object(o) {
        Some(obj) => match &obj.fields.borrow()[idx as usize] {
            Value::Int(_, v) => *v as i64,
            _ => 0,
        },
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_get_field_f64(o: BoxPtr, idx: i32) -> f64 {
    match object(o) {
        Some(obj) => match &obj.fields.borrow()[idx as usize] {
            Value::Float(_, v) => *v,
            _ => 0.0,
        },
        None => 0.0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_get_field_bool(o: BoxPtr, idx: i32) -> bool {
    match object(o) {
        Some(obj) => matches!(obj.fields.borrow()[idx as usize], Value::Bool(true)),
        None => false,
    }
}

/// Stores an owned box into a field.
#[no_mangle]
pub unsafe extern "C" fn l2_set_field(o: BoxPtr, idx: i32, v: BoxPtr) {
    let x = if v.is_null() { Value::Null } else { *Box::from_raw(v) };
    if let Some(obj) = object(o) {
        obj.fields.borrow_mut()[idx as usize] = x;
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_set_field_int(o: BoxPtr, idx: i32, kind: u8, v: i64) {
    let b = l2_box_int(kind, v);
    l2_set_field(o, idx, b);
}

#[no_mangle]
pub unsafe extern "C" fn l2_set_field_f64(o: BoxPtr, idx: i32, kind: u8, v: f64) {
    if let Some(obj) = object(o) {
        obj.fields.borrow_mut()[idx as usize] = Value::Float(FloatTy::from_code(kind), v);
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_set_field_bool(o: BoxPtr, idx: i32, v: bool) {
    if let Some(obj) = object(o) {
        obj.fields.borrow_mut()[idx as usize] = Value::Bool(v);
    }
}

// =============================================================================================
// tuples, dictionaries, closures

#[no_mangle]
pub unsafe extern "C" fn l2_make_tuple(items: *const BoxPtr, n: i32) -> BoxPtr {
    bx(Value::Tuple(Rc::new(vals(items, n))))
}

#[no_mangle]
pub unsafe extern "C" fn l2_make_array(items: *const BoxPtr, n: i32) -> BoxPtr {
    bx(Value::array(vals(items, n), false))
}

#[no_mangle]
pub unsafe extern "C" fn l2_tuple_get(t: BoxPtr, i: i32) -> BoxPtr {
    match val(t).deref() {
        Value::Tuple(items) => bx(items[i as usize].clone()),
        Value::Null => {
            rt_throw(ExcKind::NullPointer, "tuple is null");
            std::ptr::null_mut()
        }
        other => bx(other),
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_make_dict(items: *const BoxPtr, n: i32) -> BoxPtr {
    let v = vals(items, 2 * n);
    let mut d = DictVal::default();
    let mut it = v.into_iter();
    while let (Some(k), Some(x)) = (it.next(), it.next()) {
        d.insert(k, x);
    }
    bx(Value::Dict(Rc::new(d)))
}

#[no_mangle]
pub unsafe extern "C" fn l2_closure_new(f: *const u8, caps: *const BoxPtr, n: i32) -> BoxPtr {
    bx(Value::Closure(Rc::new(Closure { func: f as usize, captures: vals(caps, n) })))
}

#[no_mangle]
pub unsafe extern "C" fn l2_closure_fn(c: BoxPtr) -> *const u8 {
    match val(c).deref() {
        Value::Closure(cl) => cl.func as *const u8,
        Value::Null => {
            rt_throw(ExcKind::NullPointer, "call of a null function");
            std::ptr::null()
        }
        other => {
            rt_throw(ExcKind::ClassCast, format!("{} is not callable", other.type_name()));
            std::ptr::null()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_closure_cap(c: BoxPtr, i: i32) -> BoxPtr {
    match &*c {
        Value::Closure(cl) => bx(cl.captures[i as usize].clone()),
        _ => std::ptr::null_mut(),
    }
}

// =============================================================================================
// strings

#[no_mangle]
pub extern "C" fn l2_sb_new() -> *mut String {
    Box::into_raw(Box::new(String::new()))
}

#[no_mangle]
pub unsafe extern "C" fn l2_sb_push_box(sb: *mut String, v: BoxPtr) {
    let x = val(v);
    match ops::to_display(&x, &mut NHost) {
        Ok(s) => (*sb).push_str(&s),
        Err(e) => set_pending(e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_sb_push_lit(sb: *mut String, p: *const u8, len: i64) {
    (*sb).push_str(std::str::from_utf8_unchecked(std::slice::from_raw_parts(p, len as usize)));
}

#[no_mangle]
pub unsafe extern "C" fn l2_sb_push_int(sb: *mut String, kind: u8, v: i64) {
    let t = IntTy::from_code(kind);
    if t == IntTy::U64 || (!t.signed()) {
        (*sb).push_str(&(v as u64 & (u64::MAX >> (64 - t.bits()))).to_string());
    } else {
        (*sb).push_str(&v.to_string());
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_sb_push_f64(sb: *mut String, kind: u8, v: f64) {
    (*sb).push_str(&ops::fmt_float(FloatTy::from_code(kind), v));
}

#[no_mangle]
pub unsafe extern "C" fn l2_sb_push_bool(sb: *mut String, b: bool) {
    (*sb).push_str(if b { "true" } else { "false" });
}

#[no_mangle]
pub unsafe extern "C" fn l2_sb_finish(sb: *mut String) -> BoxPtr {
    let s = *Box::from_raw(sb);
    bx(Value::str(s))
}

// =============================================================================================
// operators

#[no_mangle]
pub unsafe extern "C" fn l2_arith(op: u8, a: BoxPtr, b: BoxPtr, wrap: bool) -> BoxPtr {
    guard_box(ops::arith(ArithOp::from_code(op), &val(a), &val(b), wrap, &mut NHost))
}

#[no_mangle]
pub unsafe extern "C" fn l2_negate(a: BoxPtr, wrap: bool) -> BoxPtr {
    guard_box(ops::negate(&val(a), wrap, &mut NHost))
}

#[no_mangle]
pub unsafe extern "C" fn l2_bitnot(a: BoxPtr) -> BoxPtr {
    bx(ops::bit_not(&val(a)))
}

#[no_mangle]
pub unsafe extern "C" fn l2_compare(op: u8, a: BoxPtr, b: BoxPtr) -> bool {
    guard(ops::compare(CmpOp::from_code(op), &val(a), &val(b), &mut NHost))
}

#[no_mangle]
pub extern "C" fn l2_pow_int(kind: u8, a: i64, b: i64, wrap: bool) -> i64 {
    let t = IntTy::from_code(kind);
    let (x, y) = if t.signed() { (a as i128, b as i128) } else { (a as u64 as i128, b as u64 as i128) };
    match ops::int_arith(ArithOp::Pow, t, x, y, wrap, &mut NHost) {
        Ok(Value::Int(_, v)) => v as i64,
        Ok(_) => 0,
        Err(e) => {
            set_pending(e);
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn l2_throw_overflow(kind: u8, op: u8) {
    let t = IntTy::from_code(kind);
    let sym = if op == 255 { "unary -" } else { ArithOp::from_code(op).symbol() };
    rt_throw(ExcKind::Arithmetic, format!("integer overflow: {} {}", t.name(), sym));
}

#[no_mangle]
pub extern "C" fn l2_throw_div_zero(is_rem: bool) {
    rt_throw(ExcKind::Arithmetic, if is_rem { "% by zero" } else { "/ by zero" });
}

#[no_mangle]
pub extern "C" fn l2_throw_stack_overflow() {
    rt_throw(ExcKind::StackOverflow, "stack depth limit exceeded");
}

#[no_mangle]
pub unsafe extern "C" fn l2_convert(v: BoxPtr, ty: *const u8) -> BoxPtr {
    let t = rt_type(ty);
    bx(ops::widen(val(v), &t))
}

#[no_mangle]
pub unsafe extern "C" fn l2_cast(v: BoxPtr, ty: *const u8, wrap: bool) -> BoxPtr {
    let t = rt_type(ty);
    guard_box(ops::cast(&val(v), &t, wrap, &mut NHost))
}

/// `x!`: raises NullPointerException when the value (or a tuple element) is null.
#[no_mangle]
pub unsafe extern "C" fn l2_nonnull(v: BoxPtr, tuple: bool) {
    let bad = match val(v) {
        Value::Null => true,
        Value::Tuple(items) if tuple => items.iter().any(|x| x.is_null()),
        _ => false,
    };
    if bad {
        rt_throw(ExcKind::NullPointer, "non-null assertion failed: value is null");
    }
}

// =============================================================================================
// exceptions

/// `throw x` (takes ownership of the box).
#[no_mangle]
pub unsafe extern "C" fn l2_throw(v: BoxPtr) {
    if v.is_null() {
        rt_throw(ExcKind::NullPointer, "throw null");
        return;
    }
    set_pending(*Box::from_raw(v));
}

#[no_mangle]
pub extern "C" fn l2_take_pending() -> BoxPtr {
    match take_pending() {
        Some(v) => bx(v),
        None => std::ptr::null_mut(),
    }
}

/// Re-raises an exception box (takes ownership).
#[no_mangle]
pub unsafe extern "C" fn l2_set_pending(v: BoxPtr) {
    if !v.is_null() {
        set_pending(*Box::from_raw(v));
    }
}

#[no_mangle]
pub unsafe extern "C" fn l2_exc_matches(e: BoxPtr, classes: *const i32, n: i32) -> bool {
    let cls = match val(e) {
        Value::Object(o) => o.class,
        _ => return false,
    };
    (0..n as usize).any(|i| NHost.is_subclass(cls, *classes.add(i) as u32))
}

// =============================================================================================
// builtins

#[no_mangle]
pub unsafe extern "C" fn l2_builtin(id: u16, args: *const BoxPtr, n: i32) -> BoxPtr {
    guard_box(builtins::call(Builtin::from_u16(id), vals(args, n), &mut NHost))
}

/// Mutating builtin on a local slot holding a box (`*slot` may be replaced).
#[no_mangle]
pub unsafe extern "C" fn l2_builtin_mut_slot(id: u16, slot: *mut BoxPtr, args: *const BoxPtr, n: i32) -> BoxPtr {
    let a = vals(args, n);
    if (*slot).is_null() {
        rt_throw(ExcKind::NullPointer, format!("{} on null", Builtin::from_u16(id).name()));
        return std::ptr::null_mut();
    }
    let p = *slot;
    let mut v = std::mem::replace(&mut *p, Value::Void);
    let r = builtins::call_mut(Builtin::from_u16(id), &mut v, a, &mut NHost);
    *p = v;
    guard_box(r)
}

#[no_mangle]
pub unsafe extern "C" fn l2_builtin_mut_ref(id: u16, r: BoxPtr, args: *const BoxPtr, n: i32) -> BoxPtr {
    let a = vals(args, n);
    match ref_target(r) {
        Some(t) => guard_box(t.with_mut(|v| builtins::call_mut(Builtin::from_u16(id), v, a, &mut NHost))),
        None => {
            rt_throw(ExcKind::NullPointer, "mutation through a null reference");
            std::ptr::null_mut()
        }
    }
}

// =============================================================================================
// drop / free

fn drop_rec(v: &Value) -> Result<(), Value> {
    match v {
        Value::Object(o) => {
            if o.freed.get() {
                return Ok(());
            }
            let f = ST.with(|s| s.borrow().classes.get(o.class as usize).and_then(|c| c.drop));
            if let Some(f) = f {
                f(bx(Value::Object(o.clone())));
                NHost::check()?;
            }
            let fields: Vec<Value> = o.fields.borrow().clone();
            for x in fields.iter().rev() {
                drop_rec(x)?;
            }
            Ok(())
        }
        Value::Array(a) => a.items.iter().try_for_each(|v| drop_rec(&v)),
        Value::Dict(d) => d.entries.iter().try_for_each(|(_, x)| drop_rec(x)),
        Value::Tuple(t) => t.iter().try_for_each(drop_rec),
        _ => Ok(()),
    }
}

/// Runs `drop()` hooks for a value going out of scope (spec 9.6). Does not free the box.
#[no_mangle]
pub unsafe extern "C" fn l2_drop_value(v: BoxPtr) {
    if v.is_null() {
        return;
    }
    if let Err(e) = drop_rec(&*v) {
        set_pending(e);
    }
}

fn free_rec(v: &Value) -> Result<(), Value> {
    match v {
        Value::Object(o) => {
            if o.freed.get() {
                return Ok(());
            }
            let f = ST.with(|s| s.borrow().classes.get(o.class as usize).and_then(|c| c.drop));
            if let Some(f) = f {
                f(bx(Value::Object(o.clone())));
                NHost::check()?;
            }
            o.freed.set(true);
            let fields: Vec<Value> = o.fields.borrow().clone();
            for x in fields.iter().rev() {
                free_rec(x)?;
            }
            Ok(())
        }
        Value::Array(a) => a.items.iter().try_for_each(|v| free_rec(&v)),
        Value::Dict(d) => d.entries.iter().try_for_each(|(_, x)| free_rec(x)),
        Value::Tuple(t) => t.iter().try_for_each(free_rec),
        _ => Ok(()),
    }
}

/// `free(x)` in manual memory mode.
#[no_mangle]
pub unsafe extern "C" fn l2_free_manual(v: BoxPtr) {
    if v.is_null() {
        return;
    }
    if let Value::Object(o) = &*v {
        if o.freed.get() {
            rt_throw(ExcKind::UseAfterFree, "double free of an object");
            return;
        }
    }
    if let Err(e) = free_rec(&*v) {
        set_pending(e);
    }
}
