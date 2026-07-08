//! Code generation: the [`Backend`] trait and its LLVM implementor.
//!
//! This is the only crate allowed to depend on `inkwell`; no inkwell/LLVM
//! type may appear in this crate's public API or anywhere above it
//! (CLAUDE.md prime directive 3). `unsafe` is permitted here (LLVM FFI) but
//! must stay isolated behind safe wrappers; none is needed yet.

use diagnostics::{Diagnostic, ErrorCode};

pub mod llvm;

/// Options controlling code generation.
#[derive(Debug, Default)]
pub struct CodegenOpts {
    /// LLVM-style target triple; `None` = host.
    pub target_triple: Option<String>,
}

/// A produced relocatable object, ready for linking.
#[derive(Debug)]
pub struct ObjectFile {
    /// Raw object-file bytes.
    pub bytes: Vec<u8>,
}

/// A code-generation backend (SPECS §8).
///
/// The frontend emits [`mir::Program`]; implementors consume it. The LLVM
/// backend is the first implementor; Cranelift/C emission can be added later
/// without touching the frontend.
pub trait Backend {
    /// Compiles a lowered program to a relocatable object file.
    fn compile(
        &self,
        program: &mir::Program,
        opts: &CodegenOpts,
    ) -> Result<ObjectFile, Vec<Diagnostic>>;
}

impl Backend for llvm::LlvmBackend {
    fn compile(
        &self,
        _program: &mir::Program,
        _opts: &CodegenOpts,
    ) -> Result<ObjectFile, Vec<Diagnostic>> {
        Err(vec![Diagnostic::error(
            ErrorCode::NOT_IMPLEMENTED,
            "code generation is not implemented until Phase 3",
        )])
    }
}
