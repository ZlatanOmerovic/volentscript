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

1. **Better parallel-mark work distribution (P28 follow-up).** The single
   Mutex<Vec> worklist is why 8 threads run *slower* than 4. Two documented,
   better structures (see Research notes below) — **recommend trying the
   Boehm-style approach first**, it is simpler and proven in a *conservative*
   collector:
   - **(a) Boehm-style global queue + thread-local stacks, dup-tolerant.**
     Each marker copies work to a thread-private stack with *no*
     synchronization, tolerating occasional duplicate marking (the atomic
     mark bit dedups anyway); a thread returns work to the global queue only
     when it runs low or risks local overflow. No lock on the hot path, no
     weak-memory minefield. This is exactly what BDW-GC's `-DPARALLEL_MARK`
     does.
   - **(b) Chase-Lev work-stealing deques** (crossbeam-deque or hand-rolled).
     Faster in theory, but **a naïve deque is INCORRECT on ARM/M4**: it needs
     the specific acquire/release/seq-cst fence mix from Lê et al. (PPoPP'13),
     and watch the `size_t` underflow bug in `take` on an empty deque. Only
     worth it if (a) doesn't scale enough.
   - Reality check: Boehm reports full linear speedup is unreachable — the
     mark phase is **memory-bandwidth bound** (they measured ~1.4× on 2
     processors). Our own 1.09× / cap-at-4 is consistent. Manage expectations:
     the ceiling here is bandwidth, not the queue. Contained to
     `runtime/gc.rs`; low-risk, measurable.

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

## Research notes (external sources — read before the next GC phase)

Gathered from a web pass on 2026-07-08. Everything the GC arc was built on so
far came from training knowledge, not verified against these; they confirm the
big calls and sharpen the next ones.

### Parallel marking in a *conservative* collector (the direct precedent)

BDW-GC (Boehm-Demers-Weiser) is a conservative mark-sweep with an optional
parallel marker (`-DPARALLEL_MARK`) — the closest published match to our
setup. Key design points, and how they map to us:

- The mark phase is where "the large majority of the collection time" goes —
  so parallelizing it (P28) targets the right phase.
- Core routine `GC_mark_from` does a *bounded* amount of marking per call and
  runs on **thread-private mark stacks**, not a global stack. → our single
  shared mutex is the wrong structure; move to per-thread stacks.
- Work sharing is **dup-tolerant and unsynchronized**: markers copy from a
  global queue to a local stack "with no synchronization… it is possible for
  more than one worker to remove the same entry, resulting in some work
  duplication." Work goes back to the global queue only when it "appears to be
  running low, or if the local stack is in danger of overflowing." → validates
  that our atomic mark-bit dedup makes duplicate pushes harmless; we can drop
  locking on the hot path.
- Scaling is **memory-bandwidth bound**: "full linear speedup is probably not
  achievable… processors usually share a single memory bus." They saw ~1.4× on
  2 processors. → our sublinear 1.09× and 8<4 regression are expected, not a
  bug.
- Links: [Algorithmic overview](https://www.hboehm.info/gc/gcdescr.html) ·
  [Scalability / parallel marking](https://www.hboehm.info/gc/scale.html) ·
  [bdwgc source](https://github.com/bdwgc/bdwgc) ·
  [GC home](https://hboehm.info/gc/)

### Work-stealing deques (option (b) above)

- Original: Chase & Lev, "Dynamic Circular Work-Stealing Deque," SPAA 2005 —
  the standard lock-free deque (push/take local, steal remote).
  [Semantic Scholar](https://www.semanticscholar.org/paper/Dynamic-circular-work-stealing-deque-Chase-Lev/f856a996e7aec0ea6db55e9247a00a01cb695090)
- **Must-read before implementing on ARM:** Lê, Pop, Cohen, Zappa Nardelli,
  "Correct and Efficient Work-Stealing for Weak Memory Models," PPoPP 2013.
  Gives the exact C11 relaxed/acquire-release/seq-cst fences each op needs on
  POWER/ARM; a naïve seq-cst-free port is broken on M4.
  [PDF](https://fzn.fr/readings/ppopp13.pdf) ·
  [PDF mirror](https://www.di.ens.fr/~zappa/readings/ppopp13.pdf) ·
  [HAL](https://inria.hal.science/hal-00802885) ·
  [ACM](https://dl.acm.org/doi/10.1145/2442516.2442524)
- Accessible writeup incl. the `size_t` underflow bug in `take`:
  [wingolog](https://wingolog.org/archives/2022/10/03/on-correct-and-efficient-work-stealing-for-weak-memory-models)
- Rust: the `crossbeam-deque` crate implements Chase-Lev with the correct
  orderings — using it sidesteps the weak-memory hazard (one dep to weigh
  against the `#![forbid(unsafe_code)]`-elsewhere policy; runtime already
  allows unsafe/deps).

### Parallel-mark termination detection (better than our idle-count)

- G1 (HotSpot): terminate when local *and* global mark stacks are empty and
  the scan fingers reach the end.
  [Concurrent marking in G1](https://tschatzl.github.io/2022/08/04/concurrent-marking.html)
- HotSpot parallel GC uses a "2·N failures" termination protocol (with an
  early-out when few threads remain active).
- "Task-pushing" — a scalable parallel marking scheme that pushes spare tasks
  to peers and eliminates synchronization in the mark loop; an alternative to
  steal-based balancing.
  [ResearchGate](https://www.researchgate.net/publication/224712593_Task-pushing_a_Scalable_Parallel_GC_Marking_Algorithm_without_Synchronization_Operations)
- Mark-stack overflow recovery (needed if per-thread stacks are bounded):
  discard on overflow, then rescan the heap for marked objects with unmarked
  children.

### Sticky mark-bit generational — confirms the P27b shelving

wingolog's writeup of the exact algorithm we built: minor collections keep
mark bits "sticky" (not cleared) so survivors are implicitly the old gen;
promotion is in-place; a write barrier + remembered set cover old→young. The
author's own verdict matches our measurement: it is **"better than nothing,
not quite as good as a semi-space nursery"** — "allocation costs more" and
locality is worse than an evacuating collector. That is precisely why P27b
regressed: the win of generational comes from the *copying* nursery we cannot
have while non-moving.
[Sticky mark-bit algorithm](https://wingolog.org/archives/2022/10/22/the-sticky-mark-bit-algorithm) ·
[Baffled by generational GC](https://wingolog.org/archives/2025/02/09/baffled-by-generational-garbage-collection)

### Longer-horizon alternative: Immix (mark-region)

Blackburn & McKinley, "Immix: A Mark-Region Garbage Collector with Space
Efficiency, Fast Collection, and Mutator Performance" (PLDI 2008). Bump-
allocates into fixed regions/lines, marks lines not objects, and *opportunis-
tically* evacuates to defragment. A non-moving Immix variant (or "conservative
Immix" / RiVM-style) could give better locality than our free-list arenas
without full precise-stack-map moving. Worth studying if allocation locality
(not mark cost) becomes the bottleneck.
[PDF](https://dl.acm.org/doi/pdf/10.1145/1375581.1375586)

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
