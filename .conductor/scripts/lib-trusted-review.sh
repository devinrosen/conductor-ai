#!/usr/bin/env bash
# The one rule for whether a review or comment author is trusted (#3421).
# Source this file: source "$(dirname "$0")/lib-trusted-review.sh"
#
# trusted_author_jq
#   Prints a jq definition, `def trusted_author($bots): ...;`. Prepend it to a jq
#   program and call `trusted_author($bots)` on a review or comment object, with
#   --arg bots "${TRUSTED_REVIEW_BOTS:-}":
#
#     PROG="$(trusted_author_jq)"' select(trusted_author($bots))'
#     jq --arg bots "${TRUSTED_REVIEW_BOTS:-}" "${PROG}"
#
#   An author is trusted when author_association is OWNER, MEMBER or COLLABORATOR,
#   or when user.type is "Bot" and user.login is exactly one of the comma-separated
#   logins in TRUSTED_REVIEW_BOTS (whitespace around each is ignored).
#
#   Bots are matched by exact login and type, never by association: GitHub reports
#   an app such as conductor-ai-reviewer[bot] as author_association NONE, but any
#   installed app or github-actions[bot] can post a review, so only the named ones
#   count. A User account that merely has the bot's login is not trusted.

trusted_author_jq() {
  cat <<'JQ'
def trusted_author($bots):
  ($bots | split(",") | map(gsub("^\\s+|\\s+$"; "")) | map(select(. != ""))) as $tb
  | (((.author_association // "") as $a | ["OWNER","MEMBER","COLLABORATOR"] | index($a)) != null)
    or ((.user.type // "") == "Bot" and ((.user.login // "") as $l | $tb | index($l)) != null);
JQ
}
