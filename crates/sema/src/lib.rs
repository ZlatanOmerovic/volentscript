//! Semantic analysis: resolution, type checking, coercion insertion.
//!
//! Pipeline stages 3–4 of SPECS §8: bind names, then nominally type-check
//! (§4.5) the P2 core subset, producing a typed AST ([`tast`]) with all
//! implicit conversions made explicit. Classes/interfaces arrive in P4,
//! generics and null-safety enforcement in P5, closures in P6.

#![forbid(unsafe_code)]

mod builtins;
mod check;
mod tast;
mod tdump;
mod ty;

pub use builtins::BuiltinFn;
pub use check::{CheckOutcome, check};
pub use tast::*;
pub use tdump::dump;
pub use ty::Ty;
