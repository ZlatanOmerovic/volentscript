//! Parser: tokens → AST.
//!
//! Recursive descent over the AS3 core grammar. Productions and precedence
//! follow the reference implementation, avmplus RTC
//! (`docs/avmplus/eval/eval-parse-expr.cpp`, `eval-parse-stmt.cpp`), on the
//! ECMA-262 3rd ed. baseline. P1 scope: functions, primitives, expressions,
//! statements — classes/interfaces/packages are later phases; the grammar
//! rejects them here with a clear "not yet" diagnostic rather than a generic
//! parse error.

#![forbid(unsafe_code)]

mod expr;
mod stmt;

use ast::Program;
use diagnostics::{Diagnostic, ErrorCode};
use lexer::{Token, TokenKind};
use span::{SourceId, SourceMap, Span};

/// Parses one registered source file into a [`Program`].
///
/// All lexical and syntax errors are returned together; the parser recovers
/// at statement boundaries so one error doesn't mask the rest of the file.
pub fn parse(sources: &SourceMap, file: SourceId) -> Result<Program, Vec<Diagnostic>> {
    let text = &sources.get(file).text;
    let (tokens, mut diagnostics) = lexer::lex(file, text);
    let mut parser = Parser {
        tokens,
        pos: 0,
        diagnostics: Vec::new(),
        no_in: false,
    };
    let program = parser.program(file, text.len() as u32);
    diagnostics.extend(parser.diagnostics);
    // Lexer and parser diagnostics interleave; present them in source order.
    diagnostics.sort_by_key(|d| d.span.map(|s| (s.start, s.end)));
    if diagnostics.is_empty() {
        Ok(program)
    } else {
        Err(diagnostics)
    }
}

pub(crate) struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    diagnostics: Vec<Diagnostic>,
    /// Suppresses the `in` operator at the relational tier while parsing a
    /// `for(;;)`/`for..in` head (avmplus EFLAG_NoIn, eval-parse-inlines.h:88).
    pub(crate) no_in: bool,
}

impl Parser {
    fn program(&mut self, file: SourceId, len: u32) -> Program {
        let mut directives = Vec::new();
        while !self.at(&TokenKind::Eof) {
            let before = self.pos;
            directives.push(self.statement());
            if self.pos == before {
                // Statement parser made no progress (hard error at this
                // token): skip it so we always terminate.
                self.advance();
            }
        }
        Program {
            directives,
            span: Span::new(file, 0, len),
        }
    }

    // --- token plumbing ---------------------------------------------------

    pub(crate) fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }

    pub(crate) fn peek_kind(&self, offset: usize) -> &TokenKind {
        let i = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[i].kind
    }

    pub(crate) fn at(&self, kind: &TokenKind) -> bool {
        &self.current().kind == kind
    }

    /// True when the current token is the contextual identifier `word`
    /// (`each`, `get`, `set` are identifiers, not keywords — avmplus
    /// eval-parse-stmt.cpp:435).
    pub(crate) fn at_contextual(&self, word: &str) -> bool {
        matches!(&self.current().kind, TokenKind::Ident(name) if name == word)
    }

    pub(crate) fn advance(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    pub(crate) fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(crate) fn expect(&mut self, kind: &TokenKind) -> Span {
        if self.at(kind) {
            self.advance().span
        } else {
            let current = self.current().clone();
            self.error(
                ErrorCode::UNEXPECTED_TOKEN,
                format!(
                    "expected {}, found {}",
                    kind.describe(),
                    current.kind.describe()
                ),
                current.span,
            );
            current.span
        }
    }

    /// Expects an identifier and returns its name.
    pub(crate) fn expect_ident(&mut self) -> (String, Span) {
        if let TokenKind::Ident(name) = &self.current().kind {
            let name = name.clone();
            let span = self.advance().span;
            (name, span)
        } else {
            let span = self.current().span;
            self.error(
                ErrorCode::UNEXPECTED_TOKEN,
                format!(
                    "expected identifier, found {}",
                    self.current().kind.describe()
                ),
                span,
            );
            (String::from("<error>"), span)
        }
    }

    /// Consumes a `>` closing a `.<...>` type application, splitting glued
    /// `>>`/`>>>`/`>=`/`>>=`/`>>>=` tokens so nested applications like
    /// `Vector.<Vector.<int>>` close correctly. avmplus achieves the same
    /// with its T_BreakRightAngle meta-token (eval-lex.cpp:164); with eager
    /// lexing, re-splitting is the equivalent.
    pub(crate) fn expect_type_close(&mut self) {
        use TokenKind::*;
        let token = self.current().clone();
        let rest = match token.kind {
            Gt => {
                self.advance();
                return;
            }
            Ge => Assign,
            Shr => Gt,
            ShrAssign => Ge,
            Ushr => Shr,
            UshrAssign => ShrAssign,
            _ => {
                self.error(
                    ErrorCode::UNEXPECTED_TOKEN,
                    format!("expected `>`, found {}", token.kind.describe()),
                    token.span,
                );
                return;
            }
        };
        // Shrink the token in place: one `>` consumed, remainder stays current.
        self.tokens[self.pos] = Token {
            kind: rest,
            span: Span::new(token.span.source, token.span.start + 1, token.span.end),
            newline_before: false,
        };
    }

    // --- semicolon insertion ---------------------------------------------

    /// Ends a statement: eats `;`, or inserts one before `}` / EOF / a line
    /// break (ECMA-262 3rd ed. §7.9; avmplus `Parser::semicolon()`,
    /// eval-parse-stmt.cpp:149).
    pub(crate) fn semicolon(&mut self) {
        if self.eat(&TokenKind::Semicolon) {
            return;
        }
        if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) || self.current().newline_before
        {
            return;
        }
        let span = self.current().span;
        self.error(
            ErrorCode::EXPECTED_SEMICOLON,
            format!(
                "expected `;` or a line break before {}",
                self.current().kind.describe()
            ),
            span,
        );
    }

    /// True when the next token may continue a restricted production —
    /// i.e. no line break intervenes and it isn't `;`/`}`/EOF (avmplus
    /// `Parser::noNewline()`, eval-parse-stmt.cpp:167). Used by `return`,
    /// `break`/`continue` labels, and postfix `++`/`--`.
    pub(crate) fn no_newline(&self) -> bool {
        !self.current().newline_before
            && !matches!(
                self.current().kind,
                TokenKind::Semicolon | TokenKind::RBrace | TokenKind::Eof
            )
    }

    pub(crate) fn error(&mut self, code: ErrorCode, message: impl Into<String>, span: Span) {
        self.diagnostics
            .push(Diagnostic::error(code, message).with_span(span));
    }
}
