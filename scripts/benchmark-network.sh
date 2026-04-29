#!/usr/bin/env bash
set -euo pipefail

files="${FASTSYNC_BENCH_FILES:-1000}"
bytes="${FASTSYNC_BENCH_BYTES:-4096}"
concurrency_list="${FASTSYNC_BENCH_CONCURRENCY:-1 2 4 8}"
base="${FASTSYNC_BENCH_DIR:-$(mktemp -d /tmp/fastsync-bench.XXXXXX)}"
bin="${FASTSYNC_BIN:-target/release/fastsync}"
port_base="${FASTSYNC_BENCH_PORT:-18443}"
code="${FASTSYNC_BENCH_CODE:-123456}"

mkdir -p "$base/src" "$base/dst"

if [ ! -x "$bin" ]; then
  cargo build --release
fi

index=0
while [ "$index" -lt "$files" ]; do
  file="$(printf '%s/src/file-%06d.dat' "$base" "$index")"
  if [ ! -f "$file" ]; then
    head -c "$bytes" /dev/zero > "$file"
  fi
  index="$((index + 1))"
done

printf 'benchmark_root=%s\n' "$base"
printf 'files=%s bytes_per_file=%s\n' "$files" "$bytes"

run_case() {
  concurrency="$1"
  port="$2"
  target="$base/dst-c$concurrency"
  rm -rf "$target"
  mkdir -p "$target"

  "$bin" share "$base/src" \
    --bind "127.0.0.1:$port" \
    --code "$code" \
    --mode send \
    --log-level error &
  server_pid="$!"
  sleep 0.2

  start_ns="$(date +%s%N)"
  "$bin" connect "quic://127.0.0.1:$port" "$target" \
    --pull \
    --code "$code" \
    --network-concurrency "$concurrency" \
    --log-level error
  end_ns="$(date +%s%N)"

  wait "$server_pid"
  elapsed_ms="$(((end_ns - start_ns) / 1000000))"
  copied="$(find "$target" -type f | wc -l)"
  printf 'case concurrency=%s copied=%s wall_ms=%s\n' "$concurrency" "$copied" "$elapsed_ms"
}

offset=0
for concurrency in $concurrency_list; do
  run_case "$concurrency" "$((port_base + offset))"
  offset="$((offset + 1))"
done
