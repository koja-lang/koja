#!/usr/bin/env bash
#
# Overnight scheduler endurance run. Cycles four phases until the deadline,
# collecting failures instead of stopping, so a full night of machine time
# turns into evidence either way:
#
#   stress  debug builds of the invariant tests (kill storm, monitor churn,
#           spawn-during-kill, scheduler stress) at cranked parameters,
#           with debug asserts and the lock-hierarchy check live
#   tsan    repeated ThreadSanitizer runs of the stress soak, each run
#           sampling different interleavings
#   bench   the release benchmark programs, medians appended to a CSV so
#           drift across the night is reviewable at a glance
#   churn   a long spawn/call/kill loop with RSS sampled every few seconds,
#           watching for slow leaks
#
# Results land in benchmarks/soak/results/<timestamp>/. Any unexpected
# failure is preserved in failures/ with its full log. Known-benign TSan
# fiber SEGVs (see the tsan justfile recipe) are counted, not preserved.
#
# Usage:
#   ./overnight.sh                 # 8 hours
#   DURATION_HOURS=12 ./overnight.sh
#   DURATION_SECS=60 ./overnight.sh   # smoke-test the harness itself
#
# Knobs (env): DURATION_HOURS (or DURATION_SECS), BENCH_RUNS, CHURN_SECS,
# TSAN_RUNS_PER_CYCLE.

set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"

DURATION_HOURS="${DURATION_HOURS:-8}"
DURATION_SECS="${DURATION_SECS:-$((DURATION_HOURS * 3600))}"
BENCH_RUNS="${BENCH_RUNS:-3}"
CHURN_SECS="${CHURN_SECS:-120}"
TSAN_RUNS_PER_CYCLE="${TSAN_RUNS_PER_CYCLE:-10}"

# Keep the machine awake for the whole run (harmless off macOS).
if [ "${OVERNIGHT_CAFFEINATED:-}" != 1 ] && command -v caffeinate >/dev/null 2>&1; then
  exec env OVERNIGHT_CAFFEINATED=1 caffeinate -is "$0" "$@"
fi

RUN_DIR="$HERE/results/$(date +%Y%m%d-%H%M%S)"
BIN="$RUN_DIR/bin"
FAILURES="$RUN_DIR/failures"
mkdir -p "$BIN" "$FAILURES"
BENCH_CSV="$RUN_DIR/bench.csv"
CHURN_CSV="$RUN_DIR/churn.csv"
LOG="$RUN_DIR/overnight.log"

log() {
  echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG"
}

CYCLES=0
UNEXPECTED_FAILURES=0
TSAN_RACES=0
TSAN_FIBER_FLAKES=0

summary() {
  log "===== summary ====="
  log "cycles completed:    $CYCLES"
  log "unexpected failures: $UNEXPECTED_FAILURES (logs in $FAILURES)"
  log "tsan data races:     $TSAN_RACES"
  log "tsan fiber flakes:   $TSAN_FIBER_FLAKES (known-benign, see justfile)"
  log "bench drift:         $BENCH_CSV"
  log "churn rss trend:     $CHURN_CSV"
}
trap 'log "interrupted"; summary; exit 130' INT TERM

# Runs one command to completion, preserving its log on unexpected failure.
# $1 is a short label for the failure file, the rest is the command.
run_logged() {
  local label="$1"
  shift
  local out="$RUN_DIR/last.$label.log"
  if ! "$@" >"$out" 2>&1; then
    UNEXPECTED_FAILURES=$((UNEXPECTED_FAILURES + 1))
    cp "$out" "$FAILURES/cycle$CYCLES.$label.log"
    log "FAILURE in $label (cycle $CYCLES), log preserved"
  fi
}

# ---- prep: build everything once up front ----------------------------------

command -v koja >/dev/null || { echo "koja not on PATH (run: just install)"; exit 1; }

HOST_TRIPLE="$(rustc -vV | sed -n 's/host: //p')"
TSAN_ENABLED=1
if ! cargo +nightly --version >/dev/null 2>&1; then
  log "nightly toolchain missing, skipping tsan phase"
  TSAN_ENABLED=0
fi

log "building debug stress tests"
(cd "$ROOT" && cargo test -p koja-runtime-posix --tests --no-run) >>"$LOG" 2>&1 || {
  echo "stress build failed, see $LOG"
  exit 1
}

if [ "$TSAN_ENABLED" = 1 ]; then
  log "building tsan stress binary (build-std, takes a few minutes cold)"
  (cd "$ROOT" && RUSTFLAGS="-Zsanitizer=thread --cfg koja_tsan" \
    cargo +nightly test -p koja-runtime-posix --test scheduler_stress \
    -Zbuild-std --target "$HOST_TRIPLE" --no-run) >>"$LOG" 2>&1 || {
    log "tsan build failed, skipping tsan phase"
    TSAN_ENABLED=0
  }
fi

log "building release benchmark and churn binaries"
KOJA_BINS=(loop recursion tail_scan msg_roundtrip spawn_reply process_storm)
for name in "${KOJA_BINS[@]}"; do
  koja build --release -o "$BIN/$name" "$HERE/../koja/$name.kojs" >>"$LOG" 2>&1
