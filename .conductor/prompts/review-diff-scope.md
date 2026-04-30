## Diff scope rules

Get the diff for this PR using the appropriate command for the review scope:

- If the scope is **full** (default): detect the PR base branch from the
  current branch's open PR and diff against it. **Run the command from the
  current working directory — do NOT `cd` anywhere.** The worktree is
  already cwd; `cd`ing into a different checkout (e.g. the canonical repo
  path) silently breaks `gh pr view`, which then falls back to `main` and
  produces a wrong diff.

  ```bash
  # Resolve the PR's base branch. Use `gh pr list --head` rather than
  # `gh pr view` so the lookup keys on the explicit branch name and is not
  # affected by cwd or the currently checked-out HEAD of an unrelated repo.
  BRANCH=$(git rev-parse --abbrev-ref HEAD)
  BASE_BRANCH=$(gh pr list --head "$BRANCH" --state open \
                  --json baseRefName -q '.[0].baseRefName' 2>/dev/null)
  if [ -z "$BASE_BRANCH" ]; then
    echo "ERROR: could not resolve PR base branch for $BRANCH — aborting" >&2
    exit 1
  fi
  git diff "origin/${BASE_BRANCH}...HEAD"
  ```

  Do NOT silently fall back to `main` if base detection fails. A wrong base
  produces a fabricated diff that contaminates every downstream finding.

- If the scope is **incremental**: run `git diff HEAD~1` to see only the latest commit.

**Review scope: {{scope}}**

If the diff exceeds ~50KB, focus on files most relevant to your review area.

**In scope — review carefully:**
- Lines starting with `+` (added code)
- Lines starting with `-` only when the replacement logic is relevant

**Out of scope — do not flag:**
- Context lines (no `+`/`-` prefix) — these are unchanged
- Pure deletions with no replacement unless they introduce a regression
- Formatting-only changes (whitespace, import ordering)

## Path verification (required for every finding)

Before emitting any entry in `findings`, verify that the cited `file` path
appears in the diff. The diff lists files via `diff --git a/<path> b/<path>`
and `+++ b/<path>` headers. If you cannot find the path in those headers,
**drop the finding** — it does not belong in `findings`.

Recognising a common pattern (CORS configuration, error handling shape,
logging idioms, etc.) is **not** evidence the code exists in this PR. The
diff is the only authoritative source. A submit-review safety net filters
hallucinated findings deterministically, but each false finding still
wastes reviewer attention — do not emit one in the first place.

This rule applies to `off_diff_findings` too: those must point to real
files visible in the diff context (the surrounding `--- a/<path>` blocks
of unchanged code, or files mentioned in the diff stat). A finding about
code that does not appear anywhere in the diff is almost certainly
hallucinated — drop it.

## Output format

Severity guide:
- **critical**: Bugs, security holes, data loss — blocks merge
- **warning**: Design or correctness concern — should be addressed

Only flag `critical` or `warning` issues. Do not emit suggestion-level or style findings.

Your `FLOW_OUTPUT` `context` field must be a **JSON object** (not plain text) so the aggregator can parse it. Use this structure:

```json
{
  "approved": true,
  "findings": [
    {
      "file": "src/foo.rs",
      "line": 42,
      "severity": "warning",
      "message": "One-line description",
      "suggestion": "How to fix it"
    }
  ],
  "off_diff_findings": [
    {
      "file": "src/bar.rs",
      "line": 10,
      "title": "Short issue title",
      "severity": "warning",
      "body": "Detailed description of the pre-existing issue"
    }
  ],
  "summary": "One-sentence summary of your review"
}
```

- `findings`: issues in code **added or modified by this PR** — set `approved: false` if any are `critical` or `warning`
- `off_diff_findings`: issues in **unchanged/removed code** — never affect `approved`, filed as separate GitHub issues; only include `critical` or `warning` severity
- Omit `off_diff_findings` entirely if there are none

If you find **critical** or **warning** `findings`, include `has_review_issues` in your FLOW_OUTPUT markers.
If you find no findings, do NOT include that marker.
