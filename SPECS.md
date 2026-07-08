# SPECS.md — Language Specification

> **Name:** `VigorScript`. CLI tool: `vigorscript`. Source extension: `.as`.
> The name appears in exactly the places listed in §12. Do not hard-code it
> anywhere else. (Former working name: `AS3R`/`asr` — renamed 2026-07-08;
> note `asr` collides with macOS `/usr/bin/asr`, avoid reintroducing it.)

This document defines the language, its type system, and the compiler pipeline.
It is the source of truth for *what* to build. `CLAUDE.md` defines *how* to
build it (process, phases, gates). Read both before writing code.

---

## 0. What this is and is not

**Is:** a statically-typed, garbage-collected, ahead-of-time-compiled
programming language that revives the ActionScript 3 / ECMAScript-4 object model
and type system, decoupled entirely from Flash. You write `.as` source, the
compiler produces a native executable.

**Is not:** a Flash runtime. There is **no** SWF, no ABC bytecode, no AVM2, no
`flash.*` API, no display list, no timeline, no `Sprite`/`MovieClip`/`Stage`, no
E4X-in-v1. We take the *language* and give it a native backend it never had.

The guiding principle for every ambiguous decision: **keep what AS3 got right,
fix what it got wrong, drop what was Flash.** The three sections §3–§5 make each
of those explicit.

---

## 1. Target & toolchain

- **Implementation language:** Rust (edition 2024, MSRV pinned in `CLAUDE.md`).
- **Primary backend:** LLVM via `inkwell` 0.9.x (`llvm22-1` feature or whatever
  matches the pinned LLVM — see `CLAUDE.md` §env). Backend sits behind a
  `Backend` trait (§8) so a second backend (Cranelift, C emission) can be added
  without touching the frontend.
- **Output:** native object file → linked executable. Development host is macOS
  (Apple Silicon assumed; support x86-64 too). First-class cross target:
  `x86_64-unknown-linux-gnu` (deploy to Linux servers).
- **Runtime:** a small Rust-authored native runtime library, statically linked
  into every produced binary (§7).

---

## 2. Reference documents (`docs/`)

All specification questions are answered by the downloaded documents in `docs/`.
**Never invent AS3/ES4 semantics from memory.** When a semantic decision arises,
consult the mapped document and, if it matters, cite it in a code comment.

| Question you have | Authoritative source in `docs/` |
|---|---|
| Grammar, type system, name resolution, coercion **semantics** | ES4 draft spec (`es4lang-Jan06.pdf`) — **primary language reference** |
| Object model: traits/slots, multinames, dispatch, verification-era semantics | AVM2 Overview (`avm2overview.pdf`) — semantics only, we emit **no** ABC |
| Standard-library surface & built-in class behavior | AS3 Language Reference + AS3 Developer's Guide (PDF / Markdown) |
| Modern, Flash-decoupled AS3 usage & practical semantics | Apache Royale AS3 docs |
| "What does the reference implementation *actually* do here?" | `avmplus` source (tie-breaker when the spec is ambiguous) |
| ECMAScript baseline (ES3 that AS3 sits on top of) | ECMA-262 3rd ed. |

Precedence when sources conflict: **this SPECS.md** > deliberate decisions in §3–§5 >
ES4 draft > AVM2 Overview > `avmplus` behavior > AS3 reference docs. If a
conflict can't be resolved, stop and raise it (see `CLAUDE.md` gates).

---

## 3. KEEP — AS3 features we implement faithfully

These are the reasons the language is worth reviving. Implement them with AS3
semantics unless §4 explicitly overrides.

### 3.1 Reified nominal types
Types exist at runtime and carry identity. `is` performs a real runtime type
test against actual class identity; `as` performs a checked downcast returning
`null` on failure (not a compile-time no-op). Type information travels with the
value. This is the property that makes efficient AOT codegen possible — lean on
it.

### 3.2 Sealed-by-default object model
A `class` has a fixed shape ("traits": a fixed set of typed slots and methods),
laid out as a struct with a vtable. Instances **cannot** gain arbitrary
properties. `dynamic class` opts an instance into expando behavior (a backing
property map). Sealed is the default; dynamic is the exception.

### 3.3 Numeric primitives
Distinct primitive types, not one float:
- `int` — 32-bit signed integer.
- `uint` — 32-bit unsigned integer.
- `Number` — IEEE-754 double (float64).
- `Boolean` — true/false.
- `String` — immutable UTF-16 sequence (AS3 semantics; see §4.4 note).