done
koja build --release -o "$BIN/churn" "$HERE/churn.kojs" >>"$LOG" 2>&1

echo "cycle,timestamp,loop_ms,recursion_ms,tail_scan_ms,msg_ms,spawn_ms,storm_ms" >"$BENCH_CSV"
echo "cycle,elapsed_secs,rss_kb" >"$CHURN_CSV"

# ---- phases -----------------------------------------------------------------

stress_phase() {
  run_logged kill_storm env \
    KOJA_STRESS_KILL_WAVES=80 KOJA_STRESS_KILL_WAVE_SIZE=48 \
    cargo test -p koja-runtime-posix --test kill_storm -- --nocapture
  run_logged monitor_churn env \
    KOJA_STRESS_MONITOR_ROUNDS=1500 \
    cargo test -p koja-runtime-posix --test monitor_churn -- --nocapture
  run_logged spawn_during_kill env \
    KOJA_STRESS_BREED_ROUNDS=150 KOJA_STRESS_BROOD_SIZE=32 \
    cargo test -p koja-runtime-posix --test spawn_during_kill -- --nocapture
  run_logged scheduler_stress env \
    KOJA_STRESS_CHILDREN=32 KOJA_STRESS_ROUNDS=1000 \
    cargo test -p koja-runtime-posix --test scheduler_stress -- --nocapture
}

tsan_phase() {
  [ "$TSAN_ENABLED" = 1 ] || return 0
  local out="$RUN_DIR/last.tsan.log"
  for ((t = 0; t < TSAN_RUNS_PER_CYCLE; t++)); do
    if RUSTFLAGS="-Zsanitizer=thread --cfg koja_tsan" \
      TSAN_OPTIONS="suppressions=$ROOT/crates/koja-runtime-posix/tsan.supp" \
      KOJA_STRESS_CHILDREN=16 KOJA_STRESS_ROUNDS=400 \
      KOJA_STRESS_WAVES=0 KOJA_STRESS_STEAL_BURST=0 \
      cargo +nightly test -p koja-runtime-posix --test scheduler_stress \
      -Zbuild-std --target "$HOST_TRIPLE" -- --nocapture --test-threads=1 \
      >"$out" 2>&1; then
      continue
    fi
    if grep -q 'WARNING: ThreadSanitizer' "$out"; then
      TSAN_RACES=$((TSAN_RACES + 1))
      cp "$out" "$FAILURES/cycle$CYCLES.tsan-race-$t.log"
      log "TSAN DATA RACE (cycle $CYCLES), log preserved"
    elif grep -q 'DEADLYSIGNAL' "$out"; then
      TSAN_FIBER_FLAKES=$((TSAN_FIBER_FLAKES + 1))
    else
      UNEXPECTED_FAILURES=$((UNEXPECTED_FAILURES + 1))
      cp "$out" "$FAILURES/cycle$CYCLES.tsan-other-$t.log"
      log "unexpected tsan failure (cycle $CYCLES), log preserved"
    fi
  done
}

# Median of the "<label>_ms <value>" line printed by $2 across BENCH_RUNS runs.
bench_median() {
  local label="$1" bin="$2" values
  values=$(for ((r = 0; r < BENCH_RUNS; r++)); do
    "$bin" | tr -d '"' | awk -v l="$label" '{for (i = 1; i < NF; i++) if ($i == l) print $(i + 1)}'
  done | sort -n)
  echo "$values" | awk '{a[NR] = $1} END {print (NR % 2) ? a[int(NR / 2) + 1] : int((a[NR / 2] + a[NR / 2 + 1]) / 2)}'
}

bench_phase() {
  local row="$CYCLES,$(date +%H:%M:%S)"
  local labels=(loop_ms recursion_ms tail_scan_ms msg_ms spawn_ms storm_ms)
  for idx in "${!KOJA_BINS[@]}"; do
    row="$row,$(bench_median "${labels[$idx]}" "$BIN/${KOJA_BINS[$idx]}")"
  done
  echo "$row" >>"$BENCH_CSV"
  log "bench: $row"
}

churn_phase() {
  "$BIN/churn" >/dev/null 2>&1 &
  local pid=$! start elapsed rss
  start=$(date +%s)
  while true; do
    sleep 5
    kill -0 "$pid" 2>/dev/null || break
    elapsed=$(($(date +%s) - start))
    rss=$(ps -o rss= -p "$pid" | tr -d ' ')
    echo "$CYCLES,$elapsed,$rss" >>"$CHURN_CSV"
    if [ "$elapsed" -ge "$CHURN_SECS" ]; then
      kill "$pid" 2>/dev/null
      wait "$pid" 2>/dev/null
      break
    fi
  done
  log "churn: final rss ${rss:-?} KB after ${elapsed:-0}s"
}

# ---- main loop --------------------------------------------------------------

DEADLINE=$(($(date +%s) + DURATION_SECS))
log "starting: ${DURATION_SECS}s, results in $RUN_DIR"

cd "$ROOT"
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  CYCLES=$((CYCLES + 1))
  log "cycle $CYCLES"
  stress_phase
  tsan_phase
  bench_phase
  churn_phase
done

summary
[ "$UNEXPECTED_FAILURES" -eq 0 ] && [ "$TSAN_RACES" -eq 0 ]
