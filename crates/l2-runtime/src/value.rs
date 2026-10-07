//! Runtime value model shared by the interpreter, the bytecode VM and the native runtime.

use crate::bigint::BigInt;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl IntTy {
    pub const ALL: [IntTy; 8] =
        [IntTy::I8, IntTy::I16, IntTy::I32, IntTy::I64, IntTy::U8, IntTy::U16, IntTy::U32, IntTy::U64];

    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 => 64,
        }
    }
    pub fn signed(self) -> bool {
        matches!(self, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64)
    }
    pub fn min(self) -> i128 {
        if self.signed() {
            -(1i128 << (self.bits() - 1))
        } else {
            0
        }
    }
    pub fn max(self) -> i128 {
        if self.signed() {
            (1i128 << (self.bits() - 1)) - 1
        } else {
            (1i128 << self.bits()) - 1
        }
    }
    pub fn fits(self, v: i128) -> bool {
        v >= self.min() && v <= self.max()
    }
    /// Two's complement wrap-around into this type's range.
    pub fn wrap(self, v: i128) -> i128 {
        let bits = self.bits();
        let m = v & ((1i128 << bits) - 1);
        if self.signed() && m > self.max() {
            m - (1i128 << bits)
        } else {
            m
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "Int8",
            IntTy::I16 => "Int16",
            IntTy::I32 => "Int32",
            IntTy::I64 => "Int64",
            IntTy::U8 => "UInt8",
            IntTy::U16 => "UInt16",
            IntTy::U32 => "UInt32",
            IntTy::U64 => "UInt64",
        }
    }
    pub fn code(self) -> u8 {
        self as u8
    }
    pub fn from_code(c: u8) -> IntTy {
        IntTy::ALL[c as usize]
    }
    pub fn tag(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum FloatTy {
    F16,
    F32,
    F64,
}

impl FloatTy {
    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F16 => "Float16",
            FloatTy::F32 => "Float32",
            FloatTy::F64 => "Float64",
        }
    }
    pub fn tag(self) -> &'static str {
        match self {
            FloatTy::F16 => "f16",
            FloatTy::F32 => "f32",
            FloatTy::F64 => "f64",
        }
    }
    pub fn code(self) -> u8 {
        self as u8
    }
    pub fn from_code(c: u8) -> FloatTy {
        [FloatTy::F16, FloatTy::F32, FloatTy::F64][c as usize]
    }
    /// Round a value to this type's precision.
    pub fn round(self, v: f64) -> f64 {
        match self {
            FloatTy::F64 => v,
            FloatTy::F32 => v as f32 as f64,
            FloatTy::F16 => round_f16(v as f32) as f64,
        }
    }
    pub fn mantissa_bits(self) -> u32 {
        match self {
            FloatTy::F16 => 11,
            FloatTy::F32 => 24,
            FloatTy::F64 => 53,
        }
    }
}

pub fn f32_to_f16_bits(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let man = b & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let rem = m & ((1u32 << shift) - 1);
        let halfway = 1u32 << (shift - 1);
        let mut h = half;
        if rem > halfway || (rem == halfway && (half & 1) == 1) {
            h += 1;
        }
        return sign | h as u16;
    }
    let mut h = ((e as u32) << 10) | (man >> 13);
    let rem = man & 0x1fff;
    if rem > 0x1000 || (rem == 0x1000 && (h & 1) == 1) {
        h += 1;
    }
    sign | h as u16
}

pub fn f16_bits_to_f32(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    if exp == 0 {
        if man == 0 {
            return f32::from_bits(sign);
        }
        let v = man as f32 * (2f32).powi(-24);
        return if sign != 0 { -v } else { v };
    }
    if exp == 0x1f {
        return f32::from_bits(sign | 0x7f80_0000 | (man << 13));
    }
    f32::from_bits(sign | ((exp + 127 - 15) << 23) | (man << 13))
}

pub fn round_f16(x: f32) -> f32 {
    f16_bits_to_f32(f32_to_f16_bits(x))
}

/// Exceptions the runtime itself can raise. Mapped to prelude classes by name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExcKind {
    NullPointer,
    Arithmetic,
    ClassCast,
    IndexOutOfBounds,
    IllegalArgument,
    UnsupportedOperation,
    UseAfterFree,
    IO,
    StackOverflow,
}

