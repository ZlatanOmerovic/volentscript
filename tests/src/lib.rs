//! End-to-end golden-test harness (SPECS §10): each `.vlt` program in
//! `programs/` has an expected stdout in a sibling `.out` file; the harness
//! compiles to a native binary, runs it, and compares stdout + exit code.
//! See `tests/golden.rs`.

#![forbid(unsafe_code)]
