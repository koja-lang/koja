#!/usr/bin/env bash
#
# Build and run the Koja benchmarks, comparing against BEAM baselines when
# Erlang is installed. Each program brackets just its workload with
# DateTime.now()/monotonic_time and prints a "<name>_ms <value>" line, so the
# numbers exclude VM startup and compilation. We run each program a few times
# and report the median (plus best).
#
# Usage:
#   ./run.sh            # default 3 runs each
#   RUNS=10 ./run.sh    # more runs for tighter medians

set -euo pipefail

RUNS="${RUNS:-3}"
HERE="$(cd "$(dirname "$0")" && pwd)"
TMP="$(mktemp -d)"
BUILD="$TMP/build"
mkdir -p "$BUILD"
trap 'rm -rf "$TMP"' EXIT

KOJA_BINS=(loop recursion msg_roundtrip spawn_reply process_storm)
BEAM_MODULES=(compute concurrency storm)

echo "Building Koja benchmarks (--release)..."
for name in "${KOJA_BINS[@]}"; do
  koja build --release -o "$BUILD/$name" "$HERE/koja/$name.kojs" >/dev/null
done

HAVE_BEAM=0
if command -v erlc >/dev/null 2>&1; then
  HAVE_BEAM=1
  echo "Compiling BEAM baselines..."
  for m in "${BEAM_MODULES[@]}"; do
    erlc -o "$BUILD" "$HERE/beam/$m.erl" >/dev/null
  done
else
  echo "Erlang not found (erlc); skipping BEAM baselines."
fi

# Parse every "<label>_ms <value>" pair from stdin and append the value to a
# per-engine, per-label file under $TMP. Tolerates Koja's quoted output.
record() {
  awk -v engine="$1" -v tmp="$TMP" '
    { gsub(/"/, "") }
    {
      for (i = 1; i < NF; i++) {
        if ($i ~ /_ms$/ && $(i + 1) ~ /^[0-9]+$/) {
          print $(i + 1) >> (tmp "/" engine "." $i)
        }
      }
    }'
}

echo "Running Koja ($RUNS runs each)..."
for ((r = 0; r < RUNS; r++)); do
  for b in "${KOJA_BINS[@]}"; do
    "$BUILD/$b" | record koja
  done
done

if [ "$HAVE_BEAM" -eq 1 ]; then
  echo "Running BEAM ($RUNS runs each)..."
  for ((r = 0; r < RUNS; r++)); do
    for m in "${BEAM_MODULES[@]}"; do
      erl -noshell -pa "$BUILD" -run "$m" main -s init stop | record beam
    done
  done
fi

# Print "<best> <median>" for a value file, or "- -" when missing.
stat_of() {
  if [ ! -s "$1" ]; then echo "- -"; return; fi
  sort -n "$1" | awk '
    { a[NR] = $1 }
    END {
      med = (NR % 2) ? a[int(NR / 2) + 1] : int((a[NR / 2] + a[NR / 2 + 1]) / 2)
      print a[1] " " med
    }'
}

LABELS=(loop_ms recursion_ms msg_ms spawn_ms storm_ms)
NAMES=("Tight loop (200M)" "Recursive fib(35)" "Msg round-trip (1M)" "Spawn + reply (100k)" "10k process storm")

printf '\n%-22s %14s %14s %12s\n' "Benchmark" "Koja med (ms)" "BEAM med (ms)" "Koja/BEAM"
printf '%s\n' "--------------------------------------------------------------------"
for idx in "${!LABELS[@]}"; do
  read -r _kbest kmed <<<"$(stat_of "$TMP/koja.${LABELS[$idx]}")"
  read -r _bbest bmed <<<"$(stat_of "$TMP/beam.${LABELS[$idx]}")"
  ratio="-"
  if [ "$kmed" != "-" ] && [ "$bmed" != "-" ] && [ "$bmed" -ne 0 ]; then
    ratio="$(awk -v k="$kmed" -v b="$bmed" 'BEGIN { printf "%.2fx", k / b }')"
  fi
  printf '%-22s %14s %14s %12s\n' "${NAMES[$idx]}" "$kmed" "$bmed" "$ratio"
done
echo