Implement AS3's numeric coercion rules exactly (int↔uint↔Number, `NaN`,
`Infinity`, integer wraparound, `Number`→`int` truncation via ToInt32). These
rules are in the ES4 draft and ECMA-262 §9; verify against `avmplus`.

### 3.4 Classes & single inheritance
`class C extends B implements I, J`. Single class inheritance, multiple interface
implementation. Members: `var` (field), `const`, `function` (method),
`function get`/`function set` (accessors), `static` members, constructors.
Modifiers: `public`, `private`, `protected`, `internal`, `final`, `override`
(**mandatory** when overriding — enforce it), `static`, `dynamic` (class-level).

### 3.5 Interfaces
`interface I extends J, K`. Method and accessor signatures only. Multiple
interface inheritance. A class implementing an interface must satisfy the full
signature set (checked).

### 3.6 Packages & access control
`package a.b.c { ... }`. Fully-qualified names. `internal` = package-visible.
`import a.b.C`. Top-level (package-less) definitions live in a default package.

### 3.7 Functions as values, closures, method closures
First-class functions, nested functions, closures capturing lexical environment.
**Method closures bind `this` correctly** — extracting `obj.method` yields a
closure permanently bound to `obj`. No `this`-loss footgun.

### 3.8 Control flow & statements
`if/else`, `for`, `for..in` (keys), `for each..in` (values), `while`,
`do..while`, `switch/case/default` (with fall-through), `break`/`continue` (incl.
labeled), `return`, `throw`, `try/catch/finally`. `default xml namespace` is
dropped (E4X).

### 3.9 Operators
Arithmetic, bitwise, logical, comparison, assignment, compound-assignment,
ternary `?:`, `typeof`, `is`, `as`, `in`, `delete`, `void`, comma. `instanceof`
exists but is deprecated in favor of `is` (parse it, warn). String `+`
concatenation with coercion.

### 3.10 Core built-in classes (language-level, non-Flash)
`Object`, `Class`, `Function`, `Array` (dense/sparse, dynamic length),
`Vector.<T>` (typed, dense — but see §4.3), `String`, `Boolean`, `Number`,
`int`, `uint`, `Math` (namespace of statics), `Date`, `RegExp`, `Error` and its
subclasses (`TypeError`, `RangeError`, `ReferenceError`, `ArgumentError`,
etc.), `JSON` (parse/stringify). Top-level functions: `trace` (→ stdout +
newline), `parseInt`, `parseFloat`, `isNaN`, `isFinite`, `encodeURIComponent`,
etc. The full surface is enumerated in §6 with a phase tag.

### 3.11 Special types
`void` (no value / statement expression type), `null` (the null literal &
bottom of reference types), `*` (the "any"/untyped type — dynamic typing escape
hatch, AS3's `*`), `undefined` (only meaningful on `*`/`Object`-untyped and
dynamic props; on typed slots the default is type-specific).

Default values (AS3 semantics): `int`/`uint` → 0, `Number` → `NaN`, `Boolean` →
`false`, `String` → `null`, `*` → `undefined`, other reference types → `null`
(but see §4.1 null-safety override).

---

## 4. FIX — where "properly" means departing from AS3

The user's mandate is to build AS3 *properly*, not to reproduce its mistakes.
Each of these is a deliberate, opt-out-able decision. If any should be reverted
to literal AS3 behavior, that's a gate decision (`CLAUDE.md`) — flag it, don't
silently choose.

### 4.1 Null safety **[DECISION — default: ON]**
Reference types are **non-nullable by default**. `T` cannot hold `null`; `T?`
can. Assigning/returning `null` where a non-nullable `T` is expected is a
compile error. Dereferencing a `T?` without narrowing is a compile error. This
is the single biggest departure from AS3 (which was freely nullable) and the ES4
drafts already sketched `T?` — we finish what they started.
- Migration/escape hatch: `*` remains freely nullable and untyped.
- If the user vetoes: fall back to AS3's nullable-everywhere and treat `?` as a
  no-op annotation.

### 4.2 Reified generics **[DECISION — default: ON]**
AS3 promised parametric types and only ever shipped the special-cased
`Vector.<T>`. We implement **real user-definable generics**: `class Box.<T>`,
`function map.<T,R>(...)`, generic interfaces. Type parameters are **reified**
(available at runtime, consistent with §3.1), not erased like Java/TS. Decide
monomorphization vs. uniform representation in §8 (default: monomorphize value-
typed instantiations, uniform-box reference-typed — document the boundary).

