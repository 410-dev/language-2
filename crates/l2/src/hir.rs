//! Typed, resolved, monomorphic intermediate representation shared by all three backends.
//!
//! The checker lowers the AST into this form: names are resolved to ids, generics are
//! instantiated (monomorphised), implicit conversions are explicit, moves are explicit, and
//! sugar (for-each, compound assignment, switch aliases, multi-assignment) is desugared.

use crate::diag::Span;
use crate::types::*;
use l2_runtime::ops::{ArithOp, CmpOp};
use l2_runtime::{Builtin, ExcKind};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryMode {
    Ownership,
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Backend {
    Compiler,
    Interpreter,
    Bytecode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    I386,
    Amd64,
    Arm64,
}

impl Target {
    pub fn name(self) -> &'static str {
        match self {
            Target::I386 => "i386",
            Target::Amd64 => "amd64",
            Target::Arm64 => "arm64",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub sdk: u32,
    pub runtimes: Vec<Backend>,
    pub include_dependencies: bool,
    pub targets: Vec<Target>,
    pub software_emulation: bool,
    pub memory: MemoryMode,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            sdk: 1,
            runtimes: vec![Backend::Compiler, Backend::Interpreter, Backend::Bytecode],
            include_dependencies: false,
            targets: vec![Target::I386, Target::Amd64, Target::Arm64],
            software_emulation: false,
            memory: MemoryMode::Ownership,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub name: String,
    pub ty: Type,
    pub immutable: bool,
}

#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub name: String,
    pub parent: Option<ClassId>,
    /// All interfaces implemented, transitively.
    pub ifaces: Vec<IfaceId>,
    /// All fields including inherited ones (parent fields first).
    pub fields: Vec<FieldInfo>,
    /// Dynamic dispatch table: selector -> implementation.
    pub vtable: HashMap<SelectorId, FuncId>,
    pub drop_fn: Option<FuncId>,
    pub equals_fn: Option<FuncId>,
    pub to_string_fn: Option<FuncId>,
    pub compare_fn: Option<FuncId>,
    pub needs_drop: bool,
    pub is_throwable: bool,
}

#[derive(Clone, Debug)]
pub struct IfaceInfo {
    pub name: String,
    pub parents: Vec<IfaceId>,
}

#[derive(Clone, Debug)]
pub struct Selector {
    pub name: String,
    pub params: Vec<Type>,
    pub ret: Type,
}

#[derive(Clone, Debug)]
pub struct LocalInfo {
    pub name: String,
    pub ty: Type,
    /// Storage is shared (captured by reference or mutably borrowed with `*x`).
    pub cell: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Capture {
    /// Local of the lambda that receives the captured value.
    pub inner: LocalId,
    pub by_ref: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuncKind {
    Free,
    Method,
    Ctor,
    Lambda,
    Init,
}

#[derive(Clone, Debug)]
pub struct Func {
    pub name: String,
    pub kind: FuncKind,
    pub params: Vec<LocalId>,
    pub ret: Type,
    pub locals: Vec<LocalInfo>,
    pub body: Vec<Stmt>,
    /// Integer overflow policy of the defining file: true = wrap.
    pub wrap: bool,
    /// For methods/ctors: the class of `this` (local 0).
    pub this_class: Option<ClassId>,
    pub captures: Vec<Capture>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Global {
    pub name: String,
    pub ty: Type,
}

#[derive(Clone, Debug)]
pub enum Place {
    Local(LocalId),
    /// Through a `*T` reference held in a local.
    Deref(LocalId),
    Field(Box<Expr>, u32),
    Global(GlobalId),
    /// An element of the array / dictionary at another place (`grid[1][2] = 9`).
    Elem(Box<Place>, Box<Expr>),
}

#[derive(Clone, Debug)]
pub struct Catch {
    pub classes: Vec<ClassId>,
    pub local: LocalId,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct Case {
    /// `None` = default.
    pub cond: Option<Expr>,
    pub body: Vec<Stmt>,
    pub fallthrough: bool,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Let(LocalId, Option<Expr>),
    Assign(Place, Expr),
    Expr(Expr),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    /// `for (; cond; step) body` — `continue` runs `step`.
    Loop { cond: Option<Expr>, body: Vec<Stmt>, step: Vec<Stmt> },
    Break,
    Continue,
    Return(Option<Expr>),
    Throw(Expr),
    Try { body: Vec<Stmt>, catches: Vec<Catch>, finally: Option<Vec<Stmt>> },
    /// A lexical scope; `drops` lists owned locals declared in it that must be dropped on exit
    /// (in declaration order; they are dropped in reverse).
    Block { body: Vec<Stmt>, drops: Vec<LocalId> },
    Switch { cases: Vec<Case> },
    /// Drop the value currently held by an owned local (scope end / `free`).
    Free(Expr),
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Lit {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
    Big(String),
    Null,
    Void,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    BitNot,
    Not,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Lit(Lit),
    /// Copy / borrow read of a local.
    Local(LocalId),
    /// Move out of a local (the slot becomes empty in ownership mode).
    Move(LocalId),
    /// Read through a `*T` reference.
    Deref(Box<Expr>),
    Global(GlobalId),
    Field(Box<Expr>, u32),
    TupleGet(Box<Expr>, u32),
    Unary(UnaryOp, Box<Expr>),
    /// Arithmetic. Operands have the result type, except shift amounts and exponents.
    Arith(ArithOp, Box<Expr>, Box<Expr>, bool),
    /// String concatenation of the display forms of the parts.
    Concat(Vec<Expr>),
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Coalesce(Box<Expr>, Box<Expr>),
    /// `x!`: throws NullPointerException on Null.
    NonNull(Box<Expr>),
    /// Smart-cast unwrap of a value known to be non-null.
    Unwrap(Box<Expr>),
    /// Implicit lossless conversion to `expr.ty`.
    Convert(Box<Expr>),
    /// `.castTo(T)` to `expr.ty`; bool = wrap on overflow.
    Cast(Box<Expr>, bool),
    Call(FuncId, Vec<Expr>),
    /// Dynamic dispatch; `args[0]` is the receiver.
    CallVirtual(SelectorId, Vec<Expr>),
    CallClosure(Box<Expr>, Vec<Expr>),
    New(ClassId, FuncId, Vec<Expr>),
    Builtin(Builtin, Vec<Expr>),
    BuiltinMut(Builtin, Box<Place>, Vec<Expr>),
    /// Closure creation. Capture sources are given as places (by_ref) or expressions (by move).
    Lambda(FuncId, Vec<CaptureSrc>),
    FuncRef(FuncId),
    Dict(Vec<(Expr, Expr)>),
    Tuple(Vec<Expr>),
    /// `*x`: a mutable reference to a storage location.
    RefMut(Box<Place>),
    /// Run statements, then evaluate the expression (used by desugarings that need temporaries).
    Seq(Vec<Stmt>, Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum CaptureSrc {
    Ref(Place),
    Value(Expr),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, ty: Type, span: Span) -> Expr {
        Expr { kind, ty, span }
    }
}

#[derive(Clone, Debug)]
pub struct Program {
    pub classes: Vec<ClassInfo>,
    pub ifaces: Vec<IfaceInfo>,
    pub funcs: Vec<Func>,
    pub globals: Vec<Global>,
    pub selectors: Vec<Selector>,
    /// Runs static initialisers before `main`.
    pub init: FuncId,
    pub main: FuncId,
    pub main_takes_args: bool,
    pub config: Config,
    pub exc_classes: Vec<(ExcKind, ClassId)>,
    pub throwable: ClassId,
    pub any_droppable: bool,
    pub drop_selector: Option<SelectorId>,
}

impl Program {
    pub fn exc_class(&self, k: ExcKind) -> ClassId {
        self.exc_classes.iter().find(|(e, _)| *e == k).map(|(_, c)| *c).unwrap()
    }

    pub fn is_subclass(&self, mut c: ClassId, of: ClassId) -> bool {
        loop {
            if c == of {
                return true;
            }
            match self.classes[c as usize].parent {
                Some(p) => c = p,
                None => return false,
            }
        }
    }

    pub fn implements(&self, c: ClassId, i: IfaceId) -> bool {
        self.classes[c as usize].ifaces.contains(&i)
    }

    /// Whether values of this type may own objects that need `drop()` (spec 9.6).
    pub fn needs_drop(&self, t: &Type) -> bool {
        if !self.any_droppable {
            return false;
        }
        match t {
            Type::Class(c) => {
                let c = *c;
                self.classes.iter().enumerate().any(|(i, ci)| ci.needs_drop && self.is_subclass(i as ClassId, c))
            }
            Type::Iface(i) => self.classes.iter().any(|ci| ci.needs_drop && ci.ifaces.contains(i)),
            Type::Array(e) | Type::Nullable(e) => self.needs_drop(e),
            Type::Dict(k, v) => self.needs_drop(k) || self.needs_drop(v),
            Type::Union(ts) | Type::Tuple(ts) => ts.iter().any(|t| self.needs_drop(t)),
            Type::Dyn | Type::Func(_, _) => true,
            _ => false,
        }
    }

    pub fn type_name(&self, t: &Type) -> String {
        match t {
            Type::Void => "void".into(),
            Type::Never => "never".into(),
            Type::Error => "<error>".into(),
            Type::Bool => "Boolean".into(),
            Type::Int(i) => i.name().into(),
            Type::Big => "IntLarge".into(),
            Type::Float(f) => f.name().into(),
            Type::Str => "String".into(),
            Type::Array(e) => format!("{}[]", self.type_name(e)),
            Type::Dict(k, v) => format!("Dictionary[{}, {}]", self.type_name(k), self.type_name(v)),
            Type::Class(c) => self.classes.get(*c as usize).map(|c| c.name.clone()).unwrap_or("?".into()),
            Type::Iface(i) => self.ifaces.get(*i as usize).map(|c| c.name.clone()).unwrap_or("?".into()),
            Type::Nullable(t) => format!("{}?", self.type_name(t)),
            Type::Union(ts) => ts.iter().map(|t| self.type_name(t)).collect::<Vec<_>>().join("|"),
            Type::Tuple(ts) => format!("({})", ts.iter().map(|t| self.type_name(t)).collect::<Vec<_>>().join(", ")),
            Type::Func(ps, r) => format!(
                "Function[({}), {}]",
                ps.iter().map(|t| self.type_name(t)).collect::<Vec<_>>().join(", "),
                self.type_name(r)
            ),
            Type::Dyn => "DTVariable".into(),
            Type::Null => "Null".into(),
            Type::Ref(m, t) => format!("{}{}", if *m { "*" } else { "&" }, self.type_name(t)),
        }
    }
}
