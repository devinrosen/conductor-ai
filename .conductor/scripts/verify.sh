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
# runs even when the build fails. A change to a .conductor/workflows/*.wf file or a
# conductor-core/templates/*.wft template also runs the dry-run smokes CLAUDE.md
# requires, through smoke-workflows.sh (#3453): `conductor workflow validate --dry-run`
# of each changed workflow and of each workflow that calls one, directly or through others. Anything else (.md,
# frontend files) skips the checks.
#
# The branch build must not migrate the real conductor database (#3469): the static
# validate opens the database read-only and never migrates it (#3470), so it needs
# no isolation, the smokes go through lib-branch-home.sh (a copy when this branch adds a migration), and a tripwire reads
# the real database's schema_version (read-only) before and after the checks. When it
# changed, verify fails: every installed binary older than this branch now refuses
# to open the database.
#
# Emits has_failures when any check failed or couldn't run, with a context
# that names the failing checks, test names and the first compiler or clippy
# errors (or the failing script test cases), which implement reads as its prior
# context. The FLOW_OUTPUT also
# carries `passed` and `checks` ({name, status, summary}) as structured output.
# It also carries a failure signature (#3486): `failures`, a sorted, unique list
# of `<check>: <item>` entries (one per compiler or clippy error and location,
# failing test, validate FAIL line, failed smoke or failing script test case; the
# check name alone for fmt, the schema tripwire and any failure whose log yields
# no item), capped at 200 entries, and `failure_count`, the full entry count.
# Skipped checks add nothing, and a pass emits [] and 0. verify-progress.sh reads
# the last two verifies' signatures to decide whether ticket-to-pr gets one more
# fix attempt.
#
# Flaky-test rerun (#3485): after the per-crate nextest pass, the test fns the diff
# adds or changes (#[test]/#[tokio::test] fns whose lines overlap a changed hunk) are
# rerun VERIFY_RERUNS more times with --retries 0, for the crates whose main nextest
# run passed. A failure on any rerun fails the `flaky tests` check, which names the
# test and the rerun, with one signature entry per failing test. The check is skipped
# above VERIFY_RERUN_MAX_TESTS changed tests and starts no new rerun after
# VERIFY_RERUN_BUDGET seconds. It records nothing when no test changed, and needs the
# diff (it is off under VERIFY_CRATES).
#
# Env: FEATURE_BASE_BRANCH (default main). VERIFY_CRATES overrides the crate
# list (space-separated), for testing. VERIFY_RERUNS (default 5),
# VERIFY_RERUN_MAX_TESTS (default 50) and VERIFY_RERUN_BUDGET (default 120, seconds)
# bound the flaky-test rerun.
#
# Call sites: ticket-to-pr.wf, iterate-pr.wf and quick-fix-pr.wf call this as
# `script verify` with FEATURE_BASE_BRANCH = {{feature_base_branch}} (twice in
# the first two, four times in quick-fix-pr), as does the quick-fix-pr.wft
# template. A change to the path or env must be made in every one. A shared
# sub-workflow would hide verify's markers behind the call's name, which the
# callers' `if verify.has_failures` and fail-verify's resume guidance rely on.
set -uo pipefail
# shellcheck source=lib-branch-home.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib-branch-home.sh"

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
smokes_json='[]'
failed=0
sig_entries=""
fail_details=""
pass_names=""

emit() {
  # $1 = markers JSON, $2 = context, $3 = passed (true/false)
  echo "<<<FLOW_OUTPUT>>>"
  jq -cn --argjson markers "$1" --arg ctx "$2" --argjson passed "$3" --argjson checks "${checks_json}" --argjson smokes "${smokes_json}" --arg sig "${sig_entries}" \
    '($sig | split("\n") | map(select(. != "")) | unique) as $f
     | {markers: $markers, context: $ctx, passed: $passed, checks: $checks, smokes: $smokes, failures: $f[0:200], failure_count: ($f | length)}'
  echo "<<<END_FLOW_OUTPUT>>>"
}