### 4.3 `Vector.<T>` becomes sugar
With §4.2, `Vector.<T>` is no longer a VM special case; it's a library generic
`Vector.<T>` over the same machinery. Keep the surface syntax `Vector.<T>` and
`new <T>[...]` literals for familiarity.

### 4.4 Strings
Keep AS3-observable string semantics (indexing, `length`, methods). Internal
encoding may be UTF-8 with a UTF-16-compatible API surface **only if** all
observable behavior (`.length`, `charCodeAt`, surrogate handling) matches AS3.
Default to UTF-16 storage to avoid semantic drift; revisit as an optimization.

### 4.5 Nominal, not structural **[DECISION — default: nominal]**
Type compatibility is **nominal** (by declared name/identity), matching AS3 and
easing codegen. We do **not** adopt TS-style structural typing. Interfaces are
the mechanism for polymorphism. (This was called out as the single biggest fork;
it is decided here as nominal. Veto = gate decision.)

### 4.6 Modern conveniences (additive, non-breaking)
Permitted because they don't conflict with AS3 semantics: nullish-coalescing
`??` and optional-chaining `?.` (natural companions to §4.1), block-scoped
`const`/`let`-equivalent (AS3 `var` is function-scoped; adding block scope is
allowed but must be a **new** keyword or opt-in to avoid changing `var`). Keep
these OFF until Phase 6+; don't let them distract from the core.

---

## 5. DROP — Flash / out-of-scope for v1

Not implemented (v1), and not part of the language proper:
- SWF, ABC, AVM2 bytecode emission, `flash.*`, display list, timeline, events
  framework (`flash.events.*`), `Stage3D`, `MovieClip`, `Sprite`.
- E4X / XML literals (`<foo/>` as syntax) — **deferred**, optional later phase.
  `XML`/`XMLList` as *classes* may come back as a library, but XML-as-syntax is
  not a v1 goal.
- Runtime namespaces as first-class *values* (custom `namespace` objects,
  `ns::name` runtime qualification) — **deferred to Phase 8**. v1 uses
  namespaces only for the fixed access-control set (`public`/`private`/etc.) and
  package qualification.
- `Proxy`, `flash.utils.*`, AMF, `ByteArray`-as-Flash-API (a plain byte buffer
  type may exist in the runtime, but not the Flash `ByteArray` surface).

---

## 6. Standard library surface (enumerated, phase-tagged)

Phase tags: **P3** = needed for first runnable binary, **P4** = classes/OOP,
**P5** = generics/collections, **P7** = stdlib breadth. Implement in the runtime
(§7) with `.as` declaration stubs (`intrinsic`/`native` bindings).

- **Top-level functions:** `trace` (P3), `parseInt`, `parseFloat`, `isNaN`,
  `isFinite` (P3); `encodeURIComponent`, `decodeURIComponent`, `escape`,
  `unescape` (P7).
- **`Object`** (P4): base of the reference hierarchy; `toString`, `hasOwnProperty`,
  `valueOf`, prototype-less (sealed) by default.
- **`int`/`uint`/`Number`** (P3): boxing, `toString(radix)`, `toFixed`,
  `MIN_VALUE`/`MAX_VALUE`, `NaN`, `POSITIVE_INFINITY`, etc.
- **`Boolean`** (P3), **`String`** (P3 core methods: `length`, `charAt`,
  `charCodeAt`, `indexOf`, `substr`, `substring`, `slice`, `split`, `toUpperCase`,
  `toLowerCase`, `replace` (P7 w/ regex), `concat`).
- **`Array`** (P5): `length`, `push`, `pop`, `shift`, `unshift`, `slice`,
  `splice`, `indexOf`, `concat`, `join`, `sort`, `map`/`filter`/`forEach`/`some`/
  `every` (P7), `reverse`.
- **`Vector.<T>`** (P5): typed, dense, same method surface as `Array` where
  meaningful.
- **`Math`** (P7): full static surface.
- **`Date`** (P7: `Date.now()`; instances are backlog), **`RegExp`** (P10:
  `fancy-regex`-backed; lastIndex is read-only and `split(RegExp)` is
  backlog), **`JSON`** (P7).
- **`Error`** hierarchy (P6): `Error`, `TypeError`, `RangeError`,
  `ReferenceError`, `ArgumentError`, `SyntaxError`, `VerifyError`(drop),
  custom user `Error` subclasses.
- **`Function`** (P4): `.length`, `.call`, `.apply`.
- **I/O (non-Flash, new):** minimal CLI runtime — `print`/`trace` to stdout,
  process args, exit code, env, file read/write, sockets. This is the
  Redtamarin-shaped surface that makes it a *usable* language. Spec the API in a
  later doc; **P7+**. Do not model it on `flash.*`.

