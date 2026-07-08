//! End-to-end golden-test harness (SPECS §10).
//!
//! Each `.as` program in `programs/` will have an expected stdout and exit
//! code; the harness compiles, links, runs, and compares. Lands in P3 with
//! the first golden test: `trace("hello");` → prints `hello`, exits 0.

#![forbid(unsafe_code)]
