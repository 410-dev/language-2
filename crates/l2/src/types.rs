//! Semantic types.

use l2_runtime::{FloatTy, IntTy, RtType};

pub type ClassId = u32;
pub type IfaceId = u32;
pub type FuncId = u32;
pub type LocalId = u32;
pub type GlobalId = u32;
pub type SelectorId = u32;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    Void,
    /// Type of expressions that never complete; also used for error recovery.
    Never,
    Error,
    Bool,
    Int(IntTy),
    Big,
    Float(FloatTy),
    Str,
    Array(Box<Type>),
    Dict(Box<Type>, Box<Type>),
    Class(ClassId),
    Iface(IfaceId),
    Nullable(Box<Type>),
    Union(Vec<Type>),
    Tuple(Vec<Type>),
    Func(Vec<Type>, Box<Type>),
    Dyn,
    /// Type of the `null` literal.
    Null,
    /// `&T` (false) / `*T` (true)
    Ref(bool, Box<Type>),
}

impl Type {
    pub fn int32() -> Type {
        Type::Int(IntTy::I32)
    }
    pub fn int64() -> Type {
        Type::Int(IntTy::I64)
    }
    pub fn is_error(&self) -> bool {
        matches!(self, Type::Error | Type::Never)
    }
    pub fn is_numeric(&self) -> bool {
        matches!(self, Type::Int(_) | Type::Big | Type::Float(_))
    }
    pub fn is_integer(&self) -> bool {
        matches!(self, Type::Int(_) | Type::Big)
    }
    pub fn is_nullable(&self) -> bool {
        matches!(self, Type::Nullable(_) | Type::Null | Type::Dyn)
    }
    /// Copy types are duplicated on assignment; everything else moves (spec 9.2).
    pub fn is_copy(&self) -> bool {
        match self {
            Type::Bool | Type::Int(_) | Type::Float(_) | Type::Null | Type::Void | Type::Never | Type::Error => true,
            Type::Nullable(t) => t.is_copy(),
            Type::Union(ts) | Type::Tuple(ts) => ts.iter().all(|t| t.is_copy()),
            Type::Ref(false, _) => true,
            _ => false,
        }
    }
    /// Strips a reference, giving the referenced type.
    pub fn deref(&self) -> &Type {
        match self {
            Type::Ref(_, t) => t.deref(),
            t => t,
        }
    }
    pub fn is_mut_ref(&self) -> bool {
        matches!(self, Type::Ref(true, _))
    }
    pub fn non_null(&self) -> Type {
        match self {
            Type::Nullable(t) => (**t).clone(),
            t => t.clone(),
        }
    }
    pub fn nullable(self) -> Type {
        match self {
            Type::Nullable(_) | Type::Null | Type::Dyn | Type::Error => self,
            t => Type::Nullable(Box::new(t)),
        }
    }
    pub fn int_ty(&self) -> Option<IntTy> {
        match self {
            Type::Int(t) => Some(*t),
            _ => None,
        }
    }
}

pub fn int_type_by_name(n: &str) -> Option<IntTy> {
    Some(match n {
        "Int8" | "Byte" => IntTy::I8,
        "Int16" => IntTy::I16,
        "Int32" | "Int" => IntTy::I32,
        "Int64" => IntTy::I64,
        "UInt8" => IntTy::U8,
        "UInt16" => IntTy::U16,
        "UInt32" | "UInt" => IntTy::U32,
        "UInt64" => IntTy::U64,
        _ => return None,
    })
}

pub fn float_type_by_name(n: &str) -> Option<FloatTy> {
    Some(match n {
        "Float16" => FloatTy::F16,
        "Float32" | "Float" => FloatTy::F32,
        "Float64" => FloatTy::F64,
        _ => return None,
    })
}

/// Lossless implicit numeric widening (spec 4.11).
pub fn int_widens(from: IntTy, to: IntTy) -> bool {
    if from == to {
        return true;
    }
    match (from.signed(), to.signed()) {
        (true, true) | (false, false) => to.bits() > from.bits(),
        (false, true) => to.bits() > from.bits(),
        (true, false) => false,
    }
}

pub fn int_widens_to_float(from: IntTy, to: FloatTy) -> bool {
    let need = if from.signed() { from.bits() - 1 } else { from.bits() };
    need <= to.mantissa_bits()
}

pub fn float_widens(from: FloatTy, to: FloatTy) -> bool {
    let rank = |f: FloatTy| match f {
        FloatTy::F16 => 0,
        FloatTy::F32 => 1,
        FloatTy::F64 => 2,
    };
    rank(from) <= rank(to)
}

/// The smallest integer type both operands widen into, if any.
pub fn common_int(a: IntTy, b: IntTy) -> Option<IntTy> {
    if int_widens(a, b) {
        return Some(b);
    }
    if int_widens(b, a) {
        return Some(a);
    }
    [IntTy::I16, IntTy::I32, IntTy::I64].into_iter().find(|&t| int_widens(a, t) && int_widens(b, t))
}

/// Common numeric type for binary arithmetic.
pub fn common_numeric(a: &Type, b: &Type) -> Option<Type> {
    match (a, b) {
        (Type::Int(x), Type::Int(y)) => common_int(*x, *y).map(Type::Int),
        (Type::Big, Type::Int(_)) | (Type::Int(_), Type::Big) | (Type::Big, Type::Big) => Some(Type::Big),
        (Type::Float(x), Type::Float(y)) => Some(Type::Float(if float_widens(*x, *y) { *y } else { *x })),
        (Type::Float(f), Type::Int(i)) | (Type::Int(i), Type::Float(f)) => {
            let mut t = *f;
            while !int_widens_to_float(*i, t) {
                t = match t {
                    FloatTy::F16 => FloatTy::F32,
                    FloatTy::F32 => FloatTy::F64,
                    FloatTy::F64 => return Some(Type::Float(FloatTy::F64)),
                };
            }
            Some(Type::Float(t))
        }
        (Type::Float(f), Type::Big) | (Type::Big, Type::Float(f)) => Some(Type::Float(if *f == FloatTy::F64 { FloatTy::F64 } else { FloatTy::F64 })),
        _ => None,
    }
}

/// Runtime type descriptor for a semantic type.
pub fn rt_type(t: &Type) -> RtType {
    match t {
        Type::Int(i) => RtType::Int(*i),
        Type::Big => RtType::Big,
        Type::Float(f) => RtType::Float(*f),
        Type::Bool => RtType::Bool,
        Type::Str => RtType::Str,
        Type::Array(e) => RtType::Array(Box::new(rt_type(e))),
        Type::Dict(k, v) => RtType::Dict(Box::new(rt_type(k)), Box::new(rt_type(v))),
        Type::Class(c) => RtType::Class(*c),
        Type::Iface(i) => RtType::Iface(*i),
        Type::Nullable(t) => RtType::Nullable(Box::new(rt_type(t))),
        Type::Union(ts) => RtType::Union(ts.iter().map(rt_type).collect()),
        Type::Tuple(ts) => RtType::Tuple(ts.iter().map(rt_type).collect()),
        Type::Func(_, _) => RtType::Func,
        Type::Ref(_, t) => rt_type(t),
        Type::Void => RtType::Void,
        _ => RtType::Any,
    }
}
