//! MIR: the typed, desugared, backend-agnostic mid-level IR.
//!
//! The frontend lowers the typed AST into this IR (closure conversion,
//! `for each` desugaring, explicit coercions, generics instantiation —
//! SPECS §8 stage 5); backends consume it. Nothing in this crate may
//! reference any backend, and no backend type may appear above `codegen`
//! (CLAUDE.md prime directive 3). Real IR lands in P3.

#![forbid(unsafe_code)]

/// A complete lowered program, ready for a backend.
///
/// P3 fills this with functions, bodies, and runtime-type metadata.
#[derive(Debug, Default)]
pub struct Program {}
