//! language-2: a statically typed language with Java/Python syntax and Rust-style ownership.
//!
//! Pipeline (spec 15.1): source → lexer → parser → AST → name resolution / type checking /
//! monomorphisation (HIR) → flow & ownership checks → one of three backends:
//! tree-walking interpreter, bytecode compiler + VM, or LLVM IR → native code.

pub mod ast;
pub mod check;
pub mod diag;
pub mod driver;
pub mod flow;
pub mod hir;
pub mod hir_visit;
pub mod interp;
pub mod bytecode;
pub mod vm;
pub mod llvm;
pub mod native;
pub mod lexer;
pub mod parser;
pub mod types;

/// The language name (provisional; the name is expected to change).
pub const LANG_NAME: &str = "language-2";