# error_sig LOG — one `header file:line:col` line per distinct compiler or clippy
# error, excluding the "could not compile" and "aborting" summary lines.
error_sig() {
  awk '
    /^error(\[E[0-9]+\])?:/ {
      if (hdr != "") print hdr
      if ($0 ~ /^error: (could not compile|aborting due to)/) { hdr = ""; next }
      hdr = $0; next
    }
    hdr != "" && /^[[:space:]]*-->/ { sub(/^[[:space:]]*--> */, ""); print hdr " @ " $0; hdr = "" }
    END { if (hdr != "") print hdr }
  ' "$1" | sort -u
}

# record NAME STATUS SUMMARY [DETAIL [ITEMS]]
# ITEMS (newline-separated) are the failure signature entries of a failing check;
# without any, the check name alone is the entry.
record() {
  checks_json=$(jq -c --arg n "$1" --arg s "$2" --arg m "$3" '. + [{name: $n, status: $s, summary: $m}]' <<< "${checks_json}")
  case "$2" in
    fail)
      failed=1
      if [ -n "${5:-}" ]; then
        sig_entries="${sig_entries}
$(printf '%s\n' "$5" | grep . | sed "s|^|$1: |")"
      else
        sig_entries="${sig_entries}
$1"
      fi
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
smoke_workflows=0
smoke_unknown=0
all_changed=""
if [ -n "${VERIFY_CRATES:-}" ]; then
  read -ra crates <<< "${VERIFY_CRATES}"
elif ! merge_base=$(git merge-base HEAD "origin/${base}" 2>/dev/null || git merge-base HEAD "${base}" 2>/dev/null); then
  echo "WARNING: could not find the merge base with ${base}; checking every crate" >&2
  crates=("${all_crates[@]}")
  validate_pipeline=1
  script_tests=1
  smoke_unknown=1
elif ! changed=$(git diff --name-only "${merge_base}") || ! untracked=$(git ls-files --others --exclude-standard); then
  echo "WARNING: git could not list the changed files; checking every crate" >&2
  crates=("${all_crates[@]}")
  validate_pipeline=1
  script_tests=1
  smoke_unknown=1
else
  all_changed=$(printf '%s\n%s\n' "${changed}" "${untracked}")
  rust_files=$(printf '%s\n' "${all_changed}" | grep -E '\.rs$|(^|/)Cargo\.(toml|lock)$|(^|/)build\.rs$' || true)
  if printf '%s\n' "${all_changed}" | grep -qE '^(\.conductor/|conductor-core/templates/)'; then
    validate_pipeline=1
  fi
  if printf '%s\n' "${all_changed}" | grep -qE '^(\.conductor/workflows/[^/]+\.wf|conductor-core/templates/[^/]+\.wft)$'; then
    smoke_workflows=1
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

# Tripwire: the real database's schema version before the checks (read-only).
schema_before=""
if ! schema_before=$(real_schema_version); then
  echo "WARNING: could not read the schema version of $(real_db_path) (${schema_before}); the schema tripwire is off" >&2
  schema_before=""
fi

# first_errors LOG — the first compiler or clippy errors, else the log's tail.
first_errors() {
  local out
  out=$(grep -E -A10 '^error(\[E[0-9]+\])?:' "$1" | head -n 60)
  [ -n "${out}" ] || out=$(tail -n 30 "$1")
  printf '%s' "${out}"
}

# changed_tests CRATE — the sorted, unique names of the test fns under CRATE/ whose
# lines overlap a changed line range of the diff against the merge base (an untracked
# file counts as wholly changed). A test starts at its #[test]-style attribute and ends
# at the closing brace at the fn's indentation, or at the next test attribute or EOF.
changed_tests() {
  local crate="$1" f ranges
  while IFS= read -r f; do
    case "${f}" in "${crate}/"*.rs) ;; *) continue ;; esac
    [ -f "${f}" ] || continue
    if git ls-files --error-unmatch -- "${f}" > /dev/null 2>&1; then
      ranges=$(git diff -U0 "${merge_base:-HEAD}" -- "${f}" | sed -n -E 's/^@@ -[0-9,]+ \+([0-9]+)(,([0-9]+))? @@.*/\1 \3/p' |
        awk '{ d = ($2 == "" ? 1 : $2); if (d == 0) d = 1; printf "%s %s;", $1, $1 + d - 1 }')
    else
      ranges="1 999999999;"
    fi
    [ -n "${ranges}" ] || continue
    awk -v r="${ranges}" '
      BEGIN { n = split(r, rs, ";"); for (i = 1; i <= n; i++) { split(rs[i], p, " "); lo[i] = p[1]; hi[i] = p[2] } }
      function flush(endl,   i) {
        if (name != "") for (i = 1; i <= n; i++) if (lo[i] != "" && lo[i] + 0 <= endl && hi[i] + 0 >= start) { print name; break }
        name = ""; inbody = 0
      }
      /^[[:space:]]*#\[([A-Za-z_]+::)*test(\(.*\))?\]/ { if (inbody) flush(NR - 1); pending = 1; start = NR; next }
      pending && /(^|[[:space:]])fn[[:space:]]+[A-Za-z_]/ {
        l = " " $0; sub(/^.*[[:space:]]fn[[:space:]]+/, "", l); match(l, /^[A-Za-z0-9_]+/); name = substr(l, 1, RLENGTH)
        match($0, /^[[:space:]]*/); ind = substr($0, 1, RLENGTH); inbody = 1; pending = 0; next
      }
      inbody && $0 == ind "}" { flush(NR); next }
      END { if (inbody) flush(NR) }
    ' "${f}"
  done <<< "${all_changed}" | sort -u
}

