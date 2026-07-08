#!/usr/bin/env bash
# Benchmark runner: builds everything, verifies output parity, times the
# matrix with hyperfine, and leaves results/<bench>.json for the report
# generator. Usage: ./run.sh
set -euo pipefail
cd "$(dirname "$0")"
export PATH="/opt/homebrew/opt/openjdk/bin:$PATH"
VS=../target/release/volentscript
BENCHES=(fib nbody binarytrees mandelbrot strings)

mkdir -p bin results
echo "== building =="
(cd .. && cargo build --workspace --release -q)
for b in "${BENCHES[@]}"; do
  $VS build "$b/$b.vlt" -O 2 -o "bin/$b-vs" > /dev/null
  clang -O2 -o "bin/$b-c" "$b/$b.c" -lm
  rustc -O -o "bin/$b-rs" "$b/$b.rs" 2>/dev/null
  (cd "$b" && go build -o "../bin/$b-go" "$b.go" && javac "$b.java")
done

echo "== verifying parity =="
for b in "${BENCHES[@]}"; do
  "./bin/$b-vs" > /tmp/vsbench-ref
  for cmd in "node $b/$b.js" "bun $b/$b.js" "deno run $b/$b.js" "./bin/$b-c" "./bin/$b-rs" "./bin/$b-go" "java -cp $b $b"; do
    $cmd > /tmp/vsbench-out 2>/dev/null
    diff -q /tmp/vsbench-ref /tmp/vsbench-out > /dev/null || { echo "PARITY FAIL: $b <- $cmd"; exit 1; }
  done
  echo "$b: all runtimes agree"
done

echo "== timing =="
for b in "${BENCHES[@]}"; do
  hyperfine --warmup 2 --runs 5 --export-json "results/$b.json" \
    --command-name volentscript "./bin/$b-vs" \
    --command-name c            "./bin/$b-c" \
    --command-name rust         "./bin/$b-rs" \
    --command-name go           "./bin/$b-go" \
    --command-name java         "java -cp $b $b" \
    --command-name node         "node $b/$b.js" \
    --command-name bun          "bun $b/$b.js" \
    --command-name deno         "deno run $b/$b.js"
done
echo "done — results/*.json"
