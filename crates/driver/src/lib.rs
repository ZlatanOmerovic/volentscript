//! Driver: orchestrates parse → check → lower → codegen → link (SPECS §8).

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use codegen::Backend as _;
use diagnostics::Diagnostic;
use span::{SourceId, SourceMap};

pub use codegen::OptLevel;

/// Options for one `build` invocation.
#[derive(Debug, Default)]
pub struct BuildOptions {
    /// The `.vlt` entry file.
    pub input: PathBuf,
    /// Output executable path; `None` = input filename without extension.
    pub output: Option<PathBuf>,
    /// Path to the runtime static library; `None` = `libruntime.a` next to
    /// the compiler executable (where a workspace build puts it).
    pub runtime_lib: Option<PathBuf>,
    /// Optimization level (default O2).
    pub opt: codegen::OptLevel,
    /// Cross-compilation target triple (e.g. `x86_64-unknown-linux-gnu`);
    /// `None` = host. Linux targets link with `zig cc` (CLAUDE.md §3).
    pub target: Option<String>,
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

    fn message(msg: impl Into<String>) -> Self {
        Errors {
            rendered: vec![format!("error: {}", msg.into())],
        }
    }
}

fn load(input: &Path) -> Result<(SourceMap, SourceId), Errors> {
    let text = std::fs::read_to_string(input)
        .map_err(|e| Errors::message(format!("cannot read `{}`: {e}", input.display())))?;
    let mut sources = SourceMap::new();
    let file = sources.add(input.display().to_string(), text);
    Ok((sources, file))
}

/// Parses the user file with the prelude spliced in front.
fn parse_with_prelude(
    sources: &mut SourceMap,
    file: SourceId,
) -> Result<ast::Program, Vec<diagnostics::Diagnostic>> {
    let prelude_id = sources.add("<prelude>", PRELUDE.to_string());
    let prelude = parser::parse(sources, prelude_id)?;
    let mut program = parser::parse(sources, file)?;
    let mut directives = prelude.directives;
    directives.append(&mut program.directives);
    Ok(ast::Program {
        directives,
        span: program.span,
    })
}

/// Result of `check`: warnings (rendered) plus the typed-AST dump.
#[derive(Debug)]
pub struct CheckReport {
    /// Rendered warnings (never errors — those go through [`Errors`]).
    pub warnings: Vec<String>,
    /// Typed-AST dump (`sema::dump`).
    pub dump: String,
}

/// The Error hierarchy (SPECS §6 P6), compiled into every program. The
/// field layout (message, name, errorID = slots 0..2) is an ABI contract
/// with runtime/src/exc.rs — internal faults are built from it.
const PRELUDE: &str = r#"
public class Error {
    public var message:String?;
    public var name:String? = "Error";
    public var errorID:int;
    public function Error(message:* = "", id:int = 0) {
        this.message = "" + message;
        errorID = id;
    }
    public function toString():String {
        var n:String = name == null ? "Error" : "" + name;
        if (message == null || message == "")
            return n;
        return n + ": " + message;
    }
}
public class TypeError extends Error {
    public function TypeError(m:* = "", id:int = 0) { super(m, id); name = "TypeError"; }
}
public class RangeError extends Error {
    public function RangeError(m:* = "", id:int = 0) { super(m, id); name = "RangeError"; }
}
public class ReferenceError extends Error {
    public function ReferenceError(m:* = "", id:int = 0) { super(m, id); name = "ReferenceError"; }
}
public class ArgumentError extends Error {
    public function ArgumentError(m:* = "", id:int = 0) { super(m, id); name = "ArgumentError"; }
}
public class SyntaxError extends Error {
    public function SyntaxError(m:* = "", id:int = 0) { super(m, id); name = "SyntaxError"; }
}
"#;

