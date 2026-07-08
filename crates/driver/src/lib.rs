//! Driver: orchestrates parse → check → lower → codegen → link (SPECS §8).
//!
//! Also owns platform link details when they land in P3: invoking the system
//! linker with the runtime static lib, and ad-hoc `codesign` of Apple Silicon
//! outputs (CLAUDE.md §3).

#![forbid(unsafe_code)]

use std::path::PathBuf;

use diagnostics::Diagnostic;
use span::SourceMap;

/// Options for one `build` invocation.
#[derive(Debug)]
pub struct BuildOptions {
    /// The `.as` entry file.
    pub input: PathBuf,
    /// Output executable path; `None` = derive from input.
    pub output: Option<PathBuf>,
}

/// Compiles one program to a native executable.
///
/// P0: runs the pipeline as far as it exists — the parser stub reports
/// not-implemented. Each phase extends how far this gets.
pub fn build(opts: &BuildOptions) -> Result<PathBuf, Vec<Diagnostic>> {
    let text = std::fs::read_to_string(&opts.input).map_err(|e| {
        vec![Diagnostic::error(
            diagnostics::ErrorCode::NOT_IMPLEMENTED,
            format!("cannot read `{}`: {e}", opts.input.display()),
        )]
    })?;
    let mut sources = SourceMap::new();
    let file = sources.add(opts.input.display().to_string(), text);
    let program = parser::parse(&sources, file)?;
    let typed = sema::check(program)?;
    let _ = typed; // lower → codegen → link land in P3
    unreachable!("sema stub always errors in P0");
}
