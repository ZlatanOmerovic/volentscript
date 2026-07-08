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

/// Parses one file and returns its AST dump (the CLI `parse` subcommand,
/// the P1 milestone surface).
pub fn parse_dump(input: &Path) -> Result<String, Errors> {
    let (sources, file) = load(input)?;
    let program = parser::parse(&sources, file).map_err(|d| Errors::new(d, &sources))?;
    Ok(ast::dump(&program))
}

/// Result of `check`: warnings (rendered) plus the typed-AST dump.
#[derive(Debug)]
pub struct CheckReport {
    /// Rendered warnings (never errors — those go through [`Errors`]).
    pub warnings: Vec<String>,
    /// Typed-AST dump (`sema::dump`).
    pub dump: String,
}

fn check_program(
    sources: &SourceMap,
    file: SourceId,
) -> Result<(sema::TProgram, Vec<String>), Errors> {
    let program = parser::parse(sources, file).map_err(|d| Errors::new(d, sources))?;
    let outcome = sema::check(&program);
    match outcome.program {
        Some(typed) => {
            let warnings = outcome
                .diagnostics
                .iter()
                .map(|d| d.render_full(sources))
                .collect();
            Ok((typed, warnings))
        }
        None => Err(Errors::new(outcome.diagnostics, sources)),
    }
}

/// Parses and type-checks one file (`check` subcommand — the P2 milestone
/// surface).
pub fn check(input: &Path) -> Result<CheckReport, Errors> {
    let (sources, file) = load(input)?;
    let (typed, warnings) = check_program(&sources, file)?;
    Ok(CheckReport {
        warnings,
        dump: sema::dump(&typed),
    })
}

/// Compiles one program to a native executable.
///
/// P2: parses and type-checks for real, then reports that code generation
/// is Phase 3.
pub fn build(opts: &BuildOptions) -> Result<PathBuf, Errors> {
    let (sources, file) = load(&opts.input)?;
    let (typed, _warnings) = check_program(&sources, file)?;
    let _ = typed; // lower → codegen → link land in P3
    Err(Errors {
        rendered: vec![
            diagnostics::Diagnostic::error(
                diagnostics::ErrorCode::NOT_IMPLEMENTED,
                "code generation is not implemented until Phase 3",
            )
            .render(),
        ],
    })
}
