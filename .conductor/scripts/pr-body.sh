#!/usr/bin/env bash
# pr-body.sh — print the PR description built from PRIOR_CONTEXTS (#3148, #3333).
#
# PRIOR_CONTEXTS is the engine's {{prior_contexts}} JSON array of {step, context,
# structured_output, ...}, one entry per completed step in run order (the engine
# appends, so a repeated name keeps every occurrence). The body is:
#   ## Summary         the plan's summary (implement's summary when there is no plan), minus
#                      sentences naming a local path: a /Users/ or /home/ directory, a
#                      .git/worktrees/ path or a PLAN.md path (#3422)
#   ## Implementation  the first implement / implement-ticket context (later ones are
#                      verify-loopback fixes), minus sentences claiming the code wasn't
#                      built, tested, compiled or run: implement writes its summary before
#                      verify runs and is told not to run cargo (#3333)
#   ## Decisions       when the plan has decisions and the plan gate ran: each question, its
#                      default and the acceptance item it defers (the plan's `defers`), then the
#                      gate's answer from PLAN_GATE_FEEDBACK (the defaults kept when empty, else
#                      the feedback quoted). verify-acceptance reads it to mark a deferred
#                      criterion `deferred` instead of `fail` (#3423). PLAN_GATE_FEEDBACK unset or
#                      the literal {{human_review.feedback}} (the gate never ran) omits it.
#   ## Verification    what the last verify entry ran, when it passed
#   ## Smokes          the last verify entry's `smokes` (#3453): each workflow whose dry-run smoke
#                      verify ran (changed, or calling a changed child), its result and run ID.
#                      A failing smoke is listed as FAIL, never dropped. Omitted when verify ran
#                      no smokes (no .wf/.wft change). When the smokes ran against a copy of the
#                      database (the branch adds a migration, #3469), a closing line says so
#   ## Risks and open questions
# PR_BODY_SECTION=smokes prints only the ## Smokes section (empty when there is none), for
# update-pr-smokes.sh. Output is empty (exit 0) when there is no plan or implement summary, so the caller falls
# back to the commit bodies. Exit 1 with no stderr when PRIOR_CONTEXTS is unset, empty or
# itself a bare {{...}} template; any other value goes to jq, which exits non-zero with its
# error on stderr when it can't be parsed.
set -euo pipefail

