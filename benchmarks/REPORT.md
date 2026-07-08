# VolentScript benchmarks

*2026-07-08 — Apple M4, 16 GB, macOS. Method: [hyperfine](https://github.com/sharkdp/hyperfine), 2 warmup + 5 timed runs, mean wall time of the whole process (startup included — that is how CLI tools are used). Every implementation prints identical output, verified before timing; sources in this directory, reproduce with `./run.sh`.*

## Results (mean wall time; lower is better)

| benchmark | VolentScript | C (clang -O2) | Rust (-O) | Go | Java (OpenJDK) | Bun | Deno | Node.js |
|---|---|---|---|---|---|---|---|---|
| fib | 114.7 ms | 19.2 ms | **17.7 ms** | 23.1 ms | 33.3 ms | 42.2 ms | 65.3 ms | 74.8 ms |
| nbody | 1.76 s | 184.2 ms | 187.0 ms | **169.1 ms** | 230.6 ms | 268.4 ms | 243.3 ms | 249.3 ms |
| binarytrees | 1.63 s | 206.1 ms | 208.8 ms | 197.2 ms | **84.4 ms** | 95.5 ms | 113.8 ms | 121.5 ms |
| mandelbrot | 129.5 ms | **63.3 ms** | 80.0 ms | 72.5 ms | 103.4 ms | 116.4 ms | 94.9 ms | 99.9 ms |
| strings | 203.2 ms | **17.3 ms** | 18.0 ms | 21.2 ms | 82.1 ms | 17.7 ms | 28.6 ms | 33.7 ms |

## Relative to C (times slower; 1.0 = C)

| benchmark | VolentScript | C (clang -O2) | Rust (-O) | Go | Java (OpenJDK) | Bun | Deno | Node.js |
|---|---|---|---|---|---|---|---|---|
| fib | 6.0x | 1.0x | 0.9x | 1.2x | 1.7x | 2.2x | 3.4x | 3.9x |
| nbody | 9.6x | 1.0x | 1.0x | 0.9x | 1.3x | 1.5x | 1.3x | 1.4x |
| binarytrees | 7.9x | 1.0x | 1.0x | 1.0x | 0.4x | 0.5x | 0.6x | 0.6x |
| mandelbrot | 2.0x | 1.0x | 1.3x | 1.1x | 1.6x | 1.8x | 1.5x | 1.6x |
| strings | 11.7x | 1.0x | 1.0x | 1.2x | 4.7x | 1.0x | 1.7x | 1.9x |

## Reading the numbers honestly

**Where VolentScript stands (v0.2.x, first-ever benchmark run):**

- **Tight numeric loops (mandelbrot)** — within ~2x of C and roughly at
  JS-JIT level. The LLVM -O2 pipeline does its job when the code stays in
  registers.
- **Call-heavy int code (fib)** — ~6x C, in JS-engine territory. Cost:
  a GC safepoint check per call plus conservative-GC codegen constraints.
- **Object float math (nbody)** — ~10x C. The gap is not the field math:
  `Vector.<T>` element reads are runtime calls on boxed storage, and GC
  safepoints in inner loops block loop-invariant hoisting.
- **Allocation churn (binarytrees)** — ~8x C, and the JIT runtimes beat
  everyone (generational collectors love this workload). Our conservative
  mark-sweep with size-class pooling holds memory flat but pays per-object
  bookkeeping.
- **Strings** — ~12x C. Every operation transcodes UTF-16 storage to UTF-8
  and back inside the runtime; that is the whole gap.

**Each gap maps to a planned optimization, in impact order:** unboxed
`Vector.<Number>`/`Vector.<int>` storage, string ops that stay in UTF-16,
allocation fast path + safepoint hoisting out of inner loops, and
generational collection. None require language changes.

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
- Java (OpenJDK): `openjdk 26.0.1 2026-04-21`
- Bun: `bun 1.3.14`
- Deno: `deno 2.8.3 (stable, release, aarch64-apple-darwin)`
- Node.js: `node v24.16.0`
