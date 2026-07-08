//! Parser: tokens → AST.
//!
//! Recursive descent over the AS3 grammar per the ES4 draft (SPECS §9).
//! Implementation lands in P1.

#![forbid(unsafe_code)]

use ast::Program;
use diagnostics::{Diagnostic, ErrorCode};
use span::{SourceId, SourceMap};

/// Parses one registered source file into a [`Program`].
///
/// P0 stub: always reports not-implemented. P1 replaces this with the real
/// recursive-descent parser.
pub fn parse(_sources: &SourceMap, _file: SourceId) -> Result<Program, Vec<Diagnostic>> {
    Err(vec![Diagnostic::error(
        ErrorCode::NOT_IMPLEMENTED,
        "parsing is not implemented until Phase 1",
    )])
}
