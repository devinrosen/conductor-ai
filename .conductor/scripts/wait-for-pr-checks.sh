#!/usr/bin/env bash
# wait-for-pr-checks.sh — verify-and-merge, after a base merge (#3436) or a checks_pending result (#3480). Polls the PR's
# required checks until they are green on the local HEAD. Refuses, with a one-line reason
# on stderr and exit 1, on a failing check, unreadable gh output or the timeout.
# Env vars: PR_NUMBER (required), REQUIRED_CHECKS (comma-separated names that must be
# present and passing; the names check-pr-ready saw before the merge, may be empty),
# WAIT_TIMEOUT (seconds, default 2700), POLL_INTERVAL (seconds, default 30),
# EMPTY_GRACE (seconds to keep polling when no check is reported and none is named,
# default 300), DRY_RUN.
#
# A name that hasn't appeared yet counts as pending: right after a push only some checks
# are registered, so "nothing pending" alone would pass too early.
set -euo pipefail

# shellcheck source=lib-gh-retry.sh
source "$(dirname "$0")/lib-gh-retry.sh"
# shellcheck source=lib-pr-checks.sh
source "$(dirname "$0")/lib-pr-checks.sh"

refuse() { echo "verify-and-merge: $*" >&2; exit 1; }

clean() { local v="${!1:-}"; [[ "${v}" == *"{{"* ]] && v=""; printf '%s' "${v}"; }
# pr_number is injected only when the branch has a PR and required_checks is a step
# output; neither is a declared input, so either can be missing (#3454).
PR_NUMBER=$(clean PR_NUMBER)
REQUIRED_CHECKS=$(clean REQUIRED_CHECKS)
# dry_run is an engine built-in, always set.
DRY_RUN="${DRY_RUN:-}"
# A numeric knob: an unset, empty or unresolved-placeholder value takes the default; any
# other non-integer is refused, since a non-numeric value would make the `[ -ge ]` timeout
# test error inside a `&&` list (ignored by errexit) and the loop would never end.
number_or_default() { # NAME DEFAULT
  local v; v=$(clean "$1")
  [ -n "${v}" ] || v="$2"
  [[ "${v}" =~ ^[0-9]+$ ]] || refuse "$1 must be a number of seconds, got '${v}'"
  printf '%s' "${v}"
}
WAIT_TIMEOUT=$(number_or_default WAIT_TIMEOUT 2700)
POLL_INTERVAL=$(number_or_default POLL_INTERVAL 30)
EMPTY_GRACE=$(number_or_default EMPTY_GRACE 300)

emit() { # CONTEXT
  jq -n --arg c "$1" '{markers: ["checks_green"], context: $c}' \
    | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
}

if [ "${DRY_RUN}" = "true" ]; then
  echo "dry run: would wait up to ${WAIT_TIMEOUT}s for required checks (${REQUIRED_CHECKS:-none named}) on PR #${PR_NUMBER:-?}" >&2
  emit "dry run: not waiting for checks"
  exit 0
fi

[[ "${PR_NUMBER}" =~ ^[0-9]+$ ]] || refuse "wait-for-pr-checks needs PR_NUMBER"
LOCAL_HEAD=$(git rev-parse HEAD)
SHORT="${LOCAL_HEAD:0:7}"
START=$(date +%s)
LAST=""

while :; do
  ELAPSED=$(( $(date +%s) - START ))
  TIMED_OUT=false
  [ "${ELAPSED}" -ge "${WAIT_TIMEOUT}" ] && TIMED_OUT=true

  PR_JSON=$(gh_retry gh pr view "${PR_NUMBER}" --json headRefOid) \
    || refuse "could not read PR #${PR_NUMBER}"
  PR_HEAD=$(printf '%s' "${PR_JSON}" | jq -r '.headRefOid // ""')
  if [ "${PR_HEAD}" != "${LOCAL_HEAD}" ]; then
    # GitHub can lag a push; keep polling until the deadline.
    LAST="PR head ${PR_HEAD:0:7} is not the local head ${SHORT}"
    ${TIMED_OUT} && refuse "${LAST} after ${ELAPSED}s"
  else
    read_required_checks "${PR_NUMBER}" || refuse "${CHECKS_ERROR}"
    FAILING=$(printf '%s' "${CHECKS}" | jq -r '[.[] | select(.bucket == "fail" or .bucket == "cancel") | .name] | join(", ")')
    [ -z "${FAILING}" ] || refuse "required check failing on ${SHORT}: ${FAILING}"
    # Pending: reported not passing, or named before the merge and not passing yet.
    WAITING=$(printf '%s' "${CHECKS}" | jq -r '[.[] | select(.bucket != "pass" and .bucket != "skipping") | .name] | join(", ")')
    PASSED=$(printf '%s' "${CHECKS}" | jq -r '[.[] | select(.bucket == "pass" or .bucket == "skipping") | .name] | join("\n")')
    OLDIFS="${IFS}"; IFS=','
    for n in ${REQUIRED_CHECKS}; do
      n="${n# }"; n="${n% }"
      [ -n "${n}" ] || continue
      if ! printf '%s\n' "${PASSED}" | grep -qxF -- "${n}"; then
        case ", ${WAITING}, " in *", ${n}, "*) ;; *) WAITING="${WAITING:+${WAITING}, }${n}" ;; esac
      fi
    done
    IFS="${OLDIFS}"

    if [ -z "${WAITING}" ]; then
      if [ "$(printf '%s' "${CHECKS}" | jq 'length')" -gt 0 ]; then
        emit "required checks green on ${SHORT}"
        exit 0
      fi
      # Nothing reported and nothing named: the repo may have no required checks, or
      # they aren't registered yet. Wait out the grace period.
      if [ "${ELAPSED}" -ge "${EMPTY_GRACE}" ]; then
        echo "Warning: no required checks reported for PR #${PR_NUMBER}; continuing." >&2
        emit "no required checks reported for ${SHORT}"
        exit 0
      fi
      LAST="no required checks reported yet"
    else
      LAST="${WAITING}"
    fi
    ${TIMED_OUT} && refuse "required checks still running on ${SHORT} after ${ELAPSED}s: ${LAST}"
  fi
  sleep "${POLL_INTERVAL}"
done
