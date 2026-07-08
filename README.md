# VolentScript

A native, ahead-of-time-compiled revival of ActionScript 3 / ECMAScript 4,
decoupled from Flash. You write `.vlt` source; the `volentscript` compiler
produces a native executable. Written in Rust; LLVM backend via `inkwell`.

- **`SPECS.md`** — the language definition (what to build).
- **`CLAUDE.md`** — process and phase gates (how it gets built).
- **`docs/`** — reference material (ES4 draft, AVM2 overview, avmplus source,
  ECMA-262 3rd ed., AS3 guides). Git-ignored; `docs/SOURCES.md` records the
  set and `links.md` the origins.

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
cargo run -p volentscript -- --version
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
  cli/          the `volentscript` binary
tests/          end-to-end golden tests (.vlt program + expected stdout/exit)
```

Layering: frontend crates never depend on `codegen`; `inkwell` types never
appear above `codegen`. `#![forbid(unsafe_code)]` everywhere except `codegen`
and `runtime`.

## Status: v1 complete (0.1.0)

Seventeen gated phases (P0-P16), each ending green (fmt, clippy
`-D warnings`, full test suite). Everything SPECS §1-§11 mandates for v1
is built and tested.

**Language.** Classes, interfaces, inheritance with native vtable +
interface dispatch; reified generics (classes and functions,
monomorphized); null safety (`T?` with flow narrowing, incl. `&&`/`||`
and `is`-guard narrowing); closures and Function values;
try/catch/finally with a real Error hierarchy (setjmp unwinding);
dynamic classes (expandos, `in`, `delete`, object literals);
`for..in` / `for each..in`; namespaces — both the static layer
(declarations, URI identity, namespaced members, `use namespace`,
compile-time `ns::name`) and first-class `Namespace` values with
runtime-computed qualification via per-class reflection tables;
`main():int` program entry.

**Stdlib (SPECS §6).** String/Array/Vector full surface, Math, JSON,
RegExp (backtracking engine: backreferences, lazy quantifiers, UTF-16
indices), Date (constructors, local+UTC getters, avmplus string forms,
`Date.UTC`), and the CLI runtime: `trace`, `System.args/exit/getenv/
time/gc/readLine`, File IO (`read/write/append/exists/remove/copy/
rename/mkdir/rmdir/list/isDirectory/size/mtime`), blocking TCP sockets
(`Socket.connect`, `ServerSocket.bind/accept`, line-oriented reads).

**Runtime.** Conservative mark-sweep GC (safepoint-triggered,
stack/register/static-root scanning, kind-tagged blocks, size-class
pooling — 1.5 GB-churn stress holds ~34 MB RSS); UTF-16 strings; boxed
`*` values; catchable runtime errors.

**Compiler.** LLVM new-PM optimization pipeline (`-O 0..3`, default O2;
try-functions pinned optnone for setjmp safety; inline ToInt32 fast
paths — recursive int benchmark ~5x over unoptimized); Linux
cross-compilation via `zig cc` (`--target x86_64/aarch64-unknown-linux-gnu`,
full golden corpus byte-identical in Debian containers); ad-hoc
codesigning on macOS arm64; diagnostics with stable codes and caret
rendering throughout.

**Second backend (Cranelift): deferred post-v1** — the v1 exception
scheme needs `returns_twice`, which Cranelift lacks (SPECS §11 DECISION).
The `Backend` trait boundary is verified mechanically: no frontend crate
depends on inkwell.

## Usage

```sh
volentscript build tool.vlt                 # native executable ./tool
volentscript build tool.vlt -O 3 -o fast    # optimization level
volentscript build tool.vlt --target x86_64-unknown-linux-gnu
volentscript run tool.vlt                   # compile + execute
volentscript check tool.vlt                 # type-check only
volentscript parse tool.vlt                 # AST dump
```

Debug aids: `VS_DUMP_IR=1` (pre-optimization LLVM module),
`VS_DUMP_IR_OPT=1` (post-pipeline), `VS_GC_LOG=1` (per-collection stats).

## Tests

`cargo test --workspace` — unit suites per crate plus the e2e golden
corpus in `tests/` (each `.vlt` program with expected stdout + exit 0),
capped by `tests/showcase.vlt`, the whole-language golden test. Opt-in
extras: `cross_linux` (needs zig + docker). Known deviations from AS3/ES4
are documented per phase in the git history and SPECS.