# --- cargo build -------------------------------------------------------------
# The other checks would only repeat a build failure, so they're skipped then.
build_ok=0
if cargo build > "${logs}/build.log" 2>&1; then
  build_ok=1
  record "cargo build" pass "ok"
else
  record "cargo build" fail "build failed" "$(first_errors "${logs}/build.log")" "$(error_sig "${logs}/build.log")"
  for crate in ${crates[@]+"${crates[@]}"}; do
    record "cargo nextest -p ${crate}" skipped "not run: cargo build failed"
    record "cargo clippy -p ${crate}" skipped "not run: cargo build failed"
  done
fi

if [ "${failed}" -eq 0 ]; then
  # --- cargo nextest, per crate ----------------------------------------------
  nextest_passed=""
  for crate in ${crates[@]+"${crates[@]}"}; do
    features=()
    [ "${crate}" = "conductor-core" ] && features=(--features test-helpers)
    log="${logs}/nextest-${crate}.log"
    nextest_status=0
    cargo nextest run -p "${crate}" --no-fail-fast --no-tests=pass "${features[@]+"${features[@]}"}" > "${log}" 2>&1 || nextest_status=$?
    summary=$(grep -E '^\s*Summary \[' "${log}" | tail -n 1 | sed -E 's/^[[:space:]]*Summary \[[^]]*\][[:space:]]*//')
    if [ "${nextest_status}" -eq 0 ]; then
      record "cargo nextest -p ${crate}" pass "${summary:-passed}"
      nextest_passed="${nextest_passed} ${crate}"
    else
      all_failing=$(grep -E '^\s*(FAIL|TIMEOUT|SIGSEGV|SIGABRT) \[' "${log}" | sed -E 's/^[[:space:]]*[A-Z]+ \[[^]]*\][[:space:]]*(\([0-9]+\/[0-9]+\)[[:space:]]*)?//' | sort -u)
      failing=$(printf '%s\n' "${all_failing}" | head -n 20 | grep . || true)
      if [ -n "${failing}" ]; then
        panics=$(grep -E "panicked at|assertion .* failed|left: |right: " "${log}" | head -n 20)
        record "cargo nextest -p ${crate}" fail "${summary:-tests failed}" "Failing tests:
