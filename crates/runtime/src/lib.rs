//! The native runtime (SPECS §7), built as a static library and linked into
//! every produced binary.
//!
//! P3 surface: UTF-16 strings, the boxed `*` value, ECMA-262 3rd ed. §9
//! conversions, `trace`/`parseInt`/`parseFloat`/`isNaN`/`isFinite`, String
//! and Number methods, and the C `main` entry shim that calls the compiled
//! script.
//!
//! `unsafe` is confined to the FFI boundary (`ffi` module) and the string
//! allocation; everything else is safe Rust. Memory management: P3
//! intentionally leaks allocations — the GC (SPECS §7, behind a
//! `GcAllocator` trait) is a later-phase deliverable and nothing in P3
//! creates unbounded garbage.
//!
//! Type-mismatch failures that AS3 reports as `TypeError` abort the process
//! with a message until exceptions land in P6.

mod any;
mod closure;
mod conv;
mod exc;
pub(crate) mod ffi;
mod natives;
mod object;
mod seq;
mod string;

pub use any::{Tag, VsAny};
pub use string::VsString;

/// Runtime library version, embedded so produced binaries can report it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
