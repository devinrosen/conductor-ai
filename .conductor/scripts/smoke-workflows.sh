#!/usr/bin/env bash
# smoke-workflows.sh — run the dry-run smokes a pipeline change needs (#3453).
#
# CLAUDE.md requires every PR that changes a .conductor/workflows/*.wf file or a
# conductor-core/templates/*.wft template to carry a passing
# `conductor workflow validate --dry-run <name>`, and a changed child workflow also
# needs a smoke of the workflows that call it. verify.sh pipes its changed-file list
# (newline-separated paths) into this script, so the smokes run inside every verify
# and a failing one goes back to implement like any failed check.
#
# Targets:
#   - each changed top-level .conductor/workflows/*.wf that still exists, under the
#     name on its `workflow <name>` line (the file stem when it has none);
#   - each changed conductor-core/templates/<name>.wft maps to the repo workflow of
#     the same name; with none it is skipped by name (validate --templates covers it);
#   - every workflow whose file has a `call workflow <name>` line for a changed one
#     (whole-word match), directly or through other workflows: callers of callers
#     up to the top, each listed once. `reason` gives the shortest chain, for example
#     "calls iterate-pr, which calls review-pr".
# Each target runs `$SMOKE_CONDUCTOR workflow validate <name> --dry-run --json --path .`
# (default: cargo run -q --bin conductor --), from the current directory (the
# worktree), plus the --input args from the table below. The result comes from the
# JSON object the CLI prints ({workflow, status, run_id, stopped_at, warnings, error,
# missing_inputs}) and nothing else: no `SMOKE` text is read. Output with no such
# object (a build failure, a crash) is a failure.
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

# Callers, transitively (#3473): breadth-first from the changed workflows, each caller
# once, first discovery (the shortest chain) wins. A workflow is a caller of C when its
# file has a `call workflow C` line. The visited set also ends call loops.
ancestors=""  # lines: "<name>\t<reason>", in discovery order
if [ -n "$changed_sorted" ]; then
  callers=""  # lines: "<caller>\t<callee>" for every call line in the repo
  for f in "${wf_dir}"/*.wf; do
    [ -f "$f" ] || continue
    pname=$(workflow_name "$f")
    for callee in $(sed -n -E 's/^[[:space:]]*call[[:space:]]+workflow[[:space:]]+([A-Za-z0-9_.-]+)([[:space:]{].*)?$/\1/p' "$f" | sort -u); do
      [ "$pname" = "$callee" ] && continue
      callers="${callers}${pname}	${callee}${nl}"
    done
  done
  visited="$changed_sorted"
  frontier="$changed_sorted"
  while [ -n "$frontier" ]; do
    next=""
    while IFS= read -r c; do
      [ -n "$c" ] || continue
      if [ "$(printf '%s\n' "$changed_sorted" | grep -cxF -- "$c")" -gt 0 ]; then
        creason="calls ${c}"
      else
        prev=$(printf '%s' "$ancestors" | awk -F'\t' -v n="$c" '$1 == n { print $2; exit }')
        creason="calls ${c}, which ${prev}"
      fi
      for p in $(printf '%s' "$callers" | awk -F'\t' -v c="$c" '$2 == c { print $1 }' | sort -u); do
        case "${nl}${visited}${nl}" in *"${nl}${p}${nl}"*) continue ;; esac
        visited="${visited}${nl}${p}"
        ancestors="${ancestors}${p}	${creason}${nl}"
        next="${next:+${next}${nl}}${p}"
      done
    done <<< "$frontier"
    frontier=$(printf '%s\n' "$next" | sed '/^$/d' | sort -u)
  done
fi

targets=""  # lines: "<name>\t<reason>"
while IFS= read -r n; do
  [ -n "$n" ] && targets="${targets}${n}	changed${nl}"
done <<< "$changed_sorted"
targets="${targets}${ancestors}"

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
  errf=$(mktemp)
  out=$(${smoke_cmd} workflow validate "$name" --dry-run --json --path . ${args[@]+"${args[@]}"} 2>"$errf" < /dev/null)
  rc=$?
  err=$(cat "$errf"); rm -f "$errf"
  obj=$(printf '%s\n' "$out" | grep -E '^\{' | tail -n 1)
  if [ -z "$obj" ] || ! jq -e 'type == "object" and has("status")' <<< "$obj" > /dev/null 2>&1; then
    any_failed=1
    tail_lines=$(printf '%s\n%s\n' "$err" "$out" | sed '/^$/d' | tail -n 10)
    add_result "$name" "$reason" fail "" "${tail_lines:-exit ${rc} with no output}"
    continue
  fi
  status=$(jq -r '.status' <<< "$obj")
  run_id=$(jq -r '.run_id // ""' <<< "$obj")
  missing=$(jq -r '(.missing_inputs // []) | join(", ")' <<< "$obj")
  warns=$(jq -r '(.warnings // [])[]' <<< "$obj")
  if [ -n "$missing" ]; then
    add_result "$name" "$reason" skip "" "no value for required input ${missing}"
  elif [ "$status" = pass ]; then
    stop=$(jq -r 'if .stopped_at == null then "ran to completion, no agent step"
      else "stopped at \(.stopped_at.kind) step \u0027\(.stopped_at.step)\u0027" + (if .stopped_at.workflow != .workflow then " in \u0027\(.stopped_at.workflow)\u0027" else "" end) end' <<< "$obj")
    add_result "$name" "$reason" pass "$run_id" "$(printf '%s\n%s' "$stop" "$warns" | sed '/^$/d')"
  elif [ "$status" = skip ]; then
    add_result "$name" "$reason" skip "" "$(jq -r '.error // ""' <<< "$obj")"
  else
    any_failed=1
    at=$(jq -r 'if .stopped_at.kind == "failed" then "at step \u0027\(.stopped_at.step)\u0027: " else "" end' <<< "$obj")
    errtext=$(jq -r '.error // ""' <<< "$obj")
    add_result "$name" "$reason" fail "$run_id" "$(printf '%s%s\n%s' "$at" "$errtext" "$warns" | sed '/^$/d')"
  fi
done <<< "$targets"

jq -c -n --argjson smokes "$results" '{smokes: $smokes}'
[ "$any_failed" -eq 0 ]
