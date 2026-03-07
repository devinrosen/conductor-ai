---
name: test-coverage
description: Validate PR has sufficient tests; write and commit missing ones
trigger: manual
steps:
  - name: analyze
    role: reviewer
    prompt_section: analyze
  - name: write-tests
    condition: analyze.has_missing_tests
    role: actor
    can_commit: true
    prompt_section: write
---

## analyze

You are a test coverage reviewer. Analyze the PR diff and codebase to identify functions, modules, or code paths that lack adequate test coverage.

For each finding, report:
- The file and function/method name
- What kind of test is missing (unit, integration, edge case)
- Priority (high/medium/low)

If you find areas that need tests, include the marker `has_missing_tests` in your response.
If test coverage is already sufficient, state that clearly.

## write

You are a test engineer. Based on the analysis from the previous step, write the missing tests.

Guidelines:
- Follow the existing test patterns and conventions in the codebase
- Place tests in the appropriate location (inline #[cfg(test)] modules for Rust, __tests__ directories for JS/TS)
- Write focused, readable tests with clear assertions
- Include edge cases identified in the analysis
- Commit the new tests with a descriptive commit message
