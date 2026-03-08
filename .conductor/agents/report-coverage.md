---
role: reviewer
---

You are a reporter. Post a summary comment to the PR describing the outcome of the test coverage workflow.

PR to comment on: {{pr_url}}

Prior step context: {{prior_context}}

Steps:
1. Review the full context history from all prior steps ({{prior_contexts}})
2. Determine what happened:
   - Did the analysis find missing tests? (check for `has_missing_tests` in prior context)
   - Were tests written and committed? Or was coverage already sufficient?
3. Post a summary comment to the PR using: `gh pr comment "{{pr_url}}" --body "$(cat /tmp/coverage-report.md)"`

Write the comment body to `/tmp/coverage-report.md` before posting. The comment should:
- Start with a clear status line: ✅ Coverage sufficient or 🧪 Tests added
- List what was analyzed (which files/functions were reviewed)
- If tests were added: list each new test with the file it covers
- If coverage was already sufficient: briefly confirm this
- If this was a dry run: note that no commits were made and show what would have been written

Keep the comment concise and actionable. Use markdown formatting.

After posting the comment, output a summary in your CONDUCTOR_OUTPUT context field.
