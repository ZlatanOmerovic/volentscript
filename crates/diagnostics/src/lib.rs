//! Diagnostics: severities, stable error codes, and rendering.
//!
//! User-program errors are always reported through [`Diagnostic`] values —
//! never panics (CLAUDE.md §4). Caret rendering against the source map lands
//! with the first real consumers in P1; the type carries real spans from day
//! one so no call site needs retrofitting.

#![forbid(unsafe_code)]

use span::Span;

/// How severe a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Compilation cannot succeed.
    Error,
    /// Suspicious but not fatal (e.g. `instanceof` deprecation, SPECS §3.9).
    Warning,
    /// Attached explanatory note.
    Note,
}

/// A stable, documented error code (rendered as e.g. `error[E0001]`).
///
/// Codes are append-only: once published they are never reused or renumbered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorCode(pub u16);

impl ErrorCode {
    /// Feature not yet implemented at the current phase gate.
    pub const NOT_IMPLEMENTED: ErrorCode = ErrorCode(1);

    // E01xx — lexical errors.
    /// Character that cannot start any token.
    pub const UNEXPECTED_CHAR: ErrorCode = ErrorCode(101);
    /// String literal not closed before line end / EOF.
    pub const UNTERMINATED_STRING: ErrorCode = ErrorCode(102);
    /// Block comment not closed before EOF.
    pub const UNTERMINATED_COMMENT: ErrorCode = ErrorCode(103);
    /// Ill-formed numeric literal.
    pub const MALFORMED_NUMBER: ErrorCode = ErrorCode(104);
    /// Regex literal not closed before line end / EOF.
    pub const UNTERMINATED_REGEX: ErrorCode = ErrorCode(105);
    /// Regex pattern or flags rejected by the engine.
    pub const INVALID_REGEX: ErrorCode = ErrorCode(106);

    // E03xx — semantic errors.
    /// Name not found in any scope.
    pub const UNRESOLVED_NAME: ErrorCode = ErrorCode(301);
    /// Implicit conversion between the two types is not allowed.
    pub const INCOMPATIBLE_TYPES: ErrorCode = ErrorCode(302);
    /// Too few / too many call arguments.
    pub const WRONG_ARG_COUNT: ErrorCode = ErrorCode(303);
    /// Write to a `const` binding.
    pub const ASSIGN_TO_CONST: ErrorCode = ErrorCode(304);
    /// Redeclaration with a different type.
    pub const CONFLICTING_DECL: ErrorCode = ErrorCode(305);
    /// Call of a non-function value.
    pub const NOT_CALLABLE: ErrorCode = ErrorCode(306);
    /// Unknown or read-only member on a sealed type (SPECS §3.2).
    pub const UNKNOWN_PROPERTY: ErrorCode = ErrorCode(307);
    /// Function with a declared return type can complete without returning.
    pub const MISSING_RETURN: ErrorCode = ErrorCode(308);
    /// `void` used where a value is required.
    pub const VOID_VALUE: ErrorCode = ErrorCode(309);
    /// `break`/`continue` without a matching loop/label.
    pub const BAD_JUMP: ErrorCode = ErrorCode(310);
    /// `is`/`as` right side does not name a type.
    pub const NOT_A_TYPE: ErrorCode = ErrorCode(311);
    /// Possibly-null value into a non-nullable slot (SPECS §4.1).
    pub const NULL_FLOW: ErrorCode = ErrorCode(312);
    /// Dereference of a possibly-null value (SPECS §4.1).
    pub const NULL_DEREF: ErrorCode = ErrorCode(313);

    // E02xx — syntax errors.
    /// Token cannot appear here.
    pub const UNEXPECTED_TOKEN: ErrorCode = ErrorCode(201);
    /// Statement needs `;` or a line break (ECMA-262 3rd ed. §7.9).
    pub const EXPECTED_SEMICOLON: ErrorCode = ErrorCode(202);
    /// `for each` without `in` (avmplus SYNTAXERR_FOR_EACH_REQS_IN).
    pub const FOR_EACH_REQUIRES_IN: ErrorCode = ErrorCode(203);
    /// Left side of an assignment is not assignable.
    pub const INVALID_ASSIGN_TARGET: ErrorCode = ErrorCode(204);
    /// Syntax that is reserved/parsed but rejected (e.g. `goto`, `..`).
    pub const UNSUPPORTED_SYNTAX: ErrorCode = ErrorCode(205);
}

/// One user-facing diagnostic message, optionally anchored to source.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// Severity of the message.
    pub severity: Severity,
    /// Stable code, if this diagnostic has one.
    pub code: Option<ErrorCode>,
    /// Human-readable message.
    pub message: String,
    /// Primary source location, if any.
    pub span: Option<Span>,
}

impl Diagnostic {
    /// Creates an error diagnostic with a stable code.
    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: Some(code),
            message: message.into(),
            span: None,
        }
    }

    /// Creates a warning diagnostic (does not fail the build).
    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code: None,
            message: message.into(),
            span: None,
        }
    }

    /// Attaches a primary span.
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    /// Renders the diagnostic as a single line, without source context.
    pub fn render(&self) -> String {
        let label = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        };
        match self.code {
            Some(ErrorCode(n)) => format!("{label}[E{n:04}]: {}", self.message),
            None => format!("{label}: {}", self.message),
        }
    }

    /// Renders the diagnostic with file/line/column and a caret line:
    ///
    /// ```text
    /// error[E0202]: expected `;` or a line break
    ///   --> demo.as:3:12
    ///    |
    ///  3 | trace("a") trace("b");
    ///    |            ^^^^^
    /// ```
    pub fn render_full(&self, sources: &span::SourceMap) -> String {
        let mut out = self.render();
        let Some(span) = self.span else {
            return out;
        };
        let start = sources.line_col(span.source, span.start);
        let end = sources.line_col(span.source, span.end);
        let name = &sources.get(span.source).name;
        let line_no = start.line.to_string();
        let gutter = " ".repeat(line_no.len());
        // Caret run stays on the first line even for multi-line spans.
        let width = if end.line == start.line {
            (end.col - start.col).max(1)
        } else {
            start.line_text.chars().count() + 1 - start.col
        };
        out.push_str(&format!(
            "\n {gutter}--> {name}:{line}:{col}\n \
             {gutter} |\n \
             {line_no} | {text}\n \
             {gutter} | {pad}{carets}",
            line = start.line,
            col = start.col,
            text = start.line_text,
            pad = " ".repeat(start.col - 1),
            carets = "^".repeat(width.max(1)),
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_code_and_message() {
        let d = Diagnostic::error(ErrorCode::NOT_IMPLEMENTED, "not implemented");
        assert_eq!(d.render(), "error[E0001]: not implemented");
    }
}