---

## 7. Runtime library (Rust, statically linked)

A `runtime` crate compiled to a static lib and linked into every produced
binary. Responsibilities:
- **Object layout & metadata:** class descriptors (vtable, slot map, type id for
  `is`/`as`), instance headers.
- **Dispatch:** vtable dispatch for sealed classes; hashed property lookup for
  `dynamic` and `*`.
- **Boxing:** primitive ↔ `Object`/`*` boxing where AS3 semantics require it.
- **Memory management / GC:** AS3 is garbage-collected with no manual free.
  - **v1 (default):** a simple precise or conservative tracing collector, or
    reference counting **with a cycle collector**. Start with the simplest
    *correct* option (conservative mark/sweep, e.g. a bdwgc-style collector or a
    hand-rolled mark/sweep) behind an allocator interface. Cyclic garbage
    **must** be collected — plain `Rc` alone is not acceptable as the final
    answer.
  - **Implemented (P9):** hand-rolled conservative mark-sweep in
    `runtime::gc` — safepoint-triggered (function entries + loop headers),
    stack/register/static-root scanning, kind-tagged blocks with size-class
    pooling. It is a module boundary rather than a `GcAllocator` trait;
    swapping collectors means swapping the module (revisit if a second
    collector lands).
- **Runtime type support:** `is`, `as`, `instanceof`, `typeof`, class-of.
- **Coercion helpers:** the numeric/string coercion rules from §3.3.
- **Builtins:** implementations backing the §6 stdlib, bound to `.as`
  `native`-declared signatures.
- **Exceptions:** unwinding for `try/catch/finally` and `throw` (use LLVM's
  landing pads / Itanium unwinding, or a simpler setjmp-style scheme in v1 —
  document the choice).
- **Entry point:** C-ABI `main` shim that initializes the runtime/GC, invokes the
  program's top-level/`main`, flushes stdout, returns the exit code.

---

## 8. Compiler pipeline & architecture

Cargo workspace. Crates (names are guidance; keep them small and layered):

```
crates/
  span/        # source positions, spans, source map
  diagnostics/ # error type, rendering (carets, colors), error codes
  lexer/       # &str -> token stream
  ast/         # AST node types + visitor
  parser/      # tokens -> AST (recursive descent; AS3 grammar per ES4 draft)
  sema/        # name/package/namespace resolution, type checking,
               # coercion insertion, override/interface checking -> typed AST
  hir/ or mir/ # typed, desugared mid-level IR (own IR, backend-agnostic)
  codegen/     # Backend trait + LLVM impl (inkwell). One module per backend.
  runtime/     # the native runtime (§7), built as a static lib
  driver/      # orchestration: parse->check->lower->codegen->link
  cli/         # the `asr` binary (arg parsing, subcommands)
tests/         # end-to-end .as programs + expected output
docs/          # the downloaded reference PDFs/repos (read-only input)
```

Pipeline stages:
1. **Lex** → tokens (with spans).
2. **Parse** → AST. Grammar follows the ES4 draft; verify tricky productions
   (`for each`, `is`/`as` precedence, `.<T>` type args, `E4X`—skip) against the
   draft and `avmplus` parser.
3. **Resolve** → bind names, packages, imports, namespaces; build symbol tables.
4. **Type-check** → nominal type checking (§4.5), null-safety (§4.1), generics
   (§4.2), coercion insertion, mandatory-`override` and interface-conformance
   checks. Emit typed IR.
5. **Lower** → own mid-level IR (`mir`): desugar `for each`, accessors, closures
   (closure conversion), generics instantiation, coercions made explicit.
6. **Codegen** (behind `Backend` trait) → LLVM IR via inkwell → object file.
7. **Link** → invoke system linker (macOS `ld`/`ld-prime`; ad-hoc codesign the
   Apple-Silicon output — see `CLAUDE.md` §macos) with the runtime static lib →
   executable.

