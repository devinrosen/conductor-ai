#!/usr/bin/env bash
# precheck-tests.sh — cheap mechanical checks between implement and verify (#3238).
#
# 1. Every test target in the workspace compiles (`cargo test --workspace
#    --no-run`). This catches a new struct field left out of another module's
#    test literals, or a changed signature in a crate verify doesn't test.
# 2. No insta snapshot is pending. The snapshot tests run with
#    INSTA_UPDATE=new, and any `*.snap.new` file they write is reported with
#    its diff, so implement can accept an intended change (replace the .snap
#    with the .snap.new) or fix the code.
#
# Emits has_precheck_failures with the compiler errors or snapshot diffs as
# context, which implement reads as its prior context. verify still runs the
# full checks afterwards; this only saves a verify cycle on mechanical breakage.
#
# Env: FEATURE_BASE_BRANCH (default main).
set -uo pipefail

base="${FEATURE_BASE_BRANCH:-main}"
max_context=6000

emit() {
  echo "<<<FLOW_OUTPUT>>>"
  jq -cn --argjson markers "$1" --arg ctx "$2" '{markers: $markers, context: $ctx}'
  echo "<<<END_FLOW_OUTPUT>>>"
}

# Changed files against the merge base. If the base can't be resolved, run the
# checks anyway: skipping them would hide breakage, running them only costs time.
changed=""
if merge_base=$(git merge-base HEAD "origin/${base}" 2>/dev/null || git merge-base HEAD "${base}" 2>/dev/null); then
  changed=$(git diff --name-only "${merge_base}" 2>/dev/null; git ls-files --others --exclude-standard)
  if ! printf '%s\n' "${changed}" | grep -qE '\.rs$|(^|/)Cargo\.(toml|lock)$|(^|/)build\.rs$|\.snap$'; then
    emit '[]' "PASS: no Rust changes; precheck skipped."
    exit 0
  fi
else
  echo "WARNING: could not find the merge base with ${base}; running the precheck anyway" >&2
fi

logs=$(mktemp -d)
trap 'rm -rf "${logs}"' EXIT
report=""

# --- 1. All test targets compile ------------------------------------------
if ! cargo test --workspace --no-run > "${logs}/no-run.log" 2>&1; then
  errors=$(grep -E -A12 '^error(\[E[0-9]+\])?:' "${logs}/no-run.log" | head -n 80)
  [ -z "${errors}" ] && errors=$(tail -n 40 "${logs}/no-run.log")
  report="Test targets don't compile (cargo test --workspace --no-run). Fix every error, including test code in other modules and crates that builds the types you changed:
${errors}"
  emit '["has_precheck_failures"]' "FAIL: ${report:0:${max_context}}"
  exit 0
fi

# --- 2. Pending insta snapshots --------------------------------------------
# Snapshot files live in <crate>/tests/snapshots/<test binary>__<name>.snap.
# Clear leftovers from earlier runs first so only this change's are reported.
find . -name '*.snap.new' -not -path './target/*' -delete 2>/dev/null || true
while IFS= read -r target; do
  [ -z "${target}" ] && continue
  crate="${target%% *}"
  test_bin="${target#* }"
  # A failing snapshot assertion fails the test; only the .snap.new files matter here.
  INSTA_UPDATE=new cargo test -p "${crate}" --test "${test_bin}" > "${logs}/snap-${crate}-${test_bin}.log" 2>&1 || true
done < <(git ls-files '*/tests/snapshots/*.snap' \
  | sed -E 's#^([^/]+)/tests/snapshots/([^/]+)__.*\.snap$#\1 \2#' | sort -u)

pending=$(find . -name '*.snap.new' -not -path './target/*' | sort)
if [ -n "${pending}" ]; then
  diffs=""
  while IFS= read -r new_file; do
    old_file="${new_file%.new}"
    diffs="${diffs}
--- ${old_file#./}
$(diff -u "${old_file}" "${new_file}" 2>/dev/null | tail -n +3 | head -n 40)"
  done <<< "${pending}"
  count=$(printf '%s\n' "${pending}" | wc -l | tr -d ' ')
  report="${count} insta snapshot(s) changed. For each, if the new rendering is what the change intends, replace the .snap with the .snap.new (mv); otherwise fix the code. Never commit a .snap.new file.${diffs}"
  emit '["has_precheck_failures"]' "FAIL: ${report:0:${max_context}}"
  exit 0
fi

emit '[]' "PASS: all workspace test targets compile; no pending snapshots."
