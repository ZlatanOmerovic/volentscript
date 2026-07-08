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

    /// Attaches a primary span.
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    /// Renders the diagnostic as a single line (caret rendering follows in P1).
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