fn check_program(
    sources: &mut SourceMap,
    file: SourceId,
) -> Result<(sema::TProgram, Vec<String>), Errors> {
    let program = parse_with_prelude(sources, file).map_err(|d| Errors::new(d, sources))?;
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

/// Parses one file and returns its AST dump (the CLI `parse` subcommand,
/// the P1 milestone surface).
pub fn parse_dump(input: &Path) -> Result<String, Errors> {
    let (sources, file) = load(input)?;
    let program = parser::parse(&sources, file).map_err(|d| Errors::new(d, &sources))?;
    Ok(ast::dump(&program))
}

/// Parses and type-checks one file (`check` subcommand — the P2 milestone
/// surface).
pub fn check(input: &Path) -> Result<CheckReport, Errors> {
    let (mut sources, file) = load(input)?;
    let (typed, warnings) = check_program(&mut sources, file)?;
    Ok(CheckReport {
        warnings,
        dump: sema::dump(&typed),
    })
}

/// Compiles one program to a native executable: parse → check → lower →
/// LLVM codegen → link against the runtime static lib → ad-hoc codesign
/// (macOS arm64, CLAUDE.md §3).
pub fn build(opts: &BuildOptions) -> Result<PathBuf, Errors> {
    let (mut sources, file) = load(&opts.input)?;
    let (typed, warnings) = check_program(&mut sources, file)?;
    for w in &warnings {
        eprintln!("{w}");
    }
    let program = mir::lower(&typed).map_err(|d| Errors::new(d, &sources))?;

    let backend = codegen::llvm::LlvmBackend::default();
    let object = backend
        .compile(
            &program,
            &codegen::CodegenOpts {
                opt: opts.opt,
                target_triple: opts.target.clone(),
            },
        )
        .map_err(|d| Errors::new(d, &sources))?;

    let output = opts.output.clone().unwrap_or_else(|| {
        let stem = opts.input.file_stem().unwrap_or_default();
        opts.input.with_file_name(stem)
    });
    let obj_path = output.with_extension("o");
    std::fs::write(&obj_path, &object.bytes)
        .map_err(|e| Errors::message(format!("cannot write `{}`: {e}", obj_path.display())))?;

    let runtime_lib = find_runtime_lib(opts)?;
    let mut link_cmd = match zig_target(opts.target.as_deref())? {
        // Cross link: zig ships linker + libc sysroots for every target
        // (CLAUDE.md §3 hardening path).
        Some(zt) => {
            let mut c = Command::new("zig");
            // -lunwind: the Rust runtime staticlib carries unwind
            // references (_Unwind_Resume); zig bundles LLVM libunwind.
            c.args(["cc", "-target", &zt, "-lunwind"]);
            c
        }
        None => Command::new("cc"),
    };
    link_cmd
        .arg(&obj_path)
        .arg(&runtime_lib)
        .arg("-o")
        .arg(&output);
    // The runtime's timezone lookup (chrono → iana-time-zone) uses
    // CoreFoundation on macOS (host links only).
    if opts.target.is_none() && cfg!(target_os = "macos") {
        link_cmd.args(["-framework", "CoreFoundation"]);
    }
    let link = link_cmd.output().map_err(|e| {
        Errors::message(format!(
            "cannot run linker `{}`: {e}",
            if opts.target.is_some() { "zig" } else { "cc" }
        ))
    })?;
    let _ = std::fs::remove_file(&obj_path);
    if !link.status.success() {
        return Err(Errors::message(format!(
            "linking failed:\n{}",
            String::from_utf8_lossy(&link.stderr)
        )));
    }

    // Apple Silicon refuses unsigned binaries — ad-hoc sign (CLAUDE.md §3).
    if opts.target.is_none() && cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        let sign = Command::new("codesign")
            .args(["-s", "-", "--force"])
            .arg(&output)
            .output()
            .map_err(|e| Errors::message(format!("cannot run codesign: {e}")))?;
        if !sign.status.success() {
            return Err(Errors::message(format!(
                "codesign failed:\n{}",
                String::from_utf8_lossy(&sign.stderr)
            )));
        }
    }
    Ok(output)
}

/// Builds and immediately runs; returns the program's exit code.
pub fn run(opts: &BuildOptions) -> Result<i32, Errors> {
    let exe = build(opts)?;
    let exe = if exe.is_absolute() {
        exe
    } else {
        Path::new(".").join(exe)
    };
    let status = Command::new(&exe)
        .status()
        .map_err(|e| Errors::message(format!("cannot run `{}`: {e}", exe.display())))?;
    Ok(status.code().unwrap_or(1))
}

/// Maps a Rust-style target triple to zig's spelling; `None` input = host
/// (no zig). Unknown triples are an error naming the supported set.
fn zig_target(target: Option<&str>) -> Result<Option<String>, Errors> {
    let Some(t) = target else { return Ok(None) };
    match t {
        "x86_64-unknown-linux-gnu" => Ok(Some("x86_64-linux-gnu".to_string())),
        "aarch64-unknown-linux-gnu" => Ok(Some("aarch64-linux-gnu".to_string())),
        other => Err(Errors::message(format!(
            "unsupported target `{other}` — supported: x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu"
        ))),
    }
}

fn find_runtime_lib(opts: &BuildOptions) -> Result<PathBuf, Errors> {
    if let Some(p) = &opts.runtime_lib {
        if p.exists() {
            return Ok(p.clone());
        }
        return Err(Errors::message(format!(
            "runtime library not found at `{}`",
            p.display()
        )));
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    // Cross targets: the workspace puts the target's runtime at
    // target/<triple>/{release,debug}/libruntime.a (built with
    // `cargo build -p runtime --target <triple> --release`).
    if let (Some(t), Some(dir)) = (&opts.target, &exe_dir) {
        if let Some(target_root) = dir.parent() {
            for profile in ["release", "debug"] {
                let candidate = target_root.join(t).join(profile).join("libruntime.a");
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
        }
        return Err(Errors::message(format!(
            "no runtime for `{t}` — build it with `cargo build -p runtime --target {t} --release` or pass --runtime-lib"
        )));
    }
    if let Some(dir) = exe_dir {
        let candidate = dir.join("libruntime.a");
        if candidate.exists() {
            return Ok(candidate);
        }
        // Test binaries live one level down (target/<profile>/deps/).
        if let Some(parent) = dir.parent() {
            let candidate = parent.join("libruntime.a");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Err(Errors::message(
        "cannot locate libruntime.a next to the compiler; pass --runtime-lib",
    ))
}
