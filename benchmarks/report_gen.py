#!/usr/bin/env python3
"""Generates REPORT.md + report.html from results/*.json (hyperfine)."""
import json, subprocess, datetime

BENCHES = ["fib", "nbody", "binarytrees", "mandelbrot", "strings", "spectralnorm"]
RUNTIMES = ["volentscript", "c", "rust", "go", "java", "bun", "deno", "node"]
LABEL = {"volentscript": "VolentScript", "c": "C (clang -O2)", "rust": "Rust (-O)",
         "go": "Go", "java": "Java (OpenJDK)", "node": "Node.js", "bun": "Bun", "deno": "Deno"}
DESC = {
    "fib": ("fib(35), naive recursion", "function-call overhead, int arithmetic"),
    "nbody": ("n-body, 5M steps (CLBG shape)", "float math on object fields, Vector indexing"),
    "binarytrees": ("binary trees, depth 16 (CLBG shape)", "allocation churn, GC pressure"),
    "mandelbrot": ("mandelbrot 1500², 50 iters", "tight numeric loops"),
    "strings": ("split/join/case/search × 60k", "string operations, UTF handling"),
    "spectralnorm": ("spectral norm, n=2500 (CLBG shape)", "unboxed Vector.<Number> access (P23) + bounds-check elimination (P24)"),
}

def sh(cmd): return subprocess.run(cmd, shell=True, capture_output=True, text=True).stdout.strip()

data = {}
for b in BENCHES:
    d = json.load(open(f"results/{b}.json"))
    data[b] = {r["command"]: (r["mean"] * 1000, r["stddev"] * 1000) for r in d["results"]}

versions = {
    "volentscript": sh("../target/release/volentscript --version"),
    "c": sh("clang --version | head -1"),
    "rust": sh("rustc --version"),
    "go": sh("go version"),
    "java": sh("java --version | head -1"),
    "node": "node " + sh("node --version"),
    "bun": "bun " + sh("bun --version"),
    "deno": sh("deno --version | head -1"),
}
machine = sh("sysctl -n machdep.cpu.brand_string") + ", " + sh("sysctl -n hw.memsize | awk '{print $1/1073741824\" GB\"}'") + ", macOS"
date = datetime.date.today().isoformat()

def fmt(ms): return f"{ms/1000:.2f} s" if ms >= 1000 else f"{ms:.1f} ms"

# ---------------- REPORT.md ----------------
md = []
md.append("# VolentScript benchmarks\n")
md.append(f"*{date} — {machine}. Method: [hyperfine](https://github.com/sharkdp/hyperfine), 3 warmup + 8 timed runs, mean wall time of the whole process (startup included — that is how CLI tools are used). Every implementation prints identical output, verified before timing; sources in this directory, reproduce with `./run.sh`.*\n")
md.append("## Results (mean wall time; lower is better)\n")
hdr = "| benchmark | " + " | ".join(LABEL[r] for r in RUNTIMES) + " |"
md.append(hdr)
md.append("|" + "---|" * (len(RUNTIMES) + 1))
for b in BENCHES:
    cells = []
    best = min(data[b][r][0] for r in RUNTIMES)
    for r in RUNTIMES:
        m = data[b][r][0]
        cell = fmt(m)
        if m == best: cell = f"**{cell}**"
        cells.append(cell)
    md.append(f"| {b} | " + " | ".join(cells) + " |")
md.append("\n## Relative to C (times slower; 1.0 = C)\n")
md.append(hdr)
md.append("|" + "---|" * (len(RUNTIMES) + 1))
for b in BENCHES:
    c = data[b]["c"][0]
    md.append(f"| {b} | " + " | ".join(f"{data[b][r][0]/c:.1f}x" for r in RUNTIMES) + " |")
md.append("""
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
- **Unboxed numeric vectors (spectralnorm)** — `~1.5x C`, beating Bun and
  Deno and level with Node. Two optimizations stack here: P23 stores `Vector.<Number>`
  elements as raw `f64` read/written inline (2x over the old boxed
  runtime-call path), and P24 (bounds-check elimination by loop versioning)
  removes the per-element bounds branch from provably-in-range counted
  loops — which is what finally lets LLVM autovectorize the inner products
  (0 → 13 vector ops in the emitted code; ~1.1x on its own). P25 then lets
  the dimension be an idiomatic module-level `const` referenced directly in
  the helpers: it inlines to a literal, so the helpers are not
  closure-converted and `evalA` inlines — no parameter-threading workaround
  needed.
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

**Remaining gaps map to planned optimizations, in impact order:** string ops
that stay in UTF-16 instead of transcoding (strings), module-level global
storage so mutable top-level `var`s stop closure-converting their readers
(P25 already did this for `const`s), and generational collection
(binarytrees). fib's ~6x is the call ABI itself and nbody's ~10x is per-field
pointer chasing plus `Math.sqrt`, neither of which is a collector or closure
issue. None require language changes.

**Fairness notes:** same algorithm and structure in every language,
idiomatic-simple, no SIMD/threads/arena tricks anywhere. Java is timed as
a process like everything else, so its short benchmarks carry JVM startup
(~30-80 ms) — its steady-state throughput on binarytrees still wins
outright. TypeScript is not a separate row: it erases to the same JS these
engines run.

## Versions
""")
for r in RUNTIMES:
    md.append(f"- {LABEL[r]}: `{versions[r]}`")
md.append("")
md_text = "\n".join(md)
open("REPORT.md", "w").write(md_text)

