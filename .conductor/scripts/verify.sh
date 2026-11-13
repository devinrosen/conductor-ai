#!/usr/bin/env bash
# verify.sh — the cargo checks that gate a PR, decided by exit codes (#3065).
#
# Replaces the verify agent. Maps the changed files to crates, then runs:
#   cargo build
#   cargo nextest run -p <crate> --no-fail-fast --no-tests=pass (conductor-core with --features test-helpers)
#   cargo clippy -p <crate> --all-targets -- -D warnings
#   cargo fmt --all --check
# A root Cargo.toml or Cargo.lock change checks every crate. Only .rs files,
# Cargo.toml, Cargo.lock and build.rs count as Rust changes. A change under
# .conductor/ or conductor-core/templates/ also runs
#   conductor workflow validate --all --templates
# with the conductor just built from this worktree (#3290), so a pipeline bug
# such as an unknown gate type (#3281) goes back to implement. A change under
# .conductor/scripts/ or .conductor/tests/ also runs
#   bash .conductor/tests/run.sh
# (the pipeline script fixture tests, #3347). It needs no cargo build, so it
# runs even when the build fails. Anything else (.md, frontend files) skips
# the checks.
#
# Emits has_failures when any check failed or couldn't run, with a context
# that names the failing checks, test names and the first compiler or clippy
# errors (or the failing script test cases), which implement reads as its prior
# context. The FLOW_OUTPUT also
# carries `passed` and `checks` ({name, status, summary}) as structured output.
#
# Env: FEATURE_BASE_BRANCH (default main). VERIFY_CRATES overrides the crate
# list (space-separated), for testing.
#
# Call sites: ticket-to-pr.wf, iterate-pr.wf and quick-fix-pr.wf call this as
# `script verify` with FEATURE_BASE_BRANCH = {{feature_base_branch}} (twice in
# the first two, four times in quick-fix-pr), as does the quick-fix-pr.wft
# template. A change to the path or env must be made in every one. A shared
# sub-workflow would hide verify's markers behind the call's name, which the
# callers' `if verify.has_failures` and fail-verify's resume guidance rely on.
set -uo pipefail

base="${FEATURE_BASE_BRANCH:-main}"

# The engine exports the workflow's inputs and context to script steps as
# CONDUCTOR_* and FLOW_* variables (CONDUCTOR_TICKET_ID and the like). Every
# cargo command below would pass them on to the tests, so a test that reads
# the environment behaves differently inside a run than locally or in CI
# (#3311). Unset them so the checks see the developer's environment. Keep
# CONDUCTOR_HOME and CONDUCTOR_DB_PATH: they're set by whoever runs verify, not
# by the engine, and `conductor workflow validate` below must not fall back to
# the real ~/.conductor when verify runs against a scratch home.
while IFS= read -r var; do
  unset "${var}"
done < <(compgen -e | grep -E '^(CONDUCTOR|FLOW)_' | grep -vxE 'CONDUCTOR_(HOME|DB_PATH)')

all_crates=(conductor-core conductor-cli conductor-tui conductor-web)
max_context=6000

checks_json='[]'
failed=0
fail_details=""
pass_names=""

emit() {
  # $1 = markers JSON, $2 = context, $3 = passed (true/false)
  echo "<<<FLOW_OUTPUT>>>"
  jq -cn --argjson markers "$1" --arg ctx "$2" --argjson passed "$3" --argjson checks "${checks_json}" \
    '{markers: $markers, context: $ctx, passed: $passed, checks: $checks}'
  echo "<<<END_FLOW_OUTPUT>>>"
}

# record NAME STATUS SUMMARY [DETAIL]
record() {
  checks_json=$(jq -c --arg n "$1" --arg s "$2" --arg m "$3" '. + [{name: $n, status: $s, summary: $m}]' <<< "${checks_json}")
  case "$2" in
    fail)
      failed=1
      fail_details="${fail_details}
[$1] $3${4:+
$4}"
      ;;
    pass) pass_names="${pass_names:+${pass_names}, }$1" ;;
  esac
}

