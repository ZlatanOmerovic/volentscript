//! AST node types.
//!
//! Node definitions follow the grammar sketched in SPECS §9; the authoritative
//! grammar is the ES4 draft (`docs/es4lang-Jan06.pdf`). Real nodes land in P1
//! together with the parser.

#![forbid(unsafe_code)]

use span::Span;

/// A parsed compilation unit (one `.as` file).
///
/// P1 fills this with package declarations and top-level directives
/// (SPECS §9 `program := packageDecl* topLevel*`).
#[derive(Debug)]
pub struct Program {
    /// Span covering the whole file.
    pub span: Span,
}
