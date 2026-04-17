## Diff scope rules

Get the diff for this PR using the appropriate command for the review scope:

- If the scope is **full** (default): detect the PR base branch and run the diff against it:
  ```bash
  BASE_BRANCH=$(gh pr view --json baseRefName -q .baseRefName 2>/dev/null || echo main)
  git diff origin/${BASE_BRANCH}...HEAD
  ```
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

## Output format

Severity guide:
- **critical**: Bugs, security holes, data loss — blocks merge
- **warning**: Design or correctness concern — should be addressed

Only flag `critical` or `warning` issues. Do not emit suggestion-level or style findings.

Your `CONDUCTOR_OUTPUT` `context` field must be a **JSON object** (not plain text) so the aggregator can parse it. Use this structure:

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

If you find **critical** or **warning** `findings`, include `has_review_issues` in your CONDUCTOR_OUTPUT markers.
If you find no findings, do NOT include that marker.