**`Backend` trait** (the reason inkwell isn't load-bearing): the frontend emits
`mir`; `codegen` consumes `mir`. `trait Backend { fn compile(&self, program:
&Mir, opts: &CodegenOpts) -> Result<ObjectFile>; }`. The inkwell backend is the
first (and only, for now) implementor. Do not leak `inkwell` types above the
`codegen` crate.

---

## 9. Grammar (orientation, not complete)

Authoritative grammar = ES4 draft (`docs/es4lang-Jan06.pdf`). Sketch so Claude
Code knows the shape it's parsing:

```
program        := packageDecl* topLevel*
packageDecl    := 'package' qualifiedName? '{' directive* '}'
directive      := importDecl | classDecl | interfaceDecl | funcDecl | varDecl | stmt
classDecl      := attr* 'class' Ident typeParams? ('extends' typeRef)?
                  ('implements' typeRef (',' typeRef)*)? '{' member* '}'
interfaceDecl  := attr* 'interface' Ident typeParams? ('extends' typeRef (',' typeRef)*)? '{' sig* '}'
member         := attr* (varDecl | constDecl | funcDecl | accessorDecl)
funcDecl       := 'function' Ident typeParams? '(' params? ')' (':' typeRef)? block
accessorDecl   := 'function' ('get'|'set') Ident '(' params? ')' (':' typeRef)? block
attr           := 'public'|'private'|'protected'|'internal'|'static'|'final'|'override'|'dynamic'| Ident /*ns*/
typeRef        := qualifiedName ('.<' typeRef (',' typeRef)* '>')? '?'?     // '?' = nullable (§4.1)
param          := Ident ':' typeRef ('=' expr)?  |  '...' Ident
stmt           := ifStmt | forStmt | forEachStmt | whileStmt | doWhile | switchStmt
                | tryStmt | throwStmt | returnStmt | breakStmt | continueStmt | block | exprStmt | varDecl
expr           := assignment ; with full AS3 operator precedence incl. 'is' 'as' 'in' 'instanceof'
```

---

## 10. Testing strategy

- **Unit tests** per crate (lexer tokens, parser AST snapshots, type-checker
  accept/reject cases).
- **End-to-end** golden tests in `tests/`: each `.as` file has an expected
  stdout and expected exit code; the harness compiles, links, runs, compares.
- The **first golden test** (Phase 3 milestone): `trace("hello");` compiles to a
  binary that prints `hello\n` and exits 0.
- **Conformance corpus:** as features land, add small programs that exercise AS3
  semantics (coercions, `is`/`as`, override rules, closure `this`-binding) with
  behavior verified against the spec / `avmplus`.
- Type-checker **negative** tests are as important as positive: null-safety
  violations, missing `override`, interface non-conformance, bad coercions must
  produce diagnostics with correct spans and error codes.

---

## 11. Phase plan (gated — see `CLAUDE.md` for the gate protocol)

Each phase ends at a demonstrable milestone. Do not start a phase without a
FULL SEND.

- **P0 — Scaffold.** Workspace, crates, CLI skeleton (`asr build/run`), CI,
  clippy, `docs/` wired, this spec committed. Milestone: `asr --version` runs.
- **P1 — Lex + Parse (core).** Functions, primitives, expressions, statements,
  `trace`. Milestone: parse a core `.as` file to an AST snapshot.
- **P2 — Sema (core).** Resolution + type checking + coercions for the core
  subset (no classes yet). Milestone: type errors with spans on a test corpus.
- **P3 — Codegen (core) → FIRST BINARY.** `mir` + inkwell backend + runtime
  entry + link + macOS codesign. Milestone: `trace("hello")` → running binary.
  **This proves the whole toolchain end to end. It is the most important gate.**
- **P4 — Classes, interfaces, inheritance.** Slots, vtables, dispatch,
  constructors, accessors, `override`, `is`/`as`, `Object`/`Function`.
  Milestone: OOP program with polymorphism runs.
- **P5 — Full type system.** Generics (§4.2), null safety (§4.1), `Array`,
  `Vector.<T>`. Milestone: generic collection program runs and type-checks.
- **P6 — Closures, exceptions, remaining control flow.** Closure conversion,
  `try/catch/finally`, `Error` hierarchy, labeled break/continue, `for each`.
  Milestone: exception-handling + closures program runs.
- **P7 — Stdlib breadth.** `Math`, `Date`, `String`/`Array` full surface, `JSON`,
  `RegExp`, CLI I/O (args, files, stdout). Milestone: a real CLI tool builds.
- **P8+ — Advanced.** Custom runtime namespaces, optimization passes, Linux
  cross-compile hardening, optional E4X/XML, second backend (Cranelift) behind
  the `Backend` trait.

---

## 12. Placeholder-name locations (rename here only)

When the real name is chosen, change it **only** in: the `cli` crate name and
its `main`/help strings, the `asr` binary name in `Cargo.toml`, the `.as`
extension registration (if any), the README, and this header. Nowhere else
should the language name appear as a literal.
