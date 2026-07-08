//! Lexer: `&str` → token stream.
//!
//! Token inventory and lexing rules follow the ES4 draft; the real lexer
//! lands in P1. P0 defines the stream shape so downstream signatures are
//! stable.

#![forbid(unsafe_code)]

use span::Span;

/// What kind of token was lexed.
///
/// P1 adds the full AS3/ES4 inventory (identifiers, keywords, literals,
/// punctuation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// End of input. Always the final token of a stream.
    Eof,
}

/// One lexed token.
#[derive(Debug, Clone, Copy)]
pub struct Token {
    /// The token's kind.
    pub kind: TokenKind,
    /// Where it came from.
    pub span: Span,
}
