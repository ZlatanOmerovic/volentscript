//! Driver: orchestrates parse → check → lower → codegen → link (SPECS §8).
//!
//! Also owns platform link details when they land in P3: invoking the system
//! linker with the runtime static lib, and ad-hoc `codesign` of Apple Silicon
//! outputs (CLAUDE.md §3).

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use diagnostics::Diagnostic;
use span::{SourceId, SourceMap};

/// Options for one `build` invocation.
#[derive(Debug)]
pub struct BuildOptions {
    /// The `.as` entry file.
    pub input: PathBuf,
    /// Output executable path; `None` = derive from input.
    pub output: Option<PathBuf>,
}

/// Compilation failure: diagnostics already rendered against the source map
/// (one caret block per diagnostic), ready to print.
#[derive(Debug)]
pub struct Errors {
    /// Rendered diagnostics, in source order.
    pub rendered: Vec<String>,
}

impl Errors {
    fn new(diags: Vec<Diagnostic>, sources: &SourceMap) -> Self {
        Errors {
            rendered: diags.iter().map(|d| d.render_full(sources)).collect(),
        }
    }
}

fn load(input: &Path) -> Result<(SourceMap, SourceId), Errors> {
    let text = std::fs::read_to_string(input).map_err(|e| Errors {
        rendered: vec![format!("error: cannot read `{}`: {e}", input.display())],
    })?;
    let mut sources = SourceMap::new();
    let file = sources.add(input.display().to_string(), text);
    Ok((sources, file))
}

/// Parses one file and returns its AST dump (`asr parse`, the P1 milestone
/// surface).
pub fn parse_dump(input: &Path) -> Result<String, Errors> {
    let (sources, file) = load(input)?;
    let program = parser::parse(&sources, file).map_err(|d| Errors::new(d, &sources))?;
    Ok(ast::dump(&program))
}

/// Compiles one program to a native executable.
///
/// P1: parses for real, then reports that semantic analysis is Phase 2.
pub fn build(opts: &BuildOptions) -> Result<PathBuf, Errors> {
    let (sources, file) = load(&opts.input)?;
    let program = parser::parse(&sources, file).map_err(|d| Errors::new(d, &sources))?;
    let typed = sema::check(program).map_err(|d| Errors::new(d, &sources))?;
    let _ = typed; // lower → codegen → link land in P3
    unreachable!("sema stub always errors before P2");
}
