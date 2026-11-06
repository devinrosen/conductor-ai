#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/lib-git-retry.sh"

# Retries a transient SSH or transport failure (#3306). On a failed push,
# show git's output before exiting rather than losing it in the substitution.
# GIT_PUSH_ATTEMPTS_FILE records the attempt count, which the substitution's
# subshell can't hand back in a variable.
attempts_file=$(mktemp "${TMPDIR:-/tmp}/push_attempts.XXXXXXXXXX")
trap 'rm -f "$attempts_file"' EXIT
push_output=$(GIT_PUSH_ATTEMPTS_FILE="$attempts_file" git_push_retry -u --force-with-lease origin HEAD) || {
  status=$?
  echo "$push_output"
  exit "$status"
}
echo "$push_output"
attempts=$(cat "$attempts_file" 2>/dev/null || echo 1)

branch=$(git rev-parse --abbrev-ref HEAD)
echo "Pushed branch: $branch"

# After a retry, "Everything up-to-date" can mean an earlier attempt pushed the
# commits before its connection dropped, so only a first-attempt no-op counts.
if [ "${attempts:-1}" -le 1 ] && echo "$push_output" | grep -q "Everything up-to-date"; then
  output='{"markers": []}'
else
  output='{"markers": ["pushed_new_commits"]}'
fi
printf '<<<FLOW_OUTPUT>>>\n%s\n<<<END_FLOW_OUTPUT>>>\n' "$output"
