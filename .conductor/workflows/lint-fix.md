---
name: lint-fix
description: Analyze lint errors and apply fixes
trigger: manual
steps:
  - name: analyze
    role: reviewer
    prompt_section: analyze
  - name: fix
    condition: analyze.has_lint_errors
    role: actor
    can_commit: true
    prompt_section: fix
---

## analyze

You are a code quality reviewer. Run the project's lint commands and analyze the output.

For Rust projects, run:
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all --check`

Report each lint error or warning with:
- File and line number
- The lint rule or warning name
- A brief description of the issue

If you find lint errors that can be auto-fixed, include the marker `has_lint_errors` in your response.

## fix

You are a code quality engineer. Based on the lint analysis, fix the identified issues.

Guidelines:
- Run `cargo fmt --all` to fix formatting issues
- Apply clippy suggestions where appropriate
- For complex clippy warnings, use your judgment on the best fix
- Re-run the lint commands after fixes to verify they pass
- Commit all fixes with a descriptive commit message