${failing}${panics:+
${panics}}" "${all_failing}"
      else
        # No test failed, so it didn't compile or nextest couldn't run.
        record "cargo nextest -p ${crate}" fail "nextest exited ${nextest_status} without a test result" "$(first_errors "${log}")" "$(error_sig "${log}")"
      fi
    fi
  done

  # --- flaky-test rerun (#3485) ----------------------------------------------
  # The tests this diff adds or changes ran once above. Rerun just those, so a
  # timing-dependent new test fails here and not in CI. Only crates whose main run
  # passed take part: a failing crate goes back to implement first.
  rerun_n="${VERIFY_RERUNS:-5}"
  rerun_max="${VERIFY_RERUN_MAX_TESTS:-50}"
  rerun_budget="${VERIFY_RERUN_BUDGET:-120}"
  rerun_crates=()
  rerun_exprs=()
  rerun_total=0
  for crate in ${nextest_passed}; do
    names=$(changed_tests "${crate}")
    [ -n "${names}" ] || continue
    rerun_total=$((rerun_total + $(printf '%s\n' "${names}" | grep -c .)))
    rerun_crates+=("${crate}")
    rerun_exprs+=("test(/(^|::)($(printf '%s\n' "${names}" | paste -sd'|' -))\$/)")
  done
  if [ "${rerun_total}" -gt "${rerun_max}" ]; then
    record "flaky tests" skipped "not run: ${rerun_total} tests changed (limit ${rerun_max})"
  elif [ "${rerun_total}" -gt 0 ]; then
    rerun_start=${SECONDS}
    rerun_done=0
    rerun_capped=0
    rerun_fail_at=0
    rerun_fail_names=""
    rerun_fail_detail=""
    i=1
    while [ "${i}" -le "${rerun_n}" ]; do
      if [ "${i}" -gt 1 ] && [ $((SECONDS - rerun_start)) -ge "${rerun_budget}" ]; then
        rerun_capped=1
        break
      fi
      for idx in "${!rerun_crates[@]}"; do
        crate="${rerun_crates[${idx}]}"
        features=()
        [ "${crate}" = "conductor-core" ] && features=(--features test-helpers)
        log="${logs}/rerun-${crate}-${i}.log"
        rerun_status=0
        cargo nextest run -p "${crate}" --no-fail-fast --no-tests=pass --retries 0 "${features[@]+"${features[@]}"}" -E "${rerun_exprs[${idx}]}" > "${log}" 2>&1 || rerun_status=$?
        [ "${rerun_status}" -eq 0 ] && continue
        rerun_fail_at=${i}
        failing=$(grep -E '^\s*(FAIL|TIMEOUT|SIGSEGV|SIGABRT) \[' "${log}" | sed -E 's/^[[:space:]]*[A-Z]+ \[[^]]*\][[:space:]]*(\([0-9]+\/[0-9]+\)[[:space:]]*)?//' | sort -u | grep . || true)
        if [ -n "${failing}" ]; then
          rerun_fail_names="${rerun_fail_names:+${rerun_fail_names}
}${failing}"
          rerun_fail_detail="${rerun_fail_detail}$(grep -E "panicked at|assertion .* failed|left: |right: " "${log}" | head -n 10)
"
        else
          rerun_fail_names="${rerun_fail_names:+${rerun_fail_names}
}${crate} (nextest exited ${rerun_status} without a test result)"
          rerun_fail_detail="${rerun_fail_detail}$(first_errors "${log}")
"
        fi
      done
      [ "${rerun_fail_at}" -eq 0 ] || break
      rerun_done=${i}
      i=$((i + 1))
    done
    if [ "${rerun_fail_at}" -eq 0 ]; then
      cap_note=""
      [ "${rerun_capped}" -eq 1 ] && cap_note=" (stopped at the ${rerun_budget}s time cap)"
      record "flaky tests" pass "${rerun_total} changed tests passed ${rerun_done} reruns${cap_note}"
    else
      record "flaky tests" fail "$(printf '%s\n' "${rerun_fail_names}" | grep -c .) of ${rerun_total} changed tests failed rerun ${rerun_fail_at} of ${rerun_n}" "Flaky tests (passed the first run, failed a rerun):
${rerun_fail_names}
${rerun_fail_detail}" "${rerun_fail_names}"
    fi
  fi

  # --- cargo clippy, per crate -----------------------------------------------
  for crate in ${crates[@]+"${crates[@]}"}; do
    log="${logs}/clippy-${crate}.log"
    if cargo clippy -p "${crate}" --all-targets -- -D warnings > "${log}" 2>&1; then
      record "cargo clippy -p ${crate}" pass "no warnings"
    else
      record "cargo clippy -p ${crate}" fail "clippy reported errors" "$(first_errors "${log}")" "$(error_sig "${log}")"
    fi
  done
fi

