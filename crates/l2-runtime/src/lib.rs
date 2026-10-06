//! Shared runtime for language-2: the value model, operators and library builtins used by the
//! tree-walking interpreter, the bytecode VM and (through `l2-native-rt`) native executables.
//! Sharing one implementation keeps the three backends' observable behaviour identical.

pub mod bigint;
pub mod builtins;
pub mod ops;
pub mod value;

pub use bigint::BigInt;
pub use builtins::Builtin;
pub use value::*;
