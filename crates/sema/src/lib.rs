//! Semantic analysis: resolution, type checking, coercion insertion.
//!
//! Pipeline stages 3–4 of SPECS §8: bind names/packages/imports, then
//! nominally type-check (§4.5) with null safety (§4.1) and generics (§4.2),
//! producing a typed AST. Implementation lands in P2.

#![forbid(unsafe_code)]

use ast::Program;
use diagnostics::{Diagnostic, ErrorCode};

/// A resolved, type-checked program.
///
/// P2 replaces this with the real typed AST.
#[derive(Debug)]
pub struct TypedProgram {
    /// The underlying syntactic program.
    pub program: Program,
}

/// Resolves and type-checks a parsed program.
///
/// P0 stub: always reports not-implemented.
pub fn check(_program: Program) -> Result<TypedProgram, Vec<Diagnostic>> {
    Err(vec![Diagnostic::error(
        ErrorCode::NOT_IMPLEMENTED,
        "semantic analysis is not implemented until Phase 2",
    )])
}
