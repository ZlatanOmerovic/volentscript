# D0 — nbody + fib diagnostic findings (2026-07-09)

Read-only investigation, no code changes. Evidence from optimized LLVM IR
(`VS_DUMP_IR_OPT=1 … -O2`). Goal: pin the actual dominant cost of the two
worst compute rows so D1 is data-driven, not guessed.

## nbody (9.6× C — worst row) — DOMINATED BY REDUNDANT `vs_null_check` CALLS

`advance` is `@vs_fn11`. Its inner loop (`for.body7`) contains **24
`call void @vs_null_check(ptr …)` per iteration** — one (often two, e.g. IR
lines 862–863) before *every* field load and store. `vs_null_check` is an
external runtime function:

```
declare void @vs_null_check(ptr) local_unnamed_addr   ; NOT inlined
```
```rust
pub unsafe extern "C" fn vs_null_check(obj: *const u8) {
    if obj.is_null() { conv::type_error("null reference ..."); }
}
```

Why this is the cost:
- **Non-inlinable opaque call per field access.** `%o` and `%o17` are
  loop-invariant and provably non-null (freshly coerced from the vector,
  checked once), yet the check is re-emitted ~24×/iteration and LLVM cannot
  remove any of them — the call is an external `declare`.
- **It also blocks load optimization.** An opaque call may read/write memory,
  so LLVM must assume every `vs_null_check` could clobber the heap → it cannot
  CSE or hoist the `load double` field reads across the calls. So the checks
  cost their own call overhead *and* pin the field loads in place.
- Everything else in the loop is already good: field GEPs are `inbounds` and
  hoisted to the preheader; `Math.sqrt` lowers to `@llvm.sqrt.f64` (hardware
  `fsqrt`), not a libm call (my earlier guess — verified false); the boxed
  `vs_vec_get` + `vs_any_coerce_class` per element (~15/step) is real but minor
  next to 24-calls-per-inner-iteration × 10 inner × 5M steps.

**This is a general codegen-quality bug, not nbody-specific:** `codegen`
emits `self.null_check(obj)` unconditionally on every `FieldGet`/`FieldSet`/
`FieldIncDec` (llvm.rs). Every object-field access in every program pays it.

### Fix direction (D1 candidate — RECOMMENDED FIRST)

Emit the null check **inline** instead of as a call:
```
%isnull = icmp eq ptr %o, null
br i1 %isnull, label %npe (cold), label %cont   ; %npe: call noreturn throw
```
Consequences:
- On the `%cont` (non-null) path LLVM *knows* `%o` is non-null → GVN/dominator
  analysis eliminates every later redundant check on the same pointer, and
  hoists the surviving one out of the loop (invariant). 24 → ~0 in steady
  state.
- The throw target is `noreturn` + cold and touches no hot-path memory, so the
  field loads become CSE/hoist-eligible again.
- Wins everywhere objects are touched (oop, examples, nbody).
- Watch: the throw path must carry the right diagnostic; keep a runtime
  `vs_throw_null` (noreturn) for the cold block. Verify byte-identical output +
  that null derefs still throw the correct error.

## fib (5.8× C) — INT ARITHMETIC ROUND-TRIPS THROUGH DOUBLE

`fib` is `@vs_fn8(i32) -> i32`. ABI is already good: native `i32` args/return,
`tail call` recursion, **no** in-body safepoint. But `fib(n-1) + fib(n-2)` is
computed in `double`:

```
%call   = tail call i32 @vs_fn8(i32 %1)
%sitofp5  = sitofp i32 %call   to double
%call18 = tail call i32 @vs_fn8(i32 %2)
%sitofp19 = sitofp i32 %call18 to double
%add    = fadd double %sitofp5, %sitofp19       ; <-- int+int done as double
; …then coerced back to i32 for return:
%toi.gt/lt = fcmp … ; ToInt32 range check
%toi.trunc = fptosi double %cond to i32          ; (or slow vs_f64_to_int32)
```

Per call: 2× `sitofp` + `fadd` + a two-branch double range check + `fptosi`,
where C does a single `add`. The `+` follows AS3's "arithmetic yields Number"
rule, then the `int` return forces ToInt32 (ES §9.5).

### Fix direction (D1 candidate — SECOND)

When both operands and the consuming context are statically `int`/`uint`, emit
**native wrapping `i32` arithmetic** instead of promote-to-double-then-ToInt32.
This is semantics-preserving: `ToInt32(a + b)` for int32 `a,b` equals the
wrapping `i32` add (ES §9.5 ToInt32 idempotence / mod-2³² arithmetic). Applies
to `+ - *` in int-typed contexts across the language — general win, not fib-
specific. Needs care in `sema`/`mir` to prove the int-in/int-out context and
preserve exact wraparound semantics (cite ES §9.5 in the code).

## Recommendation for D1

Do **nbody null-check elimination first**: biggest row, clearest cost (24
opaque calls/iter), broadest benefit (every field access), and it also unblocks
LLVM's load optimization. fib's int-arithmetic native-lowering is a strong
second (also general). Neither needs a language feature; the boxed-object
value-type work stays deferred to its own phase — measure how much of nbody's
gap the null-check fix closes before deciding it's needed.