# --- Which crates changed --------------------------------------------------
# Only a successful diff can skip the checks: if the merge base or the diff
# fails, check every crate, since skipping would hide breakage.
crates=()
validate_pipeline=0
script_tests=0
if [ -n "${VERIFY_CRATES:-}" ]; then
  read -ra crates <<< "${VERIFY_CRATES}"
elif ! merge_base=$(git merge-base HEAD "origin/${base}" 2>/dev/null || git merge-base HEAD "${base}" 2>/dev/null); then
  echo "WARNING: could not find the merge base with ${base}; checking every crate" >&2
  crates=("${all_crates[@]}")
  validate_pipeline=1
  script_tests=1
elif ! changed=$(git diff --name-only "${merge_base}") || ! untracked=$(git ls-files --others --exclude-standard); then
  echo "WARNING: git could not list the changed files; checking every crate" >&2
  crates=("${all_crates[@]}")
  validate_pipeline=1
  script_tests=1
else
  all_changed=$(printf '%s\n%s\n' "${changed}" "${untracked}")
  rust_files=$(printf '%s\n' "${all_changed}" | grep -E '\.rs$|(^|/)Cargo\.(toml|lock)$|(^|/)build\.rs$' || true)
  if printf '%s\n' "${all_changed}" | grep -qE '^(\.conductor/|conductor-core/templates/)'; then
    validate_pipeline=1
  fi
  if printf '%s\n' "${all_changed}" | grep -qE '^\.conductor/(scripts|tests)/'; then
    script_tests=1
  fi
  if [ -z "${rust_files}" ] && [ "${validate_pipeline}" -eq 0 ]; then
    emit '[]' "PASS: no Rust or pipeline changes; checks skipped." true
    exit 0
  fi
  if [ -z "${rust_files}" ]; then
    : # pipeline files only: build conductor and validate, no per-crate checks
  elif printf '%s\n' "${rust_files}" | grep -qE '^Cargo\.(toml|lock)$'; then
    crates=("${all_crates[@]}")
  else
    for crate in "${all_crates[@]}"; do
      if printf '%s\n' "${rust_files}" | grep -q "^${crate}/"; then
        crates+=("${crate}")
      fi
    done
  fi
fi

logs=$(mktemp -d)
trap 'rm -rf "${logs}"' EXIT

# first_errors LOG — the first compiler or clippy errors, else the log's tail.
first_errors() {
  local out
  out=$(grep -E -A10 '^error(\[E[0-9]+\])?:' "$1" | head -n 60)
  [ -n "${out}" ] || out=$(tail -n 30 "$1")
  printf '%s' "${out}"
}

# --- cargo build -------------------------------------------------------------
# The other checks would only repeat a build failure, so they're skipped then.
build_ok=0
if cargo build > "${logs}/build.log" 2>&1; then
  build_ok=1
  record "cargo build" pass "ok"
else
  record "cargo build" fail "build failed" "$(first_errors "${logs}/build.log")"
  for crate in ${crates[@]+"${crates[@]}"}; do
    record "cargo nextest -p ${crate}" skipped "not run: cargo build failed"
    record "cargo clippy -p ${crate}" skipped "not run: cargo build failed"
  done
fi

if [ "${failed}" -eq 0 ]; then
  # --- cargo nextest, per crate ----------------------------------------------
  for crate in ${crates[@]+"${crates[@]}"}; do
    features=()
    [ "${crate}" = "conductor-core" ] && features=(--features test-helpers)
    log="${logs}/nextest-${crate}.log"
    nextest_status=0
    cargo nextest run -p "${crate}" --no-fail-fast --no-tests=pass "${features[@]+"${features[@]}"}" > "${log}" 2>&1 || nextest_status=$?
    summary=$(grep -E '^\s*Summary \[' "${log}" | tail -n 1 | sed -E 's/^[[:space:]]*Summary \[[^]]*\][[:space:]]*//')
    if [ "${nextest_status}" -eq 0 ]; then
      record "cargo nextest -p ${crate}" pass "${summary:-passed}"
    else
      failing=$(grep -E '^\s*(FAIL|TIMEOUT|SIGSEGV|SIGABRT) \[' "${log}" | sed -E 's/^[[:space:]]*[A-Z]+ \[[^]]*\][[:space:]]*(\([0-9]+\/[0-9]+\)[[:space:]]*)?//' | sort -u | head -n 20)
      if [ -n "${failing}" ]; then
        panics=$(grep -E "panicked at|assertion .* failed|left: |right: " "${log}" | head -n 20)
        record "cargo nextest -p ${crate}" fail "${summary:-tests failed}" "Failing tests:
