#!/usr/bin/env bash
# smoke-workflows.sh — run the dry-run smokes a pipeline change needs (#3453).
#
# CLAUDE.md requires every PR that changes a .conductor/workflows/*.wf file or a
# conductor-core/templates/*.wft template to carry a passing
# `conductor workflow validate --dry-run <name>`, and a changed child workflow also
# needs a smoke of each parent that calls it. verify.sh pipes its changed-file list
# (newline-separated paths) into this script, so the smokes run inside every verify
# and a failing one goes back to implement like any failed check.
#
# Targets:
#   - each changed top-level .conductor/workflows/*.wf that still exists, under the
#     name on its `workflow <name>` line (the file stem when it has none);
#   - each changed conductor-core/templates/<name>.wft maps to the repo workflow of
#     the same name; with none it is skipped by name (validate --templates covers it);
#   - every workflow whose file has a `call workflow <name>` line for a changed one
#     (direct callers only, whole-word match).
# Each target runs `$SMOKE_CONDUCTOR workflow validate <name> --dry-run --path .`
# (default: cargo run -q --bin conductor --), from the current directory (the
# worktree), plus the --input args from the table below.
#
# Output: one JSON object on stdout, {"smokes": [{workflow, reason, status, run_id,
# detail}]}, status pass | fail | skip. A smoke whose required input has no value is
# skipped and named (a skip is not a failure). Exit 1 when any smoke failed, else 0.
# No target gives {"smokes": []} and no conductor call.
#
# The smokes need the real database (repo, worktree, linked ticket) and record their
# runs in it, so they run through lib-branch-home.sh (#3469): when this branch adds a
# migration the real database doesn't have, they run against a copy, and each result
# carries `db_note` saying so (its run ID exists only in the copy). When the database
# is newer than the branch, or its version can't be read, every smoke is skipped with
# the reason instead of letting the branch build open it.
#
# Env: SMOKE_CONDUCTOR, SMOKE_DETECT_ONLY=1 (print the targets and stop, for tests).
set -uo pipefail
# shellcheck source=lib-branch-home.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib-branch-home.sh"

# Extra `--input k=v` arguments per workflow. ticket_id needs no row: the CLI fills it
# from the worktree's linked ticket, and a worktree with none is skipped and named.
# Add a row when a workflow gains a required input that has no default.
extra_inputs() {
  case "$1" in
    analyze-repo-for-agents | analyze-ux | generate-diagrams) echo "repo=conductor-ai" ;;
    *) echo "" ;;
  esac
}

wf_dir=".conductor/workflows"

# workflow_name FILE — the name on the `workflow <name>` line, else the file stem.
workflow_name() {
  local n
  n=$(sed -n -E 's/^[[:space:]]*workflow[[:space:]]+([A-Za-z0-9_.-]+).*/\1/p' "$1" | head -n 1)
  [ -n "$n" ] || n=$(basename "$1" .wf)
  printf '%s' "$n"
}

changed_names=""   # newline-separated workflow names
skipped=""         # lines: "<name>\t<detail>"
nl='
'

add_changed() {
  case "${nl}${changed_names}${nl}" in
    *"${nl}$1${nl}"*) ;;
    *) changed_names="${changed_names:+${changed_names}${nl}}$1" ;;
  esac
}

while IFS= read -r path; do
  case "$path" in
    .conductor/workflows/*.wf)
      rest="${path#.conductor/workflows/}"
      case "$rest" in */*) continue ;; esac
      [ -f "$path" ] || continue
      add_changed "$(workflow_name "$path")"
      ;;
    conductor-core/templates/*.wft)
      rest="${path#conductor-core/templates/}"
      case "$rest" in */*) continue ;; esac
      stem="${rest%.wft}"
      if [ -f "${wf_dir}/${stem}.wf" ]; then
        add_changed "$(workflow_name "${wf_dir}/${stem}.wf")"
      else
        skipped="${skipped}template ${stem}	template ${stem} has no workflow in this repo (covered by validate --templates)${nl}"
      fi
      ;;
  esac
done

changed_sorted=$(printf '%s\n' "$changed_names" | sed '/^$/d' | sort -u)

