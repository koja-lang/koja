#!/usr/bin/env bash
# Regenerate the IRInstruction::Stub fall-through inventory.
#
# Drives the Stub-retirement workflow:
#   1. Builds + installs the current `expo` binary so all subprocess-based
#      tests use the latest lowering code.
#   2. Re-runs the lang_suite and stdlib package tests with
#      `EXPO_STUB_INVENTORY_FILE` set so each compiler invocation appends
#      its `[STUB-FALLTHROUGH]` (and `[HELPER-BAIL]`) lines to
#      stub/stub-{driver,stdlib}.log.
#   3. Combines + dedupes both logs into stub/stub-inventory.txt.
#   4. Prints per-ExprKind counts plus the sealing-progress metric
#      (distinct ExprKinds still hitting Stub / 28).
#
# Sister script to expo/stub/stub-categorization.md, which is the
# tracked deliverable; the .log + .txt files are gitignored.

set -euo pipefail
cd "$(dirname "$0")/.."

DRIVER_LOG="$(pwd)/stub/stub-driver.log"
STDLIB_LOG="$(pwd)/stub/stub-stdlib.log"
INVENTORY="$(pwd)/stub/stub-inventory.txt"

rm -f "$DRIVER_LOG" "$STDLIB_LOG" "$INVENTORY"

echo "=== installing fresh expo binary ==="
just install

echo
echo "=== running lang_suite (driver) ==="
EXPO_STUB_INVENTORY_FILE="$DRIVER_LOG" \
    CARGO_TARGET_DIR=target \
    cargo test -p expo-driver --test lang_suite -- --test-threads=1

echo
echo "=== running stdlib packages ==="
export EXPO_STUB_INVENTORY_FILE="$STDLIB_LOG"
stdlib_failed=0
for pkg in lib/*/; do
    if [ -d "${pkg}test" ]; then
        name=$(basename "$pkg")
        echo "--- testing $name ---"
        (cd "$pkg" && expo test) || stdlib_failed=1
    fi
done
unset EXPO_STUB_INVENTORY_FILE

if [ "$stdlib_failed" -ne 0 ]; then
    echo "stdlib tests failed -- inventory may be incomplete" >&2
    exit 1
fi

echo
echo "=== building inventory ==="
grep -h '\[STUB-FALLTHROUGH\]' "$DRIVER_LOG" "$STDLIB_LOG" | sort -u > "$INVENTORY"
total=$(wc -l < "$INVENTORY" | tr -d ' ')
echo "total unique (kind, function, span) triples: $total"

echo
echo "=== per-ExprKind counts ==="
rg -o 'ExprKind::\w+' "$INVENTORY" | sort | uniq -c | sort -rn

echo
echo "=== per-helper bail counts ==="
helper_bails=$(grep -h '\[HELPER-BAIL\]' "$DRIVER_LOG" "$STDLIB_LOG" | sort -u || true)
if [ -n "$helper_bails" ]; then
    echo "$helper_bails" | rg -o 'reason=[\w-]+' | sort | uniq -c | sort -rn
else
    echo "(no [HELPER-BAIL] lines yet -- per-branch instrumentation not landed)"
fi

echo
distinct=$(rg -o 'ExprKind::\w+' "$INVENTORY" | sort -u | wc -l | tr -d ' ')
printf 'sealing progress: %d / 28 ExprKinds still hitting Stub\n' "$distinct"