impl ExcKind {
    pub fn class_name(self) -> &'static str {
        match self {
            ExcKind::NullPointer => "NullPointerException",
            ExcKind::Arithmetic => "ArithmeticException",
            ExcKind::ClassCast => "ClassCastException",
            ExcKind::IndexOutOfBounds => "IndexOutOfBoundsException",
            ExcKind::IllegalArgument => "IllegalArgumentException",
            ExcKind::UnsupportedOperation => "UnsupportedOperationException",
            ExcKind::UseAfterFree => "UseAfterFreeError",
            ExcKind::IO => "IOException",
            ExcKind::StackOverflow => "StackOverflowError",
        }
    }
    pub const ALL: [ExcKind; 9] = [
        ExcKind::NullPointer,
        ExcKind::Arithmetic,
        ExcKind::ClassCast,
        ExcKind::IndexOutOfBounds,
        ExcKind::IllegalArgument,
        ExcKind::UnsupportedOperation,
        ExcKind::UseAfterFree,
        ExcKind::IO,
        ExcKind::StackOverflow,
    ];
}

#[derive(Debug)]
pub struct Object {
    pub class: u32,
    pub fields: RefCell<Vec<Value>>,
    pub freed: Cell<bool>,
}

#[derive(Clone, Debug)]
pub struct ArrayVal {
    pub items: Vec<Value>,
    pub fixed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HKey {
    Str(String),
    Int(i128),
    Bool(bool),
    Float(u64),
    Null,
    Other(String),
}

#[derive(Clone, Debug, Default)]
pub struct DictVal {
    pub entries: Vec<(Value, Value)>,
    pub index: HashMap<HKey, usize>,
}

impl DictVal {
    pub fn get(&self, k: &Value) -> Option<&Value> {
        self.index.get(&hkey(k)).map(|&i| &self.entries[i].1)
    }
    pub fn insert(&mut self, k: Value, v: Value) {
        let hk = hkey(&k);
        if let Some(&i) = self.index.get(&hk) {
            self.entries[i].1 = v;
        } else {
            self.index.insert(hk, self.entries.len());
            self.entries.push((k, v));
        }
    }
    pub fn remove(&mut self, k: &Value) -> Option<Value> {
        let hk = hkey(k);
        let i = self.index.remove(&hk)?;
        let (_, v) = self.entries.remove(i);
        for idx in self.index.values_mut() {
            if *idx > i {
                *idx -= 1;
            }
        }
        Some(v)
    }
}

pub fn hkey(v: &Value) -> HKey {
    match v {
        Value::Str(s) => HKey::Str(s.to_string()),
        Value::Int(_, i) => HKey::Int(*i),
        Value::Big(b) => match b.to_i128() {
            Some(i) => HKey::Int(i),
            None => HKey::Other(b.to_string()),
        },
        Value::Bool(b) => HKey::Bool(*b),
        Value::Float(_, f) => {
            if f.fract() == 0.0 && f.abs() < 1e30 {
                HKey::Int(*f as i128)
            } else {
                HKey::Float(f.to_bits())
            }
        }
        Value::Null => HKey::Null,
        Value::Object(o) => HKey::Other(format!("obj@{:p}", Rc::as_ptr(o))),
        other => HKey::Other(format!("{:?}", other)),
    }
}

/// A closure. `func` is a backend-specific function handle (function id or native pointer).
#[derive(Debug)]
pub struct Closure {
    pub func: usize,
    pub captures: Vec<Value>,
}

/// A mutable reference (`*x`) to a storage location.
#[derive(Clone, Debug)]
pub enum RefTarget {
    Cell(Rc<RefCell<Value>>),
    Field(Rc<Object>, usize),
    /// An element of the array / dictionary stored at the parent location. The key is already
    /// validated (normalised array index or existing dictionary key).
    Elem(Rc<(RefTarget, Value)>),
}

impl RefTarget {
    pub fn get(&self) -> Value {
        match self {
            RefTarget::Cell(c) => c.borrow().clone(),
            RefTarget::Field(o, i) => o.fields.borrow()[*i].clone(),
            RefTarget::Elem(pk) => match pk.0.get() {
                Value::Array(a) => a.items.get(pk.1.as_int() as usize).cloned().unwrap_or(Value::Null),
                Value::Dict(d) => d.get(&pk.1).cloned().unwrap_or(Value::Null),
                _ => Value::Null,
            },
        }
    }
    pub fn set(&self, v: Value) {
        match self {
            RefTarget::Cell(c) => *c.borrow_mut() = v,
            RefTarget::Field(o, i) => o.fields.borrow_mut()[*i] = v,
            RefTarget::Elem(..) => self.with_mut(|slot| *slot = v),
        }
    }
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut Value) -> R) -> R {
        let mut f = Some(f);
        let mut out = None;
        self.with_mut_dyn(&mut |v| {
            if let Some(f) = f.take() {
                out = Some(f(v));
            }
        });
        out.expect("with_mut callback not invoked")
    }

    fn with_mut_dyn(&self, f: &mut dyn FnMut(&mut Value)) {
        match self {
            RefTarget::Elem(pk) => pk.0.with_mut_dyn(&mut |pv| match pv {
                Value::Array(a) => {
                    let m = Rc::make_mut(a);
                    let i = pk.1.as_int() as usize;
                    let mut x = std::mem::replace(&mut m.items[i], Value::Void);
                    f(&mut x);
                    m.items[i] = x;
                }
                Value::Dict(d) => {
                    let m = Rc::make_mut(d);
                    let mut x = m.get(&pk.1).cloned().unwrap_or(Value::Null);
                    f(&mut x);
                    m.insert(pk.1.clone(), x);
                }
                _ => f(&mut Value::Null),
            }),
            RefTarget::Cell(c) => {
                // Take the value out so that re-entrant access (e.g. callbacks) cannot alias.
                let mut v = std::mem::replace(&mut *c.borrow_mut(), Value::Void);
                f(&mut v);
                *c.borrow_mut() = v;
            }
            RefTarget::Field(o, i) => {
                let mut v = std::mem::replace(&mut o.fields.borrow_mut()[*i], Value::Void);
                f(&mut v);
                o.fields.borrow_mut()[*i] = v;
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum Value {
    /// No value. Also used as the "moved-out" marker for storage slots.
    Void,
    Null,
    Bool(bool),
    Int(IntTy, i128),
    Big(Rc<BigInt>),
    Float(FloatTy, f64),
    Str(Rc<String>),
    Array(Rc<ArrayVal>),
    Dict(Rc<DictVal>),
    Object(Rc<Object>),
    Tuple(Rc<Vec<Value>>),
    Closure(Rc<Closure>),
    Ref(RefTarget),
    /// Raw pointer-sized payload used by the native runtime (closure environments).
    Raw(usize),
}

impl Value {
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(Rc::new(s.into()))
    }
    pub fn i32(v: i32) -> Value {
        Value::Int(IntTy::I32, v as i128)
    }
    pub fn i64(v: i64) -> Value {
        Value::Int(IntTy::I64, v as i128)
    }
    pub fn array(items: Vec<Value>, fixed: bool) -> Value {
        Value::Array(Rc::new(ArrayVal { items, fixed }))
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
    pub fn is_void(&self) -> bool {
        matches!(self, Value::Void)
    }
    pub fn as_bool(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            _ => panic!("expected Boolean, got {:?}", self),
        }
    }
    pub fn as_int(&self) -> i128 {
        match self {
            Value::Int(_, v) => *v,
            Value::Big(b) => b.to_i128().unwrap_or(0),
            Value::Float(_, f) => *f as i128,
            _ => panic!("expected integer, got {:?}", self),
        }
    }
    pub fn as_f64(&self) -> f64 {
        match self {
            Value::Float(_, f) => *f,
            Value::Int(_, v) => *v as f64,
            Value::Big(b) => b.to_f64(),
            _ => panic!("expected number, got {:?}", self),
        }
    }
    pub fn as_str(&self) -> &str {
        match self {
            Value::Str(s) => s.as_str(),
            _ => panic!("expected String, got {:?}", self),
        }
    }
    /// Read through a mutable reference if this value is one.
    pub fn deref(&self) -> Value {
        match self {
            Value::Ref(r) => r.get().deref(),
            v => v.clone(),
        }
    }
    pub fn type_name(&self) -> String {
        match self {
            Value::Void => "void".into(),
            Value::Null => "Null".into(),
            Value::Bool(_) => "Boolean".into(),
            Value::Int(t, _) => t.name().into(),
            Value::Big(_) => "IntLarge".into(),
            Value::Float(t, _) => t.name().into(),
            Value::Str(_) => "String".into(),
            Value::Array(_) => "Array".into(),
            Value::Dict(_) => "Dictionary".into(),
            Value::Object(_) => "Object".into(),
            Value::Tuple(_) => "Tuple".into(),
            Value::Closure(_) => "Function".into(),
            Value::Ref(_) => "Reference".into(),
            Value::Raw(_) => "Raw".into(),
        }
    }
}

/// Runtime type descriptor used by `castTo`, `parse` and array defaults.
#[derive(Clone, Debug, PartialEq)]
pub enum RtType {
    Int(IntTy),
    Big,
    Float(FloatTy),
    Bool,
    Str,
    Array(Box<RtType>),
    Dict(Box<RtType>, Box<RtType>),
    Class(u32),
    Iface(u32),
    Nullable(Box<RtType>),
    Union(Vec<RtType>),
    Tuple(Vec<RtType>),
    Func,
    Any,
    Void,
}

impl RtType {
    pub fn encode(&self) -> String {
        match self {
            RtType::Int(t) => t.tag().into(),
            RtType::Big => "big".into(),
            RtType::Float(t) => t.tag().into(),
            RtType::Bool => "bool".into(),
            RtType::Str => "str".into(),
            RtType::Array(e) => format!("arr({})", e.encode()),
            RtType::Dict(k, v) => format!("dict({},{})", k.encode(), v.encode()),
            RtType::Class(c) => format!("obj({})", c),
            RtType::Iface(c) => format!("if({})", c),
            RtType::Nullable(t) => format!("opt({})", t.encode()),
            RtType::Union(ts) => format!("un({})", ts.iter().map(|t| t.encode()).collect::<Vec<_>>().join(",")),
            RtType::Tuple(ts) => format!("tup({})", ts.iter().map(|t| t.encode()).collect::<Vec<_>>().join(",")),
            RtType::Func => "fn".into(),
            RtType::Any => "any".into(),
            RtType::Void => "void".into(),
        }
    }

    pub fn decode(s: &str) -> Option<RtType> {
        let (t, rest) = Self::decode_at(s)?;
        if rest.is_empty() {
            Some(t)
        } else {
            None
        }
    }

    fn decode_list(mut s: &str) -> Option<(Vec<RtType>, &str)> {
        let mut out = Vec::new();
        if let Some(r) = s.strip_prefix(')') {
            return Some((out, r));
        }
        loop {
            let (t, r) = Self::decode_at(s)?;
            out.push(t);
            if let Some(r2) = r.strip_prefix(',') {
                s = r2;
            } else if let Some(r2) = r.strip_prefix(')') {
                return Some((out, r2));
            } else {
                return None;
            }
        }
    }

    fn decode_at(s: &str) -> Option<(RtType, &str)> {
        let end = s.find(|c: char| c == '(' || c == ')' || c == ',').unwrap_or(s.len());
        let head = &s[..end];
        let rest = &s[end..];
        let simple = match head {
            "i8" => Some(RtType::Int(IntTy::I8)),
            "i16" => Some(RtType::Int(IntTy::I16)),
            "i32" => Some(RtType::Int(IntTy::I32)),
            "i64" => Some(RtType::Int(IntTy::I64)),
            "u8" => Some(RtType::Int(IntTy::U8)),
            "u16" => Some(RtType::Int(IntTy::U16)),
            "u32" => Some(RtType::Int(IntTy::U32)),
            "u64" => Some(RtType::Int(IntTy::U64)),
            "big" => Some(RtType::Big),
            "f16" => Some(RtType::Float(FloatTy::F16)),
            "f32" => Some(RtType::Float(FloatTy::F32)),
            "f64" => Some(RtType::Float(FloatTy::F64)),
            "bool" => Some(RtType::Bool),
            "str" => Some(RtType::Str),
            "fn" => Some(RtType::Func),
            "any" => Some(RtType::Any),
            "void" => Some(RtType::Void),
            _ => None,
        };
        if let Some(t) = simple {
            return Some((t, rest));
        }
        let inner = rest.strip_prefix('(')?;
        match head {
            "obj" | "if" => {
                let close = inner.find(')')?;
                let n: u32 = inner[..close].parse().ok()?;
                let t = if head == "obj" { RtType::Class(n) } else { RtType::Iface(n) };
                Some((t, &inner[close + 1..]))
            }
            _ => {
                let (list, r) = Self::decode_list(inner)?;
                let t = match head {
                    "arr" if list.len() == 1 => RtType::Array(Box::new(list[0].clone())),
                    "dict" if list.len() == 2 => RtType::Dict(Box::new(list[0].clone()), Box::new(list[1].clone())),
                    "opt" if list.len() == 1 => RtType::Nullable(Box::new(list[0].clone())),
                    "un" => RtType::Union(list),
                    "tup" => RtType::Tuple(list),
                    _ => return None,
                };
                Some((t, r))
            }
        }
    }
}

/// Services a backend provides to the shared runtime (user-defined behaviour on objects).
pub trait Host {
    type Err;
    fn throw(&mut self, kind: ExcKind, msg: String) -> Self::Err;
    fn obj_to_string(&mut self, o: &Rc<Object>) -> Result<String, Self::Err>;
    /// The default `Name(field=value, ...)` form, ignoring any `toString` override.
    fn obj_default_string(&mut self, o: &Rc<Object>) -> Result<String, Self::Err>;
    fn obj_equals(&mut self, a: &Rc<Object>, b: &Rc<Object>) -> Result<bool, Self::Err>;
    fn obj_compare(&mut self, a: &Rc<Object>, b: &Rc<Object>) -> Result<i32, Self::Err>;
    fn is_subclass(&self, cls: u32, of: u32) -> bool;
    fn implements(&self, cls: u32, iface: u32) -> bool;
    fn class_name(&self, cls: u32) -> String;
}
