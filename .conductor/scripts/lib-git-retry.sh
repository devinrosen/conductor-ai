#!/usr/bin/env bash
# Retry `git push` when it fails with a transport error that looks transient.
# Source this file: source "$(dirname "$0")/lib-git-retry.sh"
#
# git_push_retry <git push args...>
#   Runs `git push <args>` and writes its combined output to stdout. On failure,
#   retries after GIT_PUSH_RETRY_DELAYS seconds (default "2 5 10", so up to 4
#   attempts) when the output looks like a transient transport failure: an
#   intermittent SSH agent ("Permission denied (publickey)", seen on #3302 where
#   the third `ssh -T` succeeded), a dropped or refused connection, a timeout
#   or an HTTP 5xx. A push the remote received and refused (non-fast-forward,
#   stale lease, protected branch, hook rejection) or a missing repository
#   returns at once. The last attempt's output is written and its exit status
#   returned (#3306).
#
# Retrying a push is safe: a push that reached the remote before its response
# was lost makes the retry a no-op ("Everything up-to-date"). A caller that
# reads "Everything up-to-date" as "nothing was pushed" can set
# GIT_PUSH_ATTEMPTS_FILE to a file that receives the number of attempts made.

GIT_PUSH_RETRY_DELAYS="${GIT_PUSH_RETRY_DELAYS:-2 5 10}"

# git_is_transient_push_error OUTPUT_TEXT
git_is_transient_push_error() {
  # Refused by the remote, or not there at all: retrying can't help.
  if printf '%s' "$1" | grep -qiE \
    '\[rejected\]|\[remote rejected\]|non-fast-forward|stale info|fetch first|protected branch|GH006|GH013|repository not found|does not appear to be a git repository'; then
    return 1
  fi
  printf '%s' "$1" | grep -qiE \
    'Permission denied \(publickey\)|Could not read from remote repository|connection (reset|refused|closed)|timed? ?out|could not resolve host|kex_exchange_identification|ssh_exchange_identification|remote end hung up unexpectedly|early EOF|RPC failed|broken pipe|network is unreachable|temporary failure|HTTP 5[0-9][0-9]|bad gateway|service unavailable|internal server error'
}

git_push_retry() {
  local out status delay attempts=0
  for delay in $GIT_PUSH_RETRY_DELAYS ""; do
    status=0
    attempts=$((attempts + 1))
    if [ -n "${GIT_PUSH_ATTEMPTS_FILE:-}" ]; then
      echo "$attempts" > "$GIT_PUSH_ATTEMPTS_FILE"
    fi
    out=$(git push "$@" 2>&1) || status=$?
    if [ "$status" -eq 0 ] || [ -z "$delay" ] || ! git_is_transient_push_error "$out"; then
      printf '%s\n' "$out"
      return "$status"
    fi
    echo "git push failed with a transient-looking error (exit ${status}); retrying in ${delay}s: ${out}" >&2
    sleep "$delay"
  done
}
