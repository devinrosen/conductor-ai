#!/usr/bin/env bash
# Retry a gh command when it fails with an error that looks transient.
# Source this file: source "$(dirname "$0")/lib-gh-retry.sh"
#
# gh_retry gh <args...>
#   Runs the command and passes its stdout through. On failure, retries after
#   GH_RETRY_DELAYS seconds (default "2 5 10", so up to 4 attempts) when stderr
#   looks transient: network errors, timeouts, HTTP 5xx, HTTP 429 or a rate
#   limit. Any other failure (a 422 validation error, a missing PR, a 404)
#   returns at once. The last attempt's stderr is written to stderr and its exit
#   status is returned, so callers still see the real gh error (#3239).
#
# Only wrap calls that are safe to repeat. A write whose request reached GitHub
# but whose response was lost can land twice when retried; for a conductor
# review that means one extra review, which the next review run dismisses.

GH_RETRY_DELAYS="${GH_RETRY_DELAYS:-2 5 10}"

# gh_is_transient_error STDERR_TEXT
gh_is_transient_error() {
  printf '%s' "$1" | grep -qiE \
    'HTTP 5[0-9][0-9]|HTTP 429|rate limit|timed? ?out|timeout|connection (reset|refused)|could not resolve host|no such host|TLS handshake|unexpected EOF|i/o timeout|error connecting to|network is unreachable|temporary failure|server error|bad gateway|service unavailable'
}

gh_retry() {
  local err_file status=0 delay err
  err_file=$(mktemp "${TMPDIR:-/tmp}/gh_retry_err.XXXXXXXXXX")
  for delay in $GH_RETRY_DELAYS ""; do
    status=0
    "$@" 2>"$err_file" || status=$?
    if [ "$status" -eq 0 ]; then
      cat "$err_file" >&2
      rm -f "$err_file"
      return 0
    fi
    err=$(cat "$err_file")
    if [ -z "$delay" ] || ! gh_is_transient_error "$err"; then
      break
    fi
    echo "gh failed with a transient-looking error (exit ${status}); retrying in ${delay}s: ${err}" >&2
    sleep "$delay"
  done
  cat "$err_file" >&2
  rm -f "$err_file"
  return "$status"
}
