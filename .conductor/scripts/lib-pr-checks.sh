#!/usr/bin/env bash
# Read a PR's required checks. Source this file after lib-gh-retry.sh:
#   source "$(dirname "$0")/lib-pr-checks.sh"
#
# read_required_checks PR_NUMBER
#   Sets CHECKS to a JSON array of {name, bucket} and returns 0. gh pr checks exits 1
#   for a failing check and 8 for a pending one, still printing the JSON: that JSON is
#   used whatever the exit code. Only gh's own "no required checks" message means an
#   empty list. Any other failure with no JSON returns 1 with the reason in CHECKS_ERROR.
# Shared by check-pr-ready.sh and wait-for-pr-checks.sh so the two can't drift (#3436).

read_required_checks() {
  local pr="$1" err_file rc=0
  CHECKS="[]"
  CHECKS_ERROR=""
  err_file=$(mktemp "${TMPDIR:-/tmp}/pr_checks_err.XXXXXX")
  CHECKS=$(gh_retry gh pr checks "${pr}" --required --json name,bucket 2>"${err_file}") || rc=$?
  if ! printf '%s' "${CHECKS}" | jq -e 'type == "array"' >/dev/null 2>&1; then
    CHECKS="[]"
    if [ "${rc}" -ne 0 ] && ! grep -qi 'no required checks' "${err_file}"; then
      CHECKS_ERROR="could not read required checks for PR #${pr} (gh exit ${rc}): $(head -c 300 "${err_file}" | tr '\n' ' ')"
      rm -f "${err_file}"
      return 1
    fi
  fi
  rm -f "${err_file}"
  return 0
}