${failing}${panics:+
${panics}}"
      else
        # No test failed, so it didn't compile or nextest couldn't run.
        record "cargo nextest -p ${crate}" fail "nextest exited ${nextest_status} without a test result" "$(first_errors "${log}")"
      fi
    fi
  done

  # --- cargo clippy, per crate -----------------------------------------------
  for crate in ${crates[@]+"${crates[@]}"}; do
    log="${logs}/clippy-${crate}.log"
    if cargo clippy -p "${crate}" --all-targets -- -D warnings > "${log}" 2>&1; then
      record "cargo clippy -p ${crate}" pass "no warnings"
    else
      record "cargo clippy -p ${crate}" fail "clippy reported errors" "$(first_errors "${log}")"
    fi
  done
fi

# --- conductor workflow validate ------------------------------------------------
# Uses the conductor built above from this worktree, so a change to the
# validator itself is checked by the new code.
if [ "${validate_pipeline}" -eq 1 ]; then
  if [ "${build_ok}" -eq 0 ]; then
    record "conductor workflow validate" skipped "not run: cargo build failed"
  elif cargo run -q --bin conductor -- workflow validate --all --templates --path . > "${logs}/validate.log" 2>&1; then
    record "conductor workflow validate" pass "$(tail -n 1 "${logs}/validate.log")"
  else
    record "conductor workflow validate" fail "workflow or template validation failed" "$(grep -E '^(FAIL|\s*✗)' "${logs}/validate.log" | head -n 40)"
  fi
fi

# --- pipeline script tests -----------------------------------------------------
# Needs no cargo build, so it runs even when the build failed.
if [ "${script_tests}" -eq 1 ]; then
  log="${logs}/script-tests.log"
  if [ ! -f .conductor/tests/run.sh ]; then
    record "pipeline script tests" skipped "not run: no .conductor/tests/run.sh"
  else
    script_status=0
    bash .conductor/tests/run.sh > "${log}" 2>&1 || script_status=$?
    summary=$(grep -E '^passed: [0-9]+, failed: [0-9]+$' "${log}" | tail -n 1)
    if [ "${script_status}" -eq 0 ]; then
      record "pipeline script tests" pass "${summary:-passed}"
    else
      detail=$(grep -E -A3 '^FAIL  ' "${log}" | head -n 60)
      [ -n "${detail}" ] || detail=$(tail -n 30 "${log}")
      record "pipeline script tests" fail "${summary:-run.sh exited ${script_status}}" "Failing cases:
${detail}"
    fi
  fi
fi

# --- cargo fmt -----------------------------------------------------------------
# fmt-fix runs just before verify, so a failure here means rustfmt itself
# couldn't format the code, usually a parse error.
if cargo fmt --all --check -- --color never > "${logs}/fmt.log" 2>&1; then
  record "cargo fmt" pass "ok"
else
  record "cargo fmt" fail "cargo fmt --all --check failed after fmt-fix (rustfmt couldn't format the code, usually a parse error)" "$(head -n 30 "${logs}/fmt.log")"
fi

if [ "${failed}" -eq 0 ]; then
  emit '[]' "PASS: ${pass_names}." true
else
  context="FAIL:${fail_details}"
  context="${context:0:${max_context}}"
  [ -n "${pass_names}" ] && context="${context}
PASS: ${pass_names}."
  emit '["has_failures"]' "${context}" false
fi