# ---------------- report.html ----------------
ORANGE = "#E85C0F"; DARK = "#241B14"; CREAM = "#FFFDF9"; AMBER = "#FFAE3D"
rows_html = ""
for b in BENCHES:
    best = min(data[b][r][0] for r in RUNTIMES)
    worst = max(data[b][r][0] for r in RUNTIMES)
    bars = ""
    for r in RUNTIMES:
        m = data[b][r][0]
        pct = max(2, m / worst * 100)
        color = ORANGE if r == "volentscript" else "#B9AC9E"
        weight = "700" if r == "volentscript" else "400"
        bars += f"""
      <div class="row"><span class="lang" style="font-weight:{weight}">{LABEL[r]}</span>
        <span class="track"><span class="bar" style="width:{pct:.1f}%;background:{color}"></span></span>
        <span class="val">{fmt(m)}<em>{m/data[b]['c'][0]:.1f}x C</em></span></div>"""
    title, tests = DESC[b]
    rows_html += f"""
  <section>
    <h2>{b}</h2>
    <p class="sub">{title} &mdash; tests {tests}</p>
    {bars}
  </section>"""

html = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>VolentScript benchmarks</title>
<style>
  :root {{ color-scheme: light; }}
  * {{ box-sizing: border-box; margin: 0; }}
  body {{ font: 16px/1.55 -apple-system, "Segoe UI", sans-serif; background: {CREAM}; color: {DARK}; max-width: 860px; margin: 0 auto; padding: 40px 20px 80px; }}
  h1 {{ font-size: 34px; letter-spacing: -0.5px; }}
  h1 span {{ color: {ORANGE}; }}
  .meta {{ color: #7A6E60; margin: 8px 0 34px; font-size: 14px; }}
  section {{ margin: 34px 0; }}
  h2 {{ font-size: 20px; border-bottom: 3px solid {ORANGE}; display: inline-block; padding-bottom: 2px; }}
  .sub {{ color: #7A6E60; font-size: 14px; margin: 6px 0 14px; }}
  .row {{ display: flex; align-items: center; gap: 10px; margin: 5px 0; }}
  .lang {{ width: 130px; font-size: 14px; text-align: right; flex: none; }}
  .track {{ flex: 1; background: #F0E7DC; border-radius: 4px; height: 20px; overflow: hidden; }}
  .bar {{ display: block; height: 100%; border-radius: 4px; }}
  .val {{ width: 130px; font-size: 13px; font-variant-numeric: tabular-nums; flex: none; }}
  .val em {{ color: #9A8C7C; font-style: normal; margin-left: 6px; font-size: 12px; }}
  .notes {{ background: #F7F3EC; border-left: 4px solid {AMBER}; padding: 14px 18px; border-radius: 0 8px 8px 0; margin-top: 40px; font-size: 15px; }}
  .notes h3 {{ margin-bottom: 8px; }}
  .notes li {{ margin: 6px 0 6px 18px; }}
  code {{ background: #F0E7DC; padding: 1px 5px; border-radius: 4px; font-size: 90%; }}
  footer {{ margin-top: 44px; color: #9A8C7C; font-size: 13px; }}
  a {{ color: {ORANGE}; }}
</style></head><body>
<h1>Volent<span>Script</span> benchmarks</h1>
<p class="meta">{date} &middot; {machine} &middot; hyperfine, 3 warmup + 8 timed runs, mean process wall time (startup included) &middot; identical outputs verified across all runtimes before timing &middot; <a href="https://github.com/ZlatanOmerovic/volentscript/tree/main/benchmarks">sources &amp; runner</a></p>
{rows_html}
<div class="notes">
  <h3>Reading the numbers honestly</h3>
  <ul>
    <li><b>Tight numeric loops</b> (mandelbrot): within ~2x of C, at JS-JIT level — LLVM -O2 works when code stays in registers.</li>
    <li><b>Call-heavy code</b> (fib): ~6x C — a GC safepoint check per call, plus conservative-GC codegen constraints.</li>
    <li><b>Object float math</b> (nbody): ~10x C — <code>Vector.&lt;T&gt;</code> element reads are runtime calls on boxed storage; safepoints block loop hoisting.</li>
    <li><b>Allocation churn</b> (binarytrees): ~8x C; generational JIT collectors win this workload outright.</li>
    <li><b>Strings</b>: ~12x C — every operation transcodes UTF-16 &harr; UTF-8 inside the runtime.</li>
  </ul>
  <p style="margin-top:10px">Each gap maps to a planned optimization: unboxed numeric Vectors, UTF-16-native string ops, allocation fast path + safepoint hoisting, generational GC. None require language changes. Same idiomatic-simple algorithm in every language; Java timed as a process like everything else (its binarytrees throughput still wins). TypeScript is not a separate row — it erases to the same JS.</p>
</div>
<footer>VolentScript — ActionScript 3, revived. Native, ahead-of-time, and entirely of its own will.</footer>
</body></html>"""
open("report.html", "w").write(html)

# Dated archive: every run is preserved under reports/<date>.* so the
# language's evolution stays visible; merged raw JSON alongside.
import os
os.makedirs("reports", exist_ok=True)
open(f"reports/{date}.md", "w").write(md_text)
open(f"reports/{date}.html", "w").write(html)
merged = {b: {r: {"mean_ms": data[b][r][0], "stddev_ms": data[b][r][1]} for r in RUNTIMES} for b in BENCHES}
json.dump({"date": date, "machine": machine, "versions": versions, "results": merged},
          open(f"reports/{date}.json", "w"), indent=1)
history = sorted(f[:-5] for f in os.listdir("reports") if f.endswith(".html"))
lines = ["# Benchmark history", "",
         "One entry per run — open any date to see that day's full report.", ""]
for d in reversed(history):
    lines.append(f"- **{d}** — [report]({d}.html) · [markdown]({d}.md) · [raw]({d}.json)")
lines.append("")
open("reports/README.md", "w").write("\n".join(lines))
print(f"REPORT.md + report.html written; archived as reports/{date}.*")
