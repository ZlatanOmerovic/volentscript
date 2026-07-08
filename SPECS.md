# SPECS.md — Language Specification

> **Name:** `VolentScript`. CLI tool: `volentscript`. Source extension: `.vlt`.
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
and type system, decoupled entirely from Flash. You write `.vlt` source, the
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

**Module-const inlining (P25):** a function that references a top-level
variable captures it, which closure-converts the function — its calls then go
through an indirect, un-inlinable dispatch. To keep the idiomatic pattern
(module-level `const`s used inside functions) fast, a top-level `const` whose
initializer folds to a compile-time literal (int/uint/Number/Boolean/String
literals and arithmetic/bitwise/string-concat over other such consts) is
**inlined at every reference** instead of captured. The reader stays a plain
top-level function and inlines normally. Folding runs before any function body
is checked, so ordering is not an issue; non-foldable consts and any read that
can't be typed exactly fall back to capture (correct, just unoptimized).

*Deferred — module-level global storage:* mutable top-level `var`s (and
non-foldable `const`s) still capture, so functions that read/write them
closure-convert. Giving top-level `var`/`const` real static storage
(referenced by a global load/store, not a capture cell) would remove that for
all module state, not just foldable consts. It is a larger, separate change
because a global holding a GC pointer (String/Array/object) must be registered
as a **GC root** scanned every collection — const-inlining sidesteps that
entirely (immediates and interned strings). Bundle it with that GC-root work.

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

**Storage (P23):** numeric instantiations — `Vector.<Number>`, `Vector.<int>`,
`Vector.<uint>` — are stored **unboxed** as a contiguous `f64`/`i32`/`u32`
buffer (a flat `#[repr(C)]` header the codegen reads), and compiled code
**inlines** in-range element reads/writes as a bounds-checked load/store — no
boxing, no runtime call (avmplus stores typed Vectors unboxed likewise). These
vectors are also GC leaves (no element tracing). Reference-typed instantiations
(`Vector.<String>`, `Vector.<SomeClass>`) keep boxed `VsAny` storage, traced
precisely. This is the value-typed/reference-typed boundary of §4.2 made
concrete for `Vector`.

