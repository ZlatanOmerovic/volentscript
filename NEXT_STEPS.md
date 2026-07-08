# VolentScript — next steps

Working notes for resuming. `main` is green (clippy `-D warnings` clean, all
suites pass) at the P28 commit. Governance unchanged: propose a phase plan,
wait for **FULL SEND**, execute, demonstrate the milestone, stop. Cite
`docs/` for semantics; never invent AS3/ES4 behavior from memory.

## Where things stand (GC arc, most recent)

- **P27a — arena allocator (SHIPPED).** Bump arenas + 16-byte inline headers
  replaced the per-object `BTreeMap`. binarytrees 8× → 1.8× C, strings 13× →
  8.6× C. This was the big win.
- **P27b — non-moving generational (SHELVED, not on main).** Correct
  (teeth-test `tests/programs/gcgen.vlt`) but a measured regression
  (binarytrees 1.20× slower): a non-moving nursery pays the barrier/
  promotion/two-phase costs without the copying-nursery benefit. Preserved on
  branch `p27b-generational` with `P27B_MEASUREMENT.md`.
- **P28 — parallel marking (SHIPPED).** STW mark drained by ≤4 GC threads,
  atomic mark bit, shared-mutex worklist, `VS_GC_THREADS` knob. binarytrees
  390 → 356 ms (1.09×), isolated. Bandwidth-bound; single mutex caps scaling.

Benchmark history archived per date under `benchmarks/reports/` (latest =
`2026-07-08-p28.*`). Standing rule: **archive every future run per date** so
the language's evolution stays visible.

## Candidate next phases (ranked by leverage)

1. **Work-stealing mark deque (P28 follow-up).** The single-mutex worklist is
   why 8 threads run *slower* than 4. Replace it with per-worker Chase-Lev
   deques + stealing (crossbeam-deque, or hand-rolled std-only). Lifts the
   scaling ceiling on binarytrees marking beyond the current ~1.09×. Contained
   to `runtime/gc.rs`. Low-risk, measurable.

2. **Module-level global storage for mutable top-level `var`.** Deferred since
   P25 (SPECS §3.7). Today mutable top-level `var`s force closure-conversion of
   their readers (only `const`s inline). Needs GC-root tracking for pointer
   globals (register the globals' storage as a root range — machinery already
   exists via `add_root`). Removes a closure-conversion tax; helps programs
   with top-level mutable state.

3. **fib (~6× C) — call ABI.** fib allocates nothing; its gap is the call
   convention + conservative-GC codegen constraints, not the collector.
   Investigate: argument passing, safepoint placement at call boundaries,
   whether small leaf calls can skip the entry safepoint. Speculative — profile
   the emitted code first.

4. **nbody (~9.6× C) — per-field pointer chasing + `Math.sqrt`.** Uses
   `Vector.<Body>` (boxed object elements), so P23 unboxing doesn't apply.
   Levers: object field-access codegen, `Math.sqrt` inlining/intrinsic, whether
   `Body` can be stored unboxed/inline. Bigger design question (value types?).

## Explicitly NOT next (measured dead ends / needs prerequisites)

- **Generational GC** — revisit ONLY after precise stack maps + a copying
  nursery exist. Both are large prerequisites; the conservative stack scan is
  the blocker. See `P27B_MEASUREMENT.md`.
- Anything requiring moving/compaction — blocked by conservative scanning.

## Useful context for whoever resumes

- Env knobs: `VS_GC_LOG=1` (collection stats), `VS_GC_THREADS=N` (mark
  threads; 0/1 = serial), `VS_DUMP_IR` / `VS_DUMP_IR_OPT`, `VS_NO_BCE`.
- Thermal discipline: the M4 throttles under sustained load. Always cool
  (~30–45 s idle) and re-measure a single benchmark in isolation before
  trusting a delta; batch hyperfine matrices produce garbage numbers (learned
  twice this session — a "1.37× faster" that was really 1.09×).
- A/B method: build the candidate, `git stash` for the baseline binary, build
  that, `git stash pop`. `hyperfine --warmup 5 --runs 12 -N` one bench at a
  time.
- Build: `export LLVM_SYS_221_PREFIX="$(brew --prefix llvm)"`; LLVM 22 /
  inkwell `llvm22-1` / MSRV 1.88. Rebuild `target/<profile>/libruntime.a`
  (via `cargo build --workspace`) after runtime changes or the `examples`
  tests link against a stale static lib.
- Repo dir is still `~/Projects/vigorscript`; language = VolentScript, binary
  `volentscript`, extension `.vlt`.
