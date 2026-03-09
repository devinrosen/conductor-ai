## Diff scope rules

Get the diff for this PR by running:
```
git diff origin/main...HEAD
```

If the diff exceeds ~50KB, focus on files most relevant to your review area.

**In scope — review carefully:**
- Lines starting with `+` (added code)
- Lines starting with `-` only when the replacement logic is relevant

**Out of scope — do not flag:**
- Context lines (no `+`/`-` prefix) — these are unchanged
- Pure deletions with no replacement unless they introduce a regression
- Formatting-only changes (whitespace, import ordering)

## Output format

For each issue found, report:
- **Issue**: one-line description
- **Severity**: critical | warning | suggestion
- **Location**: file:line reference
- **Details**: explanation and recommended fix

Severity guide:
- **critical**: Bugs, security holes, data loss — blocks merge
- **warning**: Design or correctness concern — should be addressed
- **suggestion**: Style, minor improvement — non-blocking

If you find **critical** or **warning** issues, include `has_review_issues` in your CONDUCTOR_OUTPUT markers.
If you find only **suggestion** issues or no issues, do NOT include that marker.

Include a brief summary of your findings (or "No issues found") in your CONDUCTOR_OUTPUT context.
