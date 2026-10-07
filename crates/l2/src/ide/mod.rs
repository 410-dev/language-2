//! Editor tooling (spec 15.4): analysis of a source file for the language server — diagnostics,
//! hover, go to definition, completion, signature help and document symbols.
//!
//! The analysis runs the normal front end (with parser error recovery) and lets the checker
//! record what names refer to. Completion and signature help analyse a patched copy of the
//! text in which the position being edited is a complete name (see [`analysis`]).

pub mod analysis;
pub mod builtins;
pub mod text;

pub use analysis::*;

use crate::diag::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymKind {
    Variable,
    Parameter,
    Field,
    Method,
    Function,
    Constructor,
    Class,
    Interface,
    TypeParam,
    Module,
    Keyword,
    Constant,
    BuiltinType,
    BuiltinMethod,
}

/// A name in the analysed file and what it refers to.
#[derive(Clone, Debug)]
pub struct IdeRef {
    /// Position of the name (1-based line and column, in characters).
    pub line: u32,
    pub col: u32,
    /// Length of the name in characters.
    pub len: u32,
    pub kind: SymKind,
    /// Declaration-like text shown in a code block (`Int64 count`, `function void run()`).
    pub code: String,
    /// One line describing the symbol (`field of Point`), may be empty.
    pub about: String,
    /// Built-in documentation (built-in members have no declaration to read it from).
    pub doc: Option<String>,
    /// Where the symbol is declared: the declaration's span and the declared name.
    pub def: Option<(Span, String)>,
}

/// A completion candidate.
#[derive(Clone, Debug)]
pub struct CompItem {
    pub label: String,
    pub kind: SymKind,
    /// Signature or type shown next to the label.
    pub detail: String,
    pub doc: Option<String>,
    pub def: Option<(Span, String)>,
    /// For callables: whether the (first) overload takes parameters.
    pub takes_args: Option<bool>,
}

/// One signature of a callable, for signature help.
#[derive(Clone, Debug)]
pub struct SigInfo {
    pub label: String,
    pub params: Vec<String>,
    pub doc: Option<String>,
    pub def: Option<(Span, String)>,
}
