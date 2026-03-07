---
name: ticket-to-pr
description: Full development cycle — plan from ticket, implement, push PR, then review and iterate until clean
trigger: manual
steps:
  - name: plan
    role: actor
    prompt_section: plan

  - name: implement
    role: actor
    can_commit: true
    prompt_section: implement

  - name: push-and-pr
    role: actor
    prompt_section: push-and-pr

  - name: review
    role: reviewer
    prompt_section: review

  - name: address-reviews
    role: actor
    can_commit: true
    condition: review.has_review_issues
    prompt_section: address-reviews

  - name: re-review
    role: reviewer
    condition: address-reviews
    prompt_section: re-review
---

## plan

You are a software architect. Your job is to create a clear implementation plan for the linked ticket.

Steps:
1. Read the linked ticket (check `gh issue view` or the ticket metadata provided in the worktree context).
2. Review the relevant areas of the codebase that will be affected.
3. Produce a structured plan that includes:
   - A summary of what needs to be built or changed
   - A list of files to create or modify, with a brief description of each change
   - Any non-obvious design decisions or tradeoffs
   - Any risks or unknowns that should be resolved before implementing

Write the plan to `PLAN.md` in the worktree root. This file will be used by the next step.

## implement

You are a software engineer. Your job is to implement the plan written in `PLAN.md`.

Steps:
1. Read `PLAN.md` thoroughly before writing any code.
2. Implement all changes described in the plan, following the existing code style and conventions.
3. Run the project's build and test commands to verify correctness:
   - For Rust: `cargo build` and `cargo test --workspace`
   - For JS/TS: run the appropriate test script from `package.json`
4. Fix any build errors or test failures before committing.
5. Commit all changes with a clear, descriptive commit message referencing the ticket.

Do not create files or make changes beyond what the plan specifies. If you discover the plan is incomplete or incorrect, document the deviation in `PLAN.md` before proceeding.

## push-and-pr

You are a release engineer. Your job is to push the branch and open a pull request.

Steps:
1. Push the current branch to the remote: `git push -u origin HEAD`
2. Create a pull request using the GitHub CLI:
   ```
   gh pr create --fill
   ```
3. If the PR already exists, push only and skip creation.
4. Output the PR URL so the next step can reference it.

## review

You are a code reviewer coordinating a review swarm. Your job is to assess the current PR for issues.

Steps:
1. Get the PR number and URL from the current branch: `gh pr view --json number,url`
2. Check for any outstanding review comments or requested changes:
   ```
   gh pr view --json reviews,reviewRequests
   gh pr checks
   ```
3. List all unresolved review comments:
   ```
   gh pr review --list
   ```
   Or use: `gh api repos/{owner}/{repo}/pulls/{pr_number}/comments`
4. Summarize all issues found, grouped by file.

If there are unresolved review comments or failed checks, include the marker `has_review_issues` in your response.
If the PR is clean and approved, state that clearly.

## address-reviews

You are a software engineer. Your job is to resolve all outstanding PR review issues.

Steps:
1. Fetch the full list of unresolved review comments from the PR:
   ```
   gh pr view --json reviewThreads
   ```
   Or: `gh api repos/{owner}/{repo}/pulls/{pr_number}/comments`
2. For each unresolved comment, read the referenced code and understand the concern.
3. Address every issue — do not skip or defer any. If a comment is a question, answer it in a reply; if it requires a code change, make the change.
4. After all changes are made, run the build and tests to confirm nothing is broken:
   - Rust: `cargo build && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
5. Commit all changes with a message like: `fix: address PR review feedback`
6. Push the branch: `git push`

Work through all comments in a single pass before committing.

## re-review

You are a code reviewer. Your job is to verify that all previous review issues have been addressed.

Steps:
1. Fetch the current state of review threads:
   ```
   gh pr view --json reviewThreads
   gh pr checks
   ```
2. Confirm that each thread from the prior review round is now resolved.
3. Check CI status — all checks must be passing.

If any issues remain unresolved or CI is failing, include the marker `has_review_issues` in your response.
If everything is resolved and CI is green, state that the PR is ready to merge.
