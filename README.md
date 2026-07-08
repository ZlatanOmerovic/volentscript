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

Phases 0–7 done. The language surface: classes/interfaces/inheritance with
native vtable + interface dispatch, reified generics, null safety (`T?`),
closures and Function values, try/catch/finally with a real Error
hierarchy, Array + `Vector.<T>` + dynamic objects (expandos, `in`,
`delete`, object literals), `for..in`/`for each..in`, and a P7 stdlib:
Math, JSON (stringify/parse), Array iteration callbacks
(map/filter/forEach/some/every), `String#replace`, URI encoding, plus the
CLI runtime — `System.args/exit/getenv/time`, `File.read/write/exists`,
`Date.now()`.

`vigorscript build tool.as` produces a native binary for real CLI tools
(the milestone golden test is a word-frequency tool with a JSON report).
Remaining (P8+): RegExp, Date instances, generic functions, runtime
namespaces, GC (currently leak-only), optimization passes, Linux
cross-compile hardening. Phase plan: SPECS §11.
