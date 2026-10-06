//! Untyped syntax tree produced by the parser.

use crate::diag::Span;

#[derive(Clone, Debug, PartialEq)]
pub enum TypeExpr {
    /// `Int64`, `Dictionary[String, Int]`, `Box[T]`, `List[?]`, `STVariable`, ...
    Named { name: String, args: Vec<TypeExpr>, span: Span },
    Array(Box<TypeExpr>),
    Nullable(Box<TypeExpr>),
    Union(Vec<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    Func(Vec<TypeExpr>, Box<TypeExpr>),
    Ref { mutable: bool, inner: Box<TypeExpr> },
    Wildcard(Span),
    Void,
}

impl TypeExpr {
    pub fn named(name: &str, span: Span) -> TypeExpr {
        TypeExpr::Named { name: name.to_string(), args: Vec::new(), span }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
    Ref,
    RefMut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        use BinOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Rem => "%",
            Pow => "**",
            BitAnd => "&&",
            BitOr => "||",
            BitXor => "^",
            Shl => "<<",
            Shr => ">>",
            Eq => "==",
            Ne => "!=",
            Lt => "<",
            Gt => ">",
            Le => "<=",
            Ge => ">=",
            And => "and",
            Or => "or",
        }
    }
}

#[derive(Clone, Debug)]
pub enum FStrPart {
    Lit(String),
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub enum LambdaBody {
    Block(Block),
    Expr(Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(u128),
    Float(f64),
    Str(String),
    FStr(Vec<FStrPart>),
    Bool(bool),
    Null,
    Ident(String),
    This,
    /// `super` or `super(Entity)` used as a method-call receiver.
    Super(Option<String>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Coalesce(Box<Expr>, Box<Expr>),
    NonNull(Box<Expr>),
    Call(Box<Expr>, Vec<Arg>),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    /// `(a, b, c)` or `return a, b, c` — multiple return values.
    Tuple(Vec<Expr>),
    Lambda { params: Vec<Param>, ret: Option<TypeExpr>, body: LambdaBody, is_move: bool },
    /// A type used in expression position, e.g. the argument of `castTo(Int8)` when it is not a
    /// plain identifier (`castTo(Int64[])`, `castTo(String?)`).
    TypeLit(TypeExpr),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Expr {
        Expr { kind, span }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DeclMods {
    pub immutable: bool,
    pub copied: bool,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub ty: TypeExpr,
    pub name: String,
    pub mods: DeclMods,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct SwitchCase {
    /// `None` for `default`.
    pub cond: Option<Expr>,
    pub body: Vec<Stmt>,
    pub fallthrough: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct CatchClause {
    pub types: Vec<String>,
    pub name: String,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    VarDecl { mods: DeclMods, ty: TypeExpr, name: String, init: Option<Expr> },
    /// `a, _, c = expr` — `None` targets are discarded.
    MultiAssign { targets: Vec<Option<Expr>>, value: Expr },
    Assign { target: Expr, op: Option<BinOp>, value: Expr },
    Expr(Expr),
    If { cond: Expr, then: Block, els: Option<Box<Stmt>> },
    ForC { init: Option<Box<Stmt>>, cond: Option<Expr>, step: Option<Box<Stmt>>, body: Block },
    ForEach { vars: Vec<String>, iter: Expr, body: Block },
    Switch { value: Expr, alias: Option<String>, cases: Vec<SwitchCase> },
    Break,
    Continue,
    Fallthrough,
    Return(Option<Expr>),
    Throw(Expr),
    Try { body: Block, catches: Vec<CatchClause>, finally: Option<Block> },
    Block(Block),
    Using { module: String, alias: String },
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Access {
    #[default]
    Default,
    Public,
    Protected,
    Private,
}

#[derive(Clone, Debug, Default)]
pub struct Mods {
    pub access: Access,
    pub is_static: bool,
    pub immutable: bool,
    pub copied: bool,
    pub is_default: bool,
    pub getter: bool,
    /// `Some(true)` = `setter.chain`, `Some(false)` = `setter.nochain`.
    pub setter: Option<bool>,
    pub annotations: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct TypeParam {
    pub name: String,
    pub bound: Option<TypeExpr>,
}

#[derive(Clone, Debug)]
pub struct FuncDecl {
    pub mods: Mods,
    pub ret: TypeExpr,
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub throws: Vec<String>,
    pub body: Option<Block>,
    /// `= origin Entity`
    pub delegate: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub mods: Mods,
    pub ty: TypeExpr,
    pub name: String,
    pub init: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct CtorDecl {
    pub mods: Mods,
    pub params: Vec<Param>,
    pub throws: Vec<String>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ClassDecl {
    pub mods: Mods,
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub extends: Option<TypeExpr>,
    pub implements: Vec<TypeExpr>,
    pub fields: Vec<FieldDecl>,
    pub ctors: Vec<CtorDecl>,
    pub methods: Vec<FuncDecl>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct InterfaceDecl {
    pub mods: Mods,
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub extends: Vec<TypeExpr>,
    pub methods: Vec<FuncDecl>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum DirValue {
    Ident(String),
    Bool(bool),
    Int(u128),
    List(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct Directive {
    pub name: String,
    pub words: Vec<String>,
    pub options: Vec<(String, DirValue, Span)>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Item {
    Function(FuncDecl),
    Class(ClassDecl),
    Interface(InterfaceDecl),
    Using { module: String, alias: String, span: Span },
}

#[derive(Clone, Debug)]
pub struct FileAst {
    pub file: u32,
    pub directives: Vec<Directive>,
    pub items: Vec<Item>,
}
