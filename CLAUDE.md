# CLAUDE.md — Operating Instructions

You are implementing a programming language: a native, AOT-compiled revival of
ActionScript 3 / ECMAScript 4, decoupled from Flash. Written in **Rust**,
backend via **LLVM/`inkwell`**. The full language definition is in **`SPECS.md`**
— read it fully before writing any code, and treat it as the source of truth for
*what* to build. This file governs *how* you work.

---

## 0. Prime directives

1. **Never invent AS3/ES4 semantics from memory.** Every semantic question
   (coercion rules, name resolution, `is`/`as` behavior, default values,
   dispatch, grammar edge cases) is answered by the documents in `docs/`. Look
   it up. When a non-obvious semantic choice is made, cite the source in a code
   comment (e.g. `// coercion per ES4 draft §X / avmplus AvmCore::...`).
2. **Respect the phase gates.** Work proceeds one phase at a time (SPECS §11).
   Do not begin a phase without an explicit **FULL SEND** from the user (§2).
3. **Keep the frontend backend-agnostic.** `inkwell` types must not appear above
   the `codegen` crate. The frontend emits `mir`; backends consume it. This is
   non-negotiable — it's what lets us add Cranelift/C later without a rewrite.
4. **No Flash.** No SWF, ABC, AVM2 emission, or `flash.*`. See SPECS §5. If a
   task seems to require it, stop and raise it.
5. When SPECS and reality conflict, or a §4 "DECISION" needs revisiting, **stop
   and ask** — do not silently pick.

---

## 1. `docs/` — how to use the reference material

The user has downloaded the reference set into `docs/`. Map (full detail in
SPECS §2):

- **`es4lang-Jan06.pdf`** — ES4 draft: **primary** for grammar, type system,
  name resolution, coercion. Your first stop for language semantics.
- **`avm2overview.pdf`** — AVM2 Overview: object model (traits/slots),
  multinames, dispatch, coercion semantics. We emit **no** ABC — read it for
  *semantics*, not for bytecode.
- **AS3 Language Reference + Developer's Guide** — standard-library surface and
  built-in class behavior.
- **Apache Royale AS3 docs** — modern, Flash-decoupled usage & practical
  semantics.
- **`avmplus/` source** — the reference implementation. Tie-breaker when the
  spec is ambiguous: check what the real VM actually does.
- **ECMA-262 3rd ed.** — the ES3 baseline AS3 builds on.

If a needed answer isn't in `docs/`, say so explicitly rather than guessing.

---

## 2. The gate protocol (FULL SEND)

For each phase (SPECS §11):

1. **Propose.** Write a short plan for the phase: what you'll build, which crates
   change, the milestone/demo that ends it, and any open decisions. Keep it
   tight.
2. **Wait.** Do not implement until the user replies **`FULL SEND`** (or explicit
   go-ahead). If they raise changes, revise the plan and re-propose.
3. **Execute.** On FULL SEND, implement the whole phase. You may run with
   `--dangerously-skip-permissions` during execution — the plan *was* the
   review.
4. **Demonstrate.** End the phase by running its milestone (SPECS §11) and
   showing the result (test output / a running binary). Do not declare a phase
   done without its milestone passing.
5. **Stop.** After the milestone, stop and report. Do not roll into the next
   phase without a new FULL SEND.

Mid-phase, if you hit a fork not covered by SPECS (especially a §4 DECISION),
pause and ask rather than choosing silently.

---

## 3. Environment (macOS host, inkwell/LLVM)

Host is macOS on Apple Silicon (also support x86-64). LLVM is **not** Apple's
system toolchain (no `llvm-config`, no static libs there). Use a real LLVM:

```sh
brew install llvm            # keg-only; note the version brew installs
# Pin inkwell's feature flag to that major version, e.g. LLVM 22 -> "llvm22-1"
export LLVM_SYS_221_PREFIX="$(brew --prefix llvm)"   # match the number to the version
```

- **Pin the version.** The inkwell feature flag, `LLVM_SYS_<ver>_PREFIX`, and the
  brew LLVM must all agree. Record the pinned LLVM major + inkwell version in the
  workspace README and `Cargo.toml`. Don't let `brew upgrade` drift it.
- **Linking:** prefer static linking of LLVM for reproducible builds where
  feasible; otherwise handle `@rpath` for the dylib.
- **macOS codesign (Apple Silicon):** every executable the compiler *emits* must
  be at least ad-hoc signed or the kernel refuses to run it. Bake a
  `codesign -s - <output>` step into the link stage of the driver on macOS/arm64.
- **Cross to Linux:** for `x86_64-unknown-linux-gnu`, LLVM codegen is a target-
  triple change, but final linking needs a Linux linker + sysroot (e.g. via
  `zig cc`/`cargo-zigbuild`, or a cross toolchain). Treat turnkey cross-linking
  as a P8 hardening task, not a P3 blocker.

---

## 4. Rust conventions

- Edition 2024. Pin MSRV in `Cargo.toml` and CI (match inkwell's minimum, ≥1.85).
- Cargo **workspace**, crates layered exactly as SPECS §8. Small crates, clear
  layers, no upward dependencies (frontend never depends on `codegen`).
- `#![forbid(unsafe_code)]` in every crate **except** `codegen` (FFI to LLVM) and
  `runtime` (layout/GC/FFI). In those two, isolate `unsafe` behind safe wrappers
  and justify each block with a comment.
- `cargo clippy --all-targets -- -D warnings` must pass. `cargo fmt` enforced.
- Diagnostics from day one: real spans, caret rendering, stable error codes.
  Never `panic!` on user-program errors — panics are for compiler bugs only.
- Tests live with their crate; end-to-end golden tests in `tests/` (SPECS §10).
- Every phase leaves the tree green: builds, clippy-clean, tests pass.

---

## 5. Git / delivery discipline

- One phase = one focused branch/PR-sized unit of work. Descriptive commits.
- Never commit `docs/` binaries as code artifacts if the user prefers them
  git-ignored — confirm once, then respect it. (They are read-only *input*.)
- Keep `SPECS.md` and `CLAUDE.md` updated when a DECISION is resolved or scope
  changes; the docs are living and must not drift from the code.

---

## 6. First action

Do **not** start coding. Begin by:
1. Confirming `docs/` contains the files listed in §1 (report anything missing).
2. Reading `SPECS.md` end to end.
3. Proposing the **Phase 0** plan (scaffold) per §2, and waiting for FULL SEND.

The single most important early milestone is **Phase 3**: `trace("hello");`
compiling to a native binary that prints `hello` and exits 0. Everything before
it is plumbing toward that proof. Optimize your sequencing to reach it cleanly,
then build outward.
