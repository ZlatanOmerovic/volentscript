//! The native runtime (SPECS §7), built as a static library and linked into
//! every produced binary.
//!
//! Will own object layout, dispatch, boxing, GC (behind a `GcAllocator`
//! trait), coercion helpers, builtins, exceptions, and the C-ABI entry shim.
//! `unsafe` is permitted in this crate (object layout / GC / FFI) but must be
//! isolated behind safe wrappers; none is needed yet. Exported C-ABI symbols
//! and their prefix are a P3 decision.

/// Runtime library version, embedded so produced binaries can report it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
