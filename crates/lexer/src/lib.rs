//! Lexer: `&str` → token stream.
//!
//! Token inventory and lexical rules follow the AS3 reference implementation
//! (`docs/avmplus/eval/eval-lex.{h,cpp}`, keyword table
//! `docs/avmplus/eval/generate-keyword-lexer.as`) on the ES3 lexical grammar
//! baseline (ECMA-262 3rd ed. §7). Deviations are P1 scope cuts, each marked:
//! no regex literals (P7), no E4X `@`/XML tokens (dropped, SPECS §5), no
//! octal literals (avmplus gates them behind `compiler->octal_literals`;
//! we reject like its default mode).

#![forbid(unsafe_code)]

mod scan;
mod token;

pub use scan::lex;
pub use token::{Token, TokenKind};
