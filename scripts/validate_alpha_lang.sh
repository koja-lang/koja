#!/usr/bin/env bash
# Walk every tests/lang/ fixture and run it through `expo alpha run --backend=llvm`.
# Reports PASS/FAIL per fixture; intended as one-shot triage during the v1 → alpha
# parity migration. Delete once `lang_suite.rs` flips its runner.
#
# Usage:
#   expo/scripts/validate_alpha_lang.sh            # run everything
#   expo/scripts/validate_alpha_lang.sh basics     # filter to fixture dir(s)
#
# Skips `compile_fail` and `runtime_fail` (failure-mode goldens, separate triage)
# and the signal-driven `process_lifecycle` (needs a wrapper around SIGTERM).

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LANG_DIR="$REPO_ROOT/tests/lang"
EXPO_BIN="${EXPO_BIN:-$HOME/.local/bin/expo}"

if [[ ! -x "$EXPO_BIN" ]]; then
  echo "error: expo binary not found at $EXPO_BIN; run \`just install\` first" >&2
  exit 2
fi

cd "$LANG_DIR"

passed=0
failed=0
skipped=0
failures=()

run_single_file() {
  local fixture_path="$1"
  local label="$2"
  local expected_path="${fixture_path%.expo}.stdout"
  if [[ ! -f "$expected_path" ]]; then
    skipped=$((skipped + 1))
    return
  fi
  local actual
  actual="$("$EXPO_BIN" alpha run --backend=llvm "$fixture_path" 2>/dev/null)"
  local code=$?
  if [[ $code -ne 0 ]]; then
    failed=$((failed + 1))
    failures+=("$label  [exit $code]")
    return
  fi
  local expected
  expected="$(cat "$expected_path")"
  if [[ "$actual" == "$expected" ]]; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    failures+=("$label  [stdout mismatch]")
  fi
}

run_project_dir() {
  local dir="$1"
  local label="$2"
  local expected_path="$dir/expected.stdout"
  if [[ ! -f "$expected_path" ]]; then
    skipped=$((skipped + 1))
    return
  fi
  local actual
  actual="$(cd "$dir" && "$EXPO_BIN" alpha run --backend=llvm 2>/dev/null)"
  local code=$?
  local expected_code=0
  if [[ -f "$dir/expected.exit_code" ]]; then
    expected_code="$(cat "$dir/expected.exit_code")"
  fi
  if [[ $code -ne $expected_code ]]; then
    failed=$((failed + 1))
    failures+=("$label  [exit $code, expected $expected_code]")
    return
  fi
  local expected
  expected="$(cat "$expected_path")"
  if [[ "$actual" == "$expected" ]]; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    failures+=("$label  [stdout mismatch]")
  fi
}

fixture_dirs=()
if [[ $# -gt 0 ]]; then
  for arg in "$@"; do
    fixture_dirs+=("$arg")
  done
else
  for d in */; do
    fixture_dirs+=("${d%/}")
  done
fi

for fixture_dir in "${fixture_dirs[@]}"; do
  case "$fixture_dir" in
    compile_fail|runtime_fail|process_lifecycle)
      echo "SKIP  $fixture_dir/  (failure-mode or signal-driven)"
      continue
      ;;
  esac
  if [[ ! -d "$fixture_dir" ]]; then
    echo "warn: $fixture_dir is not a directory under tests/lang/" >&2
    continue
  fi

  if [[ -f "$fixture_dir/expo.toml" ]]; then
    label="$fixture_dir"
    before=$failed
    run_project_dir "$fixture_dir" "$label"
    if [[ $failed -eq $before ]]; then
      echo "PASS  $label"
    else
      echo "FAIL  $label"
    fi
    continue
  fi

  for expo_file in "$fixture_dir"/*.expo; do
    [[ -f "$expo_file" ]] || continue
    label="$fixture_dir/$(basename "$expo_file" .expo)"
    before=$failed
    run_single_file "$expo_file" "$label"
    if [[ $failed -eq $before ]]; then
      echo "PASS  $label"
    else
      echo "FAIL  $label"
    fi
  done
done

echo ""
echo "summary: $passed passed, $failed failed, $skipped skipped"
if [[ $failed -gt 0 ]]; then
  echo ""
  echo "failures:"
  for f in "${failures[@]}"; do
    echo "  $f"
  done
  exit 1
fi