**Bounds-check elimination (P24):** a MIR pass (`mir::bce`) removes the
per-element bounds branch from counted loops `for (var i = C; i < B; i++)`
where `i` indexes a plain-local unboxed-numeric `Vector` and every occurrence
of that vector in the loop is a `v[i]` access or `v.length` read. It rewrites
the loop to `if (B <= v.length && …) { <fast, unchecked> } else { <slow,
checked> }`; the runtime guard makes the transform sound regardless of the
analysis' precision. Removing the branch is what lets LLVM autovectorize the
inner loop (e.g. spectralnorm's inner products).

*Deferred (broader scope, not implemented — documented so it isn't
re-discovered from scratch):* affine indices `v[i±k]` (the guard must cover
both ends of the index range) and loops that legitimately grow `v` (the buffer
can reallocate mid-loop, so a hoisted data pointer would dangle — those need
per-iteration revalidation, not an entry-only guard). The v1 scope excludes
both precisely because a wrong range proof there is memory-unsafety, not a
wrong result.

### 4.4 Strings
Keep AS3-observable string semantics (indexing, `length`, methods). Storage is
**UTF-16 code units** (`VsString` = `{ len, *const u16 }`) so `.length`,
`charCodeAt`, indexing, and surrogate handling match AS3 exactly.

**UTF-16-native ops (P26):** string methods operate on the `u16` buffer
directly — `split`, `replace`, `toUpperCase`/`toLowerCase`, and `Array`/
`Vector` `join` no longer round-trip through UTF-8 (`indexOf`, `lastIndexOf`,
and `+` already did). Case mapping takes an ASCII fast path and otherwise
decodes surrogate pairs to scalars, applies the Unicode default case mapping,
and re-encodes — never touching UTF-8. This removed the transcode cost; the
remaining `strings`-benchmark gap is per-operation allocation churn (§7 GC
work), not encoding.

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
- Runtime namespaces: P12 implements the static subset (`namespace`
  declarations with URI identity, namespaced members, compile-time
  `ns::name`, `use namespace`); P16 adds the first-class layer —
  `Namespace` values interned by URI, `new Namespace(uri)`, `.uri`, and
  runtime-computed `obj.q::name` qualification via per-class reflection
  tables in the descriptor (fields box by type tag; methods dispatch
  through boxed-ABI wrappers). Runtime `use namespace` remains out.
- `Proxy`, `flash.utils.*`, AMF, `ByteArray`-as-Flash-API (a plain byte buffer
  type may exist in the runtime, but not the Flash `ByteArray` surface).

---

## 6. Standard library surface (enumerated, phase-tagged)

Phase tags: **P3** = needed for first runnable binary, **P4** = classes/OOP,
**P5** = generics/collections, **P7** = stdlib breadth. Implement in the runtime
(§7) with `.vlt` declaration stubs (`intrinsic`/`native` bindings).

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
- **`Date`** (P11: instances with UTC/local getters, setTime, Date.UTC;
  Date.parse/string ctor + component setters are backlog), **`RegExp`** (P10:
  `fancy-regex`-backed; lastIndex is read-only and `split(RegExp)` is
  backlog), **`JSON`** (P7).
- **`Error`** hierarchy (P6): `Error`, `TypeError`, `RangeError`,
  `ReferenceError`, `ArgumentError`, `SyntaxError`, `VerifyError`(drop),
  custom user `Error` subclasses.
- **`Function`** (P4): `.length`, `.call`, `.apply`.
- **I/O (non-Flash, new):** minimal CLI runtime — `print`/`trace` to stdout,
  process args, exit code, env, stdin readLine (P18), file IO (P7
  read/write/exists; P18 append/remove/copy/rename/mkdir/rmdir/list/
  isDirectory/size/mtime — rmdir refuses non-empty dirs), sockets (P15:
  blocking TCP — `Socket.connect`, `write`/`readLine`/`read`/`close`,
  `ServerSocket.bind`/`accept`/`localPort`; reads null at EOF, errors
  throw). This is the Redtamarin-shaped surface that makes it a *usable*
  language. Do not model it on `flash.*`.

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
  - **P27 Part A (allocation fast path):** small blocks are bump-allocated
    from per-size-class **arenas** and carry a 16-byte inline header
    (kind/size/mark/live), so allocation no longer inserts into a per-object
    `BTreeMap` and sweep walks arenas cache-linearly instead of iterating a
    map of every block. Only arenas and large blocks live in the interior-
    pointer registry (`regions`, ~tens of entries), shrinking the
    conservative-scan predecessor query. Strictly **non-moving** (a
    conservatively-scanned maybe-pointer can't be rewritten). Measured: clean
    A/B `binarytrees` 1530→415 ms (3.68×), `strings` 142→80 ms (1.77×),
    alloc-light benches flat.
  - **P27 Part B (non-moving generational nursery) — implemented, measured a
    regression, NOT shipped.** A sticky-mark-bit generational collector
    (gen bit + in-place promotion + minor/major split + write barrier) is
    correct (proven by `tests/programs/gcgen.vlt`) but a net loss: a
    non-moving nursery still marks survivors and walks young arenas, so it
    pays the barrier/promotion/two-phase costs without the copying-nursery
    benefit that makes generational fast in a JIT (binarytrees builds trees
    wholly live during construction → minors promote everything, free
    nothing). Clean A/B: Part A beats it (binarytrees 1.20×, strings 1.11×).
    Preserved on branch `p27b-generational`; revisit only with precise stack
    maps + a copying nursery.
  - **P28 (parallel marking):** the stop-the-world mark phase is drained by
    up to N worker threads (default `min(cores-1, 4)`, `VS_GC_THREADS`
    override) over a shared worklist with atomic mark bits and idle-count
    termination detection; scoped-per-collection (`std::thread::scope`), no
    new deps. Safe because the mutator is paused (heap static) and only the
    unique mark-winner traces a block (so `!Sync` side storage is never
    shared). Sweep stays single-threaded. Measured: clean isolated A/B
    `binarytrees` 390→356 ms (1.09×); other rows flat (small live sets stay
    below the parallel threshold, or are alloc-light). Marking is
    memory-bandwidth bound and the single-mutex worklist caps scaling (8
    threads slower than 4) — a work-stealing deque is the future lever.
- **Runtime type support:** `is`, `as`, `instanceof`, `typeof`, class-of.
- **Coercion helpers:** the numeric/string coercion rules from §3.3.
- **Builtins:** implementations backing the §6 stdlib, bound to `.vlt`
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
tests/         # end-to-end .vlt programs + expected output
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
- **End-to-end** golden tests in `tests/`: each `.vlt` file has an expected
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
  `trace`. Milestone: parse a core `.vlt` file to an AST snapshot.
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
- **P8+ — Advanced.** Custom runtime namespaces (P12/P16 ✓), optimization
  passes (P13 ✓), Linux cross-compile hardening (P14 ✓), Windows
  cross-target (P20 ✓: `x86_64-pc-windows-gnu` via zig; the runtime
  carries its own non-unwinding Win64 setjmp/longjmp because msvcrt's
  longjmp SEH-unwinds and zig's mingw import set lacks `_setjmp`; golden
  corpus executed on a Windows runner in CI; compiler-hosted-on-Windows
  remains future), sockets (P15 ✓),
  optional E4X/XML (out, §5), second backend (Cranelift) behind the
  `Backend` trait — **DECISION (P17): deferred post-v1.** The v1 exception
  scheme compiles `_setjmp` (returns_twice) into user code; Cranelift does
  not support returns_twice, so a Cranelift port first requires redesigning
  exception lowering (per-call throw-flag unwinding) and then a full
  duplicate of class/closure/RTTI/reflection emission — double maintenance
  for every future feature. The architectural claim the trait exists for is
  verified mechanically instead: no frontend crate (lexer, parser, ast,
  sema, mir) has any `inkwell`/LLVM dependency (`cargo tree` clean); only
  `codegen` links LLVM. Revisit if a JIT/fast-debug-build need appears.

---

## 12. Name (resolved)

The language is **VolentScript**, the CLI tool `volentscript`, the source
extension `.vlt` (renamed from the AS3-era `.as` after v1 completion). The
name appears as a literal only in the `cli` crate, the README, and this
header; the runtime ABI keeps its historical `vs_` symbol prefix (which
still reads as VolentScript). Brand assets (pen-nib V mark, lockups,
favicon, OG image, and Volen the fire-salamander mascot) live in
`assets/`, sourced from the website project; primary color `#E85C0F`.
