//! Source positions, spans, and the source map.
//!
//! Every token, AST node, diagnostic, and MIR instruction carries a [`Span`]
//! so diagnostics can render carets against original source (SPECS §10).

#![forbid(unsafe_code)]

/// Identifies one source file registered in a [`SourceMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceId(pub u32);

/// A half-open byte range `[start, end)` within one source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// File this span points into.
    pub source: SourceId,
    /// Byte offset of the first character.
    pub start: u32,
    /// Byte offset one past the last character.
    pub end: u32,
}

impl Span {
    /// Creates a span covering `[start, end)` in `source`.
    pub fn new(source: SourceId, start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "span start must not exceed end");
        Self { source, start, end }
    }

    /// The smallest span covering both `self` and `other`.
    ///
    /// Both spans must belong to the same file.
    pub fn to(self, other: Span) -> Span {
        debug_assert_eq!(self.source, other.source);
        Span::new(
            self.source,
            self.start.min(other.start),
            self.end.max(other.end),
        )
    }
}

/// A registered source file: its name and full text.
#[derive(Debug)]
pub struct SourceFile {
    /// Display name (usually the path passed to the CLI).
    pub name: String,
    /// Complete file contents.
    pub text: String,
}

/// Owns all source text for a compilation and maps [`SourceId`]s back to it.
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    /// Creates an empty source map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a file and returns its id.
    pub fn add(&mut self, name: impl Into<String>, text: impl Into<String>) -> SourceId {
        let id = SourceId(u32::try_from(self.files.len()).expect("too many source files"));
        self.files.push(SourceFile {
            name: name.into(),
            text: text.into(),
        });
        id
    }

    /// Looks up a registered file.
    pub fn get(&self, id: SourceId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    /// The text a span covers.
    pub fn snippet(&self, span: Span) -> &str {
        &self.get(span.source).text[span.start as usize..span.end as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_join_and_snippet() {
        let mut map = SourceMap::new();
        let id = map.add("test.as", "trace(\"hello\");");
        let a = Span::new(id, 0, 5);
        let b = Span::new(id, 6, 13);
        assert_eq!(map.snippet(a), "trace");
        assert_eq!(a.to(b), Span::new(id, 0, 13));
    }
}
