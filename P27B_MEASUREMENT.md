# P27 Part B — non-moving generational GC: measured negative result

**Status: implemented, correct, and NOT shipped to `main`.** This branch
preserves the work. `main` stays on P27 Part A (the arena allocator).

## What was built

A sticky-mark-bit generational collector layered on the Part A arenas
(`crates/runtime/src/gc.rs`):

- A `gen` bit (young/old) in the 16-byte block header; survivors are promoted
  in place (non-moving — a conservatively-scanned word can't be relocated).
- **Minor** collections trace/sweep only the nursery; **major** collections
  are the full mark-sweep. Old blocks are assumed live in a minor.
- A **write barrier** (`vs_gc_remember`) records old containers that gain a
  reference into a remembered set, seeded as roots for the next minor. Emitted
  inline from the compiled `FieldSet` and cell stores (guarded by an inline
  `gen == old` check) and from the runtime container setters
  (`vs_arr_set/push/unshift/splice`, boxed `vs_vec_set/push/unshift`,
  expando `set_prop`).

Correctness is verified by `tests/programs/gcgen.vlt`, a stress test with
*teeth*: it stores young objects into promoted (old) containers from a callee
frame that is popped before the churn, so the young children live on the heap
only. With the barrier neutered, every round's checksum is corrupted; with it,
all pass. All other suites stay green.

## Why it does not ship — the measurement

Clean A/B, release, `hyperfine --warmup 3 --runs 8`, isolated machine. Part A
is the committed baseline; Part B is this branch (with the nursery threshold
already tuned to `max(2 MB, live)`):

| benchmark    | Part A vs Part B |
|--------------|------------------|
| binarytrees  | **A 1.20× faster** |
| strings      | **A 1.11× faster** |
| nbody        | flat (within noise) |
| fib          | flat |
| mandelbrot   | flat |
| spectralnorm | flat |

Part B helps nothing and regresses the two allocation-bound workloads it was
meant to help.

## The structural reason (not a tuning miss)

**A non-moving nursery does not get generational GC's actual payoff.** In a
JIT, generational wins on binarytrees because the nursery is a *copying*
space: allocation is a bump and dead young objects cost *nothing* to reclaim
(only survivors are evacuated). Our conservative collector cannot move objects,
so Part A already made allocation a bump — and a minor still has to *mark*
survivors and *walk* the young arenas.

binarytrees builds whole trees that are entirely live during construction
(held by the recursion). A minor firing mid-build therefore **promotes
everything and frees nothing** — measured at the untuned 2 MB nursery: 211
minors freeing 0 blocks, then 17 full majors doing all the real reclamation.
The trees are *medium-lived* (survive construction, die together), which is
precisely the shape generational GC handles worst. Tuning the nursery to the
live set cut collections 228→42 and the regression 1.35×→1.20×, but cannot
turn it into a win: we pay generational's costs (write barrier on every node
link, promotion, two-phase bookkeeping) for none of its benefit.

## When to revisit

Generational would pay off here only with a **copying/compacting nursery**,
which requires **precise stack maps** (so the collector knows exactly which
stack slots are live pointers and can rewrite them) instead of today's
conservative stack scan. That is a much larger project and a prerequisite, not
a tuning knob. Until then, the remaining binarytrees/strings gap is dominated
by full-heap mark cost; the more promising levers are parallel or incremental
marking, which keep the non-moving invariant.