contexts="${PRIOR_CONTEXTS:-}"
# Exit 1 (no stderr) only when the value is empty or is itself an unsubstituted template
# such as {{prior_contexts}} (#3350). Step outputs inside a real array may mention {{var}}
# freely, so don't scan the JSON for braces.
trimmed="${contexts#"${contexts%%[![:space:]]*}"}"
trimmed="${trimmed%"${trimmed##*[![:space:]]}"}"
tmpl_re='^\{\{[[:alnum:]_.]+\}\}$'
if [ -z "$trimmed" ] || [[ "$trimmed" =~ $tmpl_re ]]; then
  exit 1
fi

gate_ran=true
gate_feedback="${PLAN_GATE_FEEDBACK-}"
if [ -z "${PLAN_GATE_FEEDBACK+x}" ] || [ "$gate_feedback" = '{{human_review.feedback}}' ]; then
  gate_ran=false
  gate_feedback=""
fi

section="${PR_BODY_SECTION:-}"

printf '%s' "$contexts" | jq -r --arg section "$section" --argjson gate "$gate_ran" --arg fb "$gate_feedback" '
    def trim: gsub("^\\s+|\\s+$"; "");
    # A sentence that says the code was not built/tested/run (#3333). Keep narrow.
    def untested:
      test("(\\bnot|\\bnever|n'"'"'t)\\s+(been\\s+|yet\\s+)?(built|tested|compiled)\\b"; "i")
      or test("\\b(nothing|none of (it|this|these))\\b.*\\b(built|tested|compiled)\\b"; "i")
      or test("(\\bnot|\\bnever|n'"'"'t)\\s+(been\\s+|yet\\s+)?(run|verified)\\b.*\\b(cargo|build|tests?|clippy|fmt)\\b"; "i")
      or test("\\b(cargo|build|tests?|clippy|fmt)\\s+(((was|were|has|have|had|is|are)n'"'"'t|((was|were|has|have|had|is|are)\\s+)?(not|never))\\s+(been\\s+|yet\\s+)?(run|verified))\\b"; "i")
      or test("\\bdid(n'"'"'t| not)\\s+run\\b.*\\b(cargo|build|tests?|clippy|fmt)\\b"; "i")
      or test("^\\s*(not run|untested|not tested|not built)\\s*:"; "i")
      or test("\\bleft (it )?to (the )?verify\\b"; "i")
      or test("\\buncompiled\\b"; "i");
    def scrub:
      split("\n")
      | map( if trim == "" then ""
             else (split("(?<=[.!?])\\s+"; null) | map(select(untested | not)) | join(" ")) end )
      | join("\n")
      | gsub("\n{3,}"; "\n\n") | trim;
    # A sentence naming a machine-local path (#3422): a home directory, a .git/worktrees dir
    # or a PLAN.md path. Keep narrow so ~/.conductor/... and repo-relative paths survive.
    def localpath:
      test("(^|[\\s(`\u0027\"])/(Users|home)/[^\\s/]+/")
      or test("\\.git/worktrees/")
      or test("/[^\\s]*PLAN\\.md\\b");
    def scrubpaths:
      split("\n")
      | map( if trim == "" then ""
             else (split("(?<=[.!?])\\s+"; null) | map(select(localpath | not)) | join(" ")) end )
      | join("\n")
      | gsub("\n{3,}"; "\n\n") | trim;
    def plan: [.[] | select(.step == "plan" and .structured_output != null)]
              | last | (.structured_output // "null") | (try fromjson catch null);
    def impl: [.[] | select(.step == "implement" or .step == "implement-ticket")]
              | first | (.context // "") | scrub | scrubpaths;
    def verify: [.[] | select(.step == "verify")] | last;
    def verification:
      verify as $v
      | if $v == null then ""
        else ($v.structured_output // null | if type == "string" then (try fromjson catch null) else . end) as $o
          | ($v.context // "" | trim) as $c
          | if $o != null and ($o.passed == true) then
              ([($o.checks // [])[] | select(.status == "pass")]) as $ok
              | if ($ok | length) > 0 then
                  "The verify step passed before this PR was opened:\n\n"
                  + ($ok | map("- " + .name + (if ((.summary // "") | length) > 0 then ": " + .summary else "" end)) | join("\n"))
                elif $c != "" then $c else "" end
            elif $o == null and ($c | test("^PASS")) then $c
            else "" end
        end;
    def smokes_section:
      verify as $v
      | if $v == null then ""
        else ($v.structured_output // null | if type == "string" then (try fromjson catch null) else . end) as $o
          | ([($o // {} | .smokes // [])[] | select(type == "object" and ((.workflow // "") | length) > 0)]) as $sm
          | if ($sm | length) == 0 then ""
            else
              "`conductor workflow validate --dry-run` from this branch\u0027s worktree:\n\n"
              + ( $sm | map(
                    "- `\(.workflow)`" + (if ((.reason // "") | length) > 0 then " (\(.reason))" else "" end) + ": "
                    + ( if .status == "pass" then "pass" + (if ((.run_id // "") | length) > 0 then ", run `\(.run_id)`" else "" end)
                        elif .status == "skip" then "skipped, " + (.detail // "" | split("\n") | first // "")
                        else "FAIL" + (if ((.run_id // "") | length) > 0 then ", run `\(.run_id)`" else "" end) end )
                  ) | join("\n") )
              + ( [$sm[] | .db_note // "" | select(length > 0)][0] // "" | if . == "" then "" else "\n\nRun against " + . + "." end )
            end
        end;
    def decisions($p):
      ($p.decisions // [] | map(select(type == "object" and ((.question // "") | length) > 0))) as $d
      | if $gate != true or ($d | length) == 0 then ""
        else
          ($fb | trim) as $f
          | ($d | map(select((.defers // "") | length > 0))) as $df
          | "The plan gate settled these before implementation:\n\n"
            + ( $d | to_entries | map(
                  "\(.key + 1). \(.value.question | scrubpaths)"
                  + "\n   - Default: \(.value.default // "none" | scrubpaths)"
                  + ( if ((.value.defers // "") | length) > 0 then "\n   - Defers: \(.value.defers | scrubpaths)" else "" end )
                ) | join("\n\n") )
            + ( if $f == "" then
                  "\n\nAnswer at the gate: kept the defaults."
                  + ( if ($df | length) > 0 then
                        "\n\nDeferred acceptance items: " + ($df | map(.defers | scrubpaths) | join("; "))
                      else "" end )
                else
                  "\n\nAnswer at the gate:\n\n" + ($f | scrubpaths | split("\n") | map("> " + .) | join("\n"))
                  + ( if ($df | length) > 0 then "\n\nDefers (if the default was kept): " + ($df | map(.defers | scrubpaths) | join("; ")) else "" end )
                end )
        end;
    if $section == "smokes" then
      smokes_section | if . == "" then empty else "## Smokes\n\n" + . end
    else
    plan as $p | decisions($p) as $dc | impl as $i | verification as $vf | smokes_section as $sm
    | (($p.summary // "") | scrubpaths) as $s
    | ($p.risks // [] | map(select(type == "string" and length > 0))) as $r
    | if $s == "" and $i == "" then empty else
        ( if $s != "" then "## Summary\n\n\($s)" else "## Summary\n\n\($i)" end )
        + ( if $s != "" and $i != "" then "\n\n## Implementation\n\n\($i)" else "" end )
        + ( if $dc != "" then "\n\n## Decisions\n\n\($dc)" else "" end )
        + ( if $vf != "" then "\n\n## Verification\n\n\($vf)" else "" end )
        + ( if $sm != "" then "\n\n## Smokes\n\n\($sm)" else "" end )
        + ( if ($r | length) > 0 then "\n\n## Risks and open questions\n\n" + ($r | map("- " + .) | join("\n")) else "" end )
      end
    end
  '