# --- conductor workflow validate ------------------------------------------------
# Uses the conductor built above from this worktree, so a change to the
# validator itself is checked by the new code. Static validate is read-only and
# never migrates the database (#3470), so it runs in the caller's environment
# (--templates resolves scripts from the worktree, #3455). One failure mode remains:
# the real database's schema newer than this build (another branch's build or a newer
# installed conductor migrated it) makes the read-only open fail with a schema error
# unrelated to the change. That error has no FAIL/✗ line, so the detail below falls
# back to the log's tail; the fix is to rebuild from a branch with that migration.
if [ "${validate_pipeline}" -eq 1 ]; then
  if [ "${build_ok}" -eq 0 ]; then
    record "conductor workflow validate" skipped "not run: cargo build failed"
  elif cargo run -q --bin conductor -- workflow validate --all --templates --path . > "${logs}/validate.log" 2>&1; then
    record "conductor workflow validate" pass "$(tail -n 1 "${logs}/validate.log")"
  else
    record "conductor workflow validate" fail "workflow or template validation failed" "$(grep -E '^(FAIL|\s*✗)' "${logs}/validate.log" | head -n 40 | grep . || tail -n 5 "${logs}/validate.log")" "$(grep -E '^(FAIL|\s*✗)' "${logs}/validate.log" | sed -E 's/^[[:space:]]+//' | sort -u)"
  fi
fi

# --- workflow smokes -----------------------------------------------------------
# CLAUDE.md's required `validate --dry-run` of each changed workflow and its callers
# (#3453). Skipped after a build or validate failure: a smoke of a broken build or a
# statically invalid workflow says nothing new. A skip inside smoke-workflows.sh (a
# required input with no value) is recorded in `smokes` but isn't a failure.
if [ "${smoke_workflows}" -eq 1 ] || { [ "${smoke_unknown}" -eq 1 ] && [ "${validate_pipeline}" -eq 1 ]; }; then
  if [ "${smoke_workflows}" -eq 0 ]; then
    record "workflow smokes" skipped "not run: could not list the changed files"
  elif [ "${build_ok}" -eq 0 ]; then
    record "workflow smokes" skipped "not run: cargo build failed"
  elif [ "$(printf '%s' "${checks_json}" | jq -r '[.[] | select(.name == "conductor workflow validate") | .status][0]')" != "pass" ]; then
    record "workflow smokes" skipped "not run: conductor workflow validate failed"
  else
    smoke_status=0
    smoke_out=$(printf '%s\n' "${all_changed}" | bash "$(dirname "${BASH_SOURCE[0]}")/smoke-workflows.sh" 2> "${logs}/smokes.err") || smoke_status=$?
    if smoke_json=$(jq -c '.smokes | arrays' <<< "${smoke_out}" 2>/dev/null) && [ -n "${smoke_json}" ]; then
      smokes_json="${smoke_json}"
      counts=$(jq -r '[(map(select(.status == "pass")) | length), (map(select(.status == "skip")) | length), (map(select(.status == "fail")) | length)] | "\(.[0]) passed, \(.[1]) skipped, \(.[2]) failed"' <<< "${smokes_json}")
      if [ "${smoke_status}" -eq 0 ]; then
        record "workflow smokes" pass "${counts}"
      else
        record "workflow smokes" fail "${counts}" "$(jq -r '.[] | select(.status == "fail") | "\(.workflow) (\(.reason))\(if .run_id != "" then " run " + .run_id else "" end):\n\(.detail)"' <<< "${smokes_json}" | head -n 40)" "$(jq -r '.[] | select(.status == "fail") | .workflow' <<< "${smokes_json}")"
      fi
    else
      record "workflow smokes" fail "smoke-workflows.sh exited ${smoke_status} without a result" "$(tail -n 20 "${logs}/smokes.err")"
    fi
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
${detail}" "$(grep -E '^FAIL  ' "${log}" | sed -E 's/^FAIL  +//' | sort -u)"
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

# --- schema tripwire -----------------------------------------------------------
# Recorded only when it trips.
if [ -n "${schema_before}" ]; then
  if ! schema_after=$(real_schema_version); then
    schema_after="unreadable"
  fi
  if [ "${schema_after}" != "${schema_before}" ]; then
    record "conductor database schema" fail "the real conductor database went from schema ${schema_before} to ${schema_after} during verify" \
      "Something in this verify opened $(real_db_path) with this branch's build and migrated it. Every installed conductor older than this branch (the MCP server, TUI, web, and the conductor that running workflows' script steps call) now refuses to open it. Find the caller that ran this branch's build outside lib-branch-home.sh, and tell the user: they must install this branch's build or restore the database."
  fi
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