# Parents: workflows with a `call workflow <child>` line for a changed child.
parents=""  # lines: "<name>\t<child>"
if [ -n "$changed_sorted" ]; then
  for f in "${wf_dir}"/*.wf; do
    [ -f "$f" ] || continue
    pname=$(workflow_name "$f")
    while IFS= read -r child; do
      [ -n "$child" ] || continue
      [ "$pname" = "$child" ] && continue
      if grep -qE "^[[:space:]]*call[[:space:]]+workflow[[:space:]]+${child//./\\.}([[:space:]{]|\$)" "$f"; then
        # A parent that also changed is listed once, as changed.
        case "${nl}${changed_sorted}${nl}" in *"${nl}${pname}${nl}"*) continue ;; esac
        case "${nl}${parents}" in *"${nl}${pname}	"*) continue ;; esac
        parents="${parents:+${parents}${nl}}${pname}	${child}"
      fi
    done <<< "$changed_sorted"
  done
  parents=$(printf '%s\n' "$parents" | sed '/^$/d' | sort -u)
fi

targets=""  # lines: "<name>\t<reason>"
while IFS= read -r n; do
  [ -n "$n" ] && targets="${targets}${n}	changed${nl}"
done <<< "$changed_sorted"
while IFS="	" read -r n child; do
  [ -n "$n" ] && targets="${targets}${n}	calls ${child}${nl}"
done <<< "$parents"

if [ "${SMOKE_DETECT_ONLY:-}" = "1" ]; then
  printf '%s' "$targets" | tr '\t' ' '
  printf '%s' "$skipped" | sed 's/^\([^	]*\)	/skip \1: /'
  exit 0
fi

results='[]'
db_note=""
add_result() { # WORKFLOW REASON STATUS RUN_ID DETAIL
  results=$(jq -c --arg w "$1" --arg r "$2" --arg s "$3" --arg id "$4" --arg d "$5" --arg db "$db_note" \
    '. + [{workflow: $w, reason: $r, status: $s, run_id: $id, detail: $d} + (if $db != "" then {db_note: $db} else {} end)]' <<< "$results")
}

any_failed=0
while IFS="	" read -r n detail; do
  [ -n "$n" ] && add_result "$n" "changed" skip "" "$detail"
done <<< "$skipped"

if [ -n "$targets" ]; then
  trap branch_home_cleanup EXIT
  if ! branch_home_setup; then
    while IFS="	" read -r name reason; do
      [ -n "$name" ] && add_result "$name" "$reason" skip "" "not run: ${BRANCH_HOME_NOTE}"
    done <<< "$targets"
    targets=""
  elif [ "$BRANCH_HOME_MODE" = copy ]; then
    db_note="$BRANCH_HOME_NOTE"
  fi
fi

smoke_cmd="${SMOKE_CONDUCTOR:-cargo run -q --bin conductor --}"
while IFS="	" read -r name reason; do
  [ -n "$name" ] || continue
  args=()
  extra=$(extra_inputs "$name")
  for kv in $extra; do args+=(--input "$kv"); done
  out=$(${smoke_cmd} workflow validate "$name" --dry-run --path . ${args[@]+"${args[@]}"} 2>&1 < /dev/null)
  rc=$?
  run_id=$(printf '%s\n' "$out" | grep -E '^SMOKE (PASS|FAIL)' | sed -n -E 's/.*\(run ([0-9A-Za-z]+)\).*/\1/p' | head -n 1)
  result_line=$(printf '%s\n' "$out" | grep -E '^SMOKE (PASS|FAIL|SKIP)  ' | head -n 1)
  warns=$(printf '%s\n' "$out" | grep -E '^SMOKE WARN' || true)
  case "$result_line" in
    "SMOKE PASS"*)
      add_result "$name" "$reason" pass "$run_id" "$(printf '%s\n%s' "${result_line#SMOKE PASS  }" "$warns" | sed '/^$/d')"
      ;;
    "SMOKE FAIL"*"Missing required input"*)
      missing=$(printf '%s' "$result_line" | sed -n -E "s/.*Missing required input: '?([A-Za-z0-9_.-]+).*/\1/p")
      add_result "$name" "$reason" skip "" "no value for required input ${missing:-?}"
      ;;
    "SMOKE SKIP"*)
      add_result "$name" "$reason" skip "" "${result_line#SMOKE SKIP  }"
      ;;
    *)
      any_failed=1
      fail_lines=$(printf '%s\n' "$out" | grep -E '^(SMOKE FAIL|FAIL|[[:space:]]*✗)' | head -n 10)
      [ -n "$fail_lines" ] || fail_lines=$(printf '%s\n' "$out" | tail -n 10)
      add_result "$name" "$reason" fail "$run_id" "${fail_lines:-exit ${rc} with no output}"
      ;;
  esac
done <<< "$targets"

jq -c -n --argjson smokes "$results" '{smokes: $smokes}'
[ "$any_failed" -eq 0 ]
