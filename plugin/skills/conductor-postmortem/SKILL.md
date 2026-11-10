---
name: conductor-postmortem
description: End-of-session review of what went wrong with conductor workflow runs and the runkon engine, and what to improve, filed as GitHub issues. Not a general retro: the session plugin's /postmortem covers that. Gathers the session's conductor runs and stats, looks back over the conversation for hand fixes and wrong reviewer claims, drafts issues with evidence routed to the right repo, and files only the ones the user approves. Use when the user asks for a conductor postmortem, what went wrong with this session's conductor runs, or bugs and improvements for the workflow engine or pipeline.
---

# conductor-postmortem

Review the current session's conductor work and turn what went wrong into issues the user approves. Nothing is written to GitHub until the user says which drafts to file.

The most useful evidence is in this conversation, not only in conductor's database: hand fixes, base merges and re-reviews, reviewer findings that turned out to be wrong, and places where a brief, doc or tool description disagreed with the code. Read both.

## 1. Find the session's runs

1. Collect every workflow run ID that appears in this conversation (`run_id: 01…` from `conductor_run_workflow`, `conductor workflow wait`, `conductor_resume_run` and the like).
2. Call `conductor_list_runs` with `include_inactive = true` (no repo filter, `limit` 50; page with `offset` if needed). The session's PRs have usually merged by now, and without `include_inactive` the tool hides runs on merged worktrees. Add runs that started during this session but weren't mentioned, for example child runs. Run IDs are ULIDs, so they sort by start time. If you can't tell when the session started, use the earliest run ID from step 1, and say so.
3. For each top-level run, call `conductor_run_stats` (pass a ticket-to-pr run and its follow-up iterate-pr run together, comma-separated). For failed or resumed runs, also call `conductor_get_run` for the step errors. To check a review's findings, scope and posted URL, call `conductor_get_review` (a review-pr run id, or a parent run id) instead of reading SQLite.

Summarize in a table: run, workflow, repo and ticket or PR, outcome, wall time, review rounds, blocking findings, cost. Mark runs that needed a hand fix, a resume, or a stop such as `fail-verify`.

Read the numbers from `conductor_run_stats` with `json = true` (or `conductor workflow run-stats <id> --json`). Don't guess paths; these are the fields:

| Column | JSON |
|---|---|
| Outcome | `.status` |
| Wall time | `.wall_ms` |
| Cost | `.usage.cost_usd` |
| Agent runs | `.usage.agent_runs` |
| Review rounds | `.cycles \| length` |
| Approved | `.cycles[-1].approved` |
| Rounds to approval | `.cycles_to_approval` (null when never approved) |
| Blocking findings per round | `.cycles[].blocking_total`, by reviewer in `.cycles[].blocking_by_reviewer` |

Cross-check: a round with `approved: false` should have a non-zero `blocking_total`. If it reads as 0 or null, the path is wrong; look at the raw JSON before filling the table.

If `~/.cache/claude-notify/usage.json` exists, note the current 5-hour and 7-day usage (`rate_limits.five_hour.used_percentage`, `rate_limits.seven_day.used_percentage`). Say if its `seen_at` is stale; don't guess numbers.

## 2. Look back over the session

Go through the conversation and list each time something didn't work as designed:

- **Hand interventions:** code or pipeline fixes made by hand during or after a run, resumes, merging the base branch to fix a semantic conflict, re-running a step.
- **Reviewer findings:** blocking findings that were wrong (disputed, overturned, or based on a false premise), and real bugs a reviewer missed that surfaced later.
- **Wrong docs and briefs:** an agent brief, `CLAUDE.md`, tool description or comment that disagreed with the code, especially where you had to read source to settle it.
- **Engine behaviour:** markers, conditions, loops, resume, FLOW_OUTPUT parsing, gates or step output that behaved differently than the workflow expected.
- **Friction:** slow or costly steps, steps that failed on transient errors, missing tools or MCP coverage, manual steps that recur every session.

Use evidence you can point to: run IDs, PR and issue links, `file:line`, the exact error text. Drop anything you can't support.

## 3. Classify and route

Give each finding one kind:

| Kind | Examples | Repo |
|---|---|---|
| Engine bug | runkon-flow resume, conditions, loops, output parsing, executors | `runkon/runkon` |
| Pipeline bug | `.conductor/` workflows, scripts, agent briefs, schemas | the repo whose pipeline it is |
| Conductor bug | conductor-core, CLI, MCP, TUI, web | `devinrosen/conductor-ai` |
| Improvement | anything that would have saved time, cost or a manual step | where the change would land |

When the root cause is in the runkon engine itself (the `runkon-*` crates or their documented behaviour), it belongs in `runkon/runkon`; how a repo's workflows or conductor use runkon belongs in that repo. When unsure, choose the host repo and say in the body that it may belong in runkon/runkon.

## 4. Check for duplicates

For each finding, search open issues in its target repo, and in the other repo when the kind is borderline:

```bash
gh issue list --repo <owner/name> --state open --search "<key terms>" --limit 20 --json number,title,milestone
```

Mark a finding as a duplicate when an open issue already covers it, with that issue's number. Suggest a comment with the new evidence instead of a new issue.

## 5. Propose, then file what the user approves

Show the drafts as a table: number, kind, repo, title, severity (bug or improvement, and how often it bit), evidence, and duplicate-of if any. Below it, list the open milestones in each target repo (`gh api repos/<owner/name>/milestones -q '.[].title'`).

Ask which drafts to file and in which milestone (or none). Use `AskUserQuestion` when available, with one multi-select question for the drafts. Don't file anything before the answer.

Then for each approved draft write an issue with **Problem** (what happened, with evidence), **Proposal** and **Acceptance** sections, and create it:

```bash
gh issue create --repo <owner/name> --title "<title>" --body-file <file> [--milestone "<milestone>"] [--label <label>]
```

Only use labels that already exist in that repo (`gh label list --repo <owner/name>`). For an approved duplicate, post the comment on the existing issue instead (`gh issue comment`).

For a repo registered with conductor, prefer `conductor_create_gh_issue` (with `milestone`, `labels` and `run_id` if any): it resolves the repo slug, validates the milestone against the open ones and records the run link. Keep the `gh` form for unregistered repos such as runkon/runkon.

If the repo is registered with conductor and an issue will be worked on through conductor, run a full `conductor_sync_tickets` for that repo afterwards so the new ticket IDs resolve.

## 6. Report

End with the issues created or commented on (links), anything the user declined, and, if the user keeps a usage log, one line for it: date, runs, total conductor cost, and usage before and after.
