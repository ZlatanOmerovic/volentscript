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

Phases 0–8 done. The language surface: classes/interfaces/inheritance with
native vtable + interface dispatch, reified generics, null safety (`T?`),
closures and Function values, try/catch/finally with a real Error
hierarchy, Array + `Vector.<T>` + dynamic objects (expandos, `in`,
`delete`, object literals), `for..in`/`for each..in`, and a P7 stdlib:
Math, JSON (stringify/parse), Array iteration callbacks
(map/filter/forEach/some/every), `String#replace`, URI encoding, plus the
CLI runtime — `System.args/exit/getenv/time`, `File.read/write/exists`,
`Date.now()`.

P8 added generic functions (`function first.<T>(...)`, monomorphized like
generic classes), the `main():int` program entry (invoked after top-level
statements; its int return becomes the exit status, SPECS §7), and
`is`-guard narrowing (`if (x is Ball) { var b:Ball = x as Ball; }` needs no
`?`). The final golden test is `tests/showcase.as` — the whole language
surface in one program, verified against its embedded expected output.

`vigorscript build tool.as` produces a native binary for real CLI tools
(the P7 milestone golden test is a word-frequency tool with a JSON report).
P9 added the garbage collector (SPECS §7): conservative mark-sweep in
`runtime::gc`, collecting only at backend-emitted safepoints (function
entries and loop headers), with conservative stack + register + static
root scanning, precise tracing of container side-storage, and size-class
block pooling so heavy churn plateaus (a 1.5 GB-churn stress test holds
~34 MB peak RSS). `System.gc()` / `System.gcLiveBytes()` are available;
`VS_GC_LOG=1` prints per-collection stats.

P10 added RegExp (ES3 §15.10) on a backtracking engine (`fancy-regex` —
ES3 needs backreferences and lazy quantifiers): `/pattern/flags` literals
(division disambiguated by the standard prev-token heuristic),
`new RegExp(p, f)`, `test`/`exec` with global `lastIndex`,
`String.match/search/replace` ($&/$n substitutions), `is`/`as RegExp`,
catchable SyntaxError on bad patterns, and GC-integrated regex objects.
Indices are UTF-16 units per the spec. `VS_DUMP_IR=1` dumps the LLVM
module (debugging aid).

P11 added Date instances (ES3 §15.9): `new Date()` / `(millis)` /
`(y, m, d, h, min, s, ms)`, all local + UTC getters, `getTimezoneOffset`,
`setTime`, `Date.UTC`, and the avmplus AS3 string forms
(`toString`/`toDateString`/`toTimeString`/`toUTCString`). Local time via
chrono/the platform tz database (macOS links CoreFoundation). Backlog:
`Date.parse`/string constructor, component setters, locale forms.

P12 added static custom namespaces (ES4 draft, SPECS §5 scope):
`namespace n;` / `namespace n = "uri";` (same URI = same namespace),
namespaced class members (`red function f()`), qualified access
`obj.ns::name` for reads/writes/calls with virtual dispatch, and
`use namespace n` with lexically-scoped open sets and ambiguity
diagnostics. Everything resolves at compile time by folding the
namespace into the member's internal name — zero runtime cost.
Namespace-as-runtime-value (the `Namespace` class) stays backlog.

Remaining (backlog): optimization passes, Linux cross-compile hardening,
runtime Namespace values, sockets. Phase plan: SPECS §11.
