#!/usr/bin/env bash
# run.sh — run the pipeline script tests. See README.md.
#   .conductor/tests/run.sh                    # every test-*.sh
#   .conductor/tests/run.sh test-fmt-fix.sh    # named files only
# Plain bash (3.2 compatible), jq, git and rustfmt; no other dependencies.
set -uo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$TESTS_DIR/../.." && pwd)"
export TESTS_DIR REPO_ROOT SCRIPTS="$REPO_ROOT/.conductor/scripts"

# A run inside a workflow step must behave like a local one.
for v in $(env | sed -n 's/^\(\(CONDUCTOR\|FLOW\)_[A-Za-z0-9_]*\)=.*/\1/p'); do
  unset "$v"
done
unset BASE_BRANCH SCOPE SINCE PRIOR_CONTEXTS PRIOR_OUTPUT FMT_FIX_ALLOW_FAILURE

if [ "$#" -gt 0 ]; then
  files=()
  for f in "$@"; do
    case "$f" in /*) files+=("$f") ;; *) files+=("$TESTS_DIR/$(basename "$f")") ;; esac
  done
else
  files=("$TESTS_DIR"/test-*.sh)
fi

total_failed=0
total_passed=0
for f in "${files[@]}"; do
  if [ ! -f "$f" ]; then
    echo "FAIL  $f: no such test file"
    total_failed=$((total_failed + 1))
    continue
  fi
  echo "== $(basename "$f")"
  out=$(mktemp)
  # Each file runs in its own bash so state and cwd never leak between files.
  bash "$f" >"$out" 2>&1
  rc=$?
  cat "$out"
  p=$(grep -c '^PASS  ' "$out" || true)
  n=$(grep -c '^FAIL  ' "$out" || true)
  rm -f "$out"
  total_passed=$((total_passed + p))
  total_failed=$((total_failed + n))
  if [ "$rc" -ne 0 ] && [ "$n" -eq 0 ]; then
    echo "FAIL  $(basename "$f"): exited $rc without reporting a failed case"
    total_failed=$((total_failed + 1))
  fi
done

echo
echo "passed: $total_passed, failed: $total_failed"
[ "$total_failed" -eq 0 ]
