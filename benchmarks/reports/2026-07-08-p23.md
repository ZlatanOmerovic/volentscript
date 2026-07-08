# VolentScript benchmarks

*2026-07-08 — Apple M4, 16 GB, macOS. Method: [hyperfine](https://github.com/sharkdp/hyperfine), 3 warmup + 8 timed runs, mean wall time of the whole process (startup included — that is how CLI tools are used). Every implementation prints identical output, verified before timing; sources in this directory, reproduce with `./run.sh`.*

## Results (mean wall time; lower is better)

| benchmark | VolentScript | C (clang -O2) | Rust (-O) | Go | Java (OpenJDK) | Bun | Deno | Node.js |
|---|---|---|---|---|---|---|---|---|
| fib | 112.6 ms | 18.8 ms | **16.4 ms** | 22.4 ms | 34.7 ms | 46.2 ms | 76.8 ms | 97.2 ms |
| nbody | 1.72 s | **178.2 ms** | 184.8 ms | 188.3 ms | 224.8 ms | 269.9 ms | 239.2 ms | 250.8 ms |
| binarytrees | 1.56 s | 201.1 ms | 207.4 ms | 203.2 ms | **81.2 ms** | 94.6 ms | 111.2 ms | 117.1 ms |
| mandelbrot | 78.5 ms | **60.5 ms** | 75.9 ms | 70.1 ms | 93.5 ms | 108.5 ms | 92.5 ms | 95.7 ms |
| strings | 200.4 ms | **11.3 ms** | 18.4 ms | 20.5 ms | 81.4 ms | 17.3 ms | 28.6 ms | 32.8 ms |
| spectralnorm | 567.6 ms | 192.1 ms | **158.0 ms** | 252.8 ms | 193.9 ms | 237.2 ms | 335.2 ms | 338.1 ms |

## Relative to C (times slower; 1.0 = C)

| benchmark | VolentScript | C (clang -O2) | Rust (-O) | Go | Java (OpenJDK) | Bun | Deno | Node.js |
|---|---|---|---|---|---|---|---|---|
| fib | 6.0x | 1.0x | 0.9x | 1.2x | 1.8x | 2.5x | 4.1x | 5.2x |
| nbody | 9.7x | 1.0x | 1.0x | 1.1x | 1.3x | 1.5x | 1.3x | 1.4x |
| binarytrees | 7.8x | 1.0x | 1.0x | 1.0x | 0.4x | 0.5x | 0.6x | 0.6x |
| mandelbrot | 1.3x | 1.0x | 1.3x | 1.2x | 1.5x | 1.8x | 1.5x | 1.6x |
| strings | 17.7x | 1.0x | 1.6x | 1.8x | 7.2x | 1.5x | 2.5x | 2.9x |
| spectralnorm | 3.0x | 1.0x | 0.8x | 1.3x | 1.0x | 1.2x | 1.7x | 1.8x |

## Reading the numbers honestly

**Where VolentScript stands (v0.2.x):**

- **Tight numeric loops (mandelbrot)** — ~1.3x C and it beats every JS
  engine outright, after P22 (safepoint elision) removed the per-iteration
  GC check from the allocation-free inner loop. This is our strongest
  showing.
- **Call-heavy int code (fib)** — ~6x C, in JS-engine territory. The entry
  safepoint is already elided (fib allocates nothing), so the remaining
  cost is the call ABI itself and conservative-GC codegen constraints, not
  the collector.
- **Unboxed numeric vectors (spectralnorm)** — the P23 workload. Elements
  of `Vector.<Number>` are now stored as raw `f64` and read/written inline
  (no boxing, no runtime call): that alone is **2x faster** than the
  previous runtime-call-on-boxed-storage path. We still trail C by ~3x here
  because a bounds-check branch per element blocks LLVM autovectorization —
  bounds-check elimination is the next lever, not more vector work.
- **Object float math (nbody)** — ~10x C. Note this uses `Vector.<Body>`
  (object elements), so P23's unboxing does not apply; the gap is
  per-`Body` field access across pointers plus `Math.sqrt`, neither
  vectorized.
- **Allocation churn (binarytrees)** — ~8x C, and the JIT runtimes beat
  everyone (generational collectors love this workload). Our conservative
  mark-sweep with size-class pooling holds memory flat but pays per-object
  bookkeeping.
- **Strings** — ~18x C. Every operation transcodes UTF-16 storage to UTF-8
  and back inside the runtime; that is the whole gap.

**Remaining gaps map to planned optimizations, in impact order:**
bounds-check elimination + autovectorization for numeric-vector loops,
string ops that stay in UTF-16, and generational collection. None require
language changes.

**Fairness notes:** same algorithm and structure in every language,
idiomatic-simple, no SIMD/threads/arena tricks anywhere. Java is timed as
a process like everything else, so its short benchmarks carry JVM startup
(~30-80 ms) — its steady-state throughput on binarytrees still wins
outright. TypeScript is not a separate row: it erases to the same JS these
engines run.

## Versions

- VolentScript: `volentscript 0.2.0`
- C (clang -O2): `Apple clang version 21.0.0 (clang-2100.1.1.101)`
- Rust (-O): `rustc 1.95.0 (59807616e 2026-04-14)`
- Go: `go version go1.26.5 darwin/arm64`
- Java (OpenJDK): ``
- Bun: `bun 1.3.14`
- Deno: `deno 2.8.3 (stable, release, aarch64-apple-darwin)`
- Node.js: `node v24.16.0`
