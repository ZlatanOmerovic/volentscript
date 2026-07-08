# VigorScript

A native, ahead-of-time-compiled revival of ActionScript 3 / ECMAScript 4,
decoupled from Flash. You write `.as` source; the `vigorscript` compiler
produces a native executable. Written in Rust; LLVM backend via `inkwell`.

- **`SPECS.md`** — the language definition (what to build).
- **`CLAUDE.md`** — process and phase gates (how it gets built).
- **`docs/`** — reference material (ES4 draft, AVM2 overview, avmplus source,
  ECMA-262 3rd ed., AS3 guides). Git-ignored; `docs/SOURCES.md` records the
  set and `links.md` the origins.

> The name **VigorScript** appears only in the `cli` crate, this README, and
> the SPECS header (SPECS §12).

## Toolchain (pinned)

| Component | Pin | Where enforced |
|---|---|---|
| LLVM | **major 22** (brew `llvm`, tested 22.1.6) | `.cargo/config.toml`, CI version check |
| inkwell | **0.9.x**, feature **`llvm22-1`** (`llvm-sys` 221) | root `Cargo.toml` |
| Rust | edition 2024, **MSRV 1.85** | `rust-version` in root `Cargo.toml` |

These three must move together. Don't let `brew upgrade` drift the LLVM major
without updating the inkwell feature, the `LLVM_SYS_221_PREFIX` variable name,
and the CI check in the same change.

## Building

```sh
brew install llvm        # keg-only; major must be 22
cargo build --workspace
cargo test --workspace
cargo run -p vigorscript -- --version
```

`.cargo/config.toml` sets `LLVM_SYS_221_PREFIX=/opt/homebrew/opt/llvm`
(Apple Silicon default). It does not override an existing value — on Intel
macs or CI, export `LLVM_SYS_221_PREFIX="$(brew --prefix llvm)"` yourself.

## Workspace layout

```
crates/
  span/         source positions, spans, source map
  diagnostics/  error type, stable codes, rendering
  lexer/        &str -> tokens
  ast/          AST node types
  parser/       tokens -> AST (recursive descent, ES4 grammar)
  sema/         resolution, type checking, coercions -> typed AST
  mir/          typed, desugared, backend-agnostic mid-level IR
  codegen/      Backend trait + LLVM (inkwell) impl — only crate touching inkwell
  runtime/      native runtime static lib (GC, dispatch, builtins, entry shim)
  driver/       parse -> check -> lower -> codegen -> link orchestration
  cli/          the `vigorscript` binary
tests/          end-to-end golden tests (.as program + expected stdout/exit)
```

Layering: frontend crates never depend on `codegen`; `inkwell` types never
appear above `codegen`. `#![forbid(unsafe_code)]` everywhere except `codegen`
and `runtime`.

## Status

Phases 0–6 done: scaffold + CI (P0); lexer + parser (P1); sema with typed
AST (P2); native compilation (P3); classes/interfaces/inheritance (P4);
reified generics + null safety + Array + Vector.<T> (P5); **closures,
exceptions, remaining control flow** (P6) — closure conversion with heap
cells and multi-level capture, Function values (function expressions,
bound method closures per SPECS §3.7, call/apply, sort comparators),
try/catch/finally on a documented setjmp scheme with typed catch dispatch
and finally on every exit path, the Error hierarchy as a compiled-in
prelude (runtime TypeError/RangeError are catchable objects), and
for..in / for each..in over Arrays and Vectors.

`vigorscript run file.as` compiles and executes native binaries for
closure- and exception-heavy programs. Remaining gates: dynamic property
access and stdlib breadth (P7); generic functions, namespaces, optimizing
passes (P8). Phase plan: SPECS §11.
