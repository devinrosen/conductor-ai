use std::sync::Arc;

use conductor_core::Conductor;
use rmcp::model::{CallToolResult, Tool, ToolAnnotations};
use serde_json::{json, Value};

use crate::mcp::helpers::{schema, schema_typed, tool_err};

mod agents;
mod gates;
mod issues;
mod prs;
mod repos;
mod runs;
mod tickets;
mod workflows;
mod worktrees;

#[cfg(test)]
pub(crate) mod test_helpers;

/// Tools that only read the database. The server bounds these with a timeout;
/// every tool in `conductor_tools()` must appear in exactly one of this list and
/// `OTHER_TOOLS` (enforced by a test), so a new tool cannot go unclassified.
pub(super) const READ_ONLY_TOOLS: &[&str] = &[
    "conductor_list_runs",
    "conductor_list_agent_runs",
    "conductor_get_step_log",
    "conductor_run_stats",
    "conductor_get_review",
    "conductor_list_worktrees",
    "conductor_list_tickets",
    "conductor_get_ready_tickets",
    "conductor_list_repos",
    "conductor_list_workflows",
    "conductor_list_templates",
];

/// Tools that mutate state, run workflows, or shell out to the network. Timing
/// these out would report failure for work that is still in progress.
#[cfg(test)]
const OTHER_TOOLS: &[&str] = &[
    "conductor_create_worktree",
    "conductor_delete_worktree",
    "conductor_sync_tickets",
    "conductor_run_workflow",
    "conductor_approve_gate",
    "conductor_reject_gate",
    "conductor_adopt_worktree",
    "conductor_push_worktree",
    // Shells out to tmux, and sleeps briefly to confirm claude started.
    "conductor_open_session",
    // Same tmux launch as `conductor_open_session`.
    "conductor_take_over_session",
    "conductor_set_base_branch",
    "conductor_cancel_run",
    // Blocks by design until the run finishes or times out, so it must not get the read timeout.
    "conductor_wait_run",
    "conductor_resume_run",
    "conductor_submit_agent_feedback",
    "conductor_list_prs",
    // Reads the DB, but also fetches PR detail with a synchronous `gh` call.
    "conductor_get_worktree",
    // Reads the DB, but its stale-process warning spawns `<exe> --version` probes (up to 2s each).
    "conductor_get_run",
    "conductor_validate_workflow",
    "conductor_register_repo",
    "conductor_unregister_repo",
    "conductor_delete_ticket",
    "conductor_upsert_ticket",
    "conductor_instantiate_template",
    "conductor_create_gh_issue",
];

pub(super) fn is_read_only(name: &str) -> bool {
    READ_ONLY_TOOLS.contains(&name)
}

pub(super) fn conductor_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "conductor_list_tickets",
            "List tickets for a repo. Filters: label, search, include_closed. \
             Individual tickets with full body available at `conductor://ticket/{repo}/{id}`.",
            schema_typed(&[
                ("repo", "string", "Repo slug (e.g. my-repo)", true),
                (
                    "label",
                    "string_list",
                    "Filter by label name (array of strings or comma-separated, e.g. 'bug,enhancement')",
                    false,
                ),
                (
                    "search",
                    "string",
                    "Text search against ticket title and body",
                    false,
                ),
                (
                    "include_closed",
                    "string",
                    "Set to 'true' to include closed tickets (default: open only)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_list_worktrees",
            "List worktrees for a repo. Defaults to active worktrees only; pass status=all to include merged/abandoned. \
             Individual worktrees available in detail via the `conductor://worktree/{repo}/{slug}` resource. \
             Supports pagination via limit (default 50) and offset (default 0).",
            schema(&[
                ("repo", "Repo slug", true),
                ("status", "Filter by status: 'active' (default) or 'all'", false),
                ("limit", "Max worktrees to return (default 50)", false),
                (
                    "offset",
                    "Number of worktrees to skip for pagination (default 0)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_create_worktree",
            "Create a new worktree (git branch + working directory) for a repo.",
            schema(&[
                ("repo", "Repo slug", true),
                (
                    "name",
                    "Worktree name (e.g. feat-my-feature or fix-bug-123)",
                    true,
                ),
                (
                    "ticket_id",
                    "Ticket ID to link (optional) — accepts either the internal ULID or an external source ID (e.g. GitHub issue number '680')",
                    false,
                ),
                (
                    "from_branch",
                    "Branch to fork from at create time (optional). Defaults to the repo default branch.",
                    false,
                ),
                (
                    "force",
                    "Set to 'true' to create the worktree even if the primary checkout has uncommitted changes (default: false). The checkout is left untouched (not fast-forwarded) and the worktree branches from origin/<base>. Ask the user before using it.",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_adopt_worktree",
            "Register an existing on-disk git worktree into conductor without recreating it. \
             Validates the path is a real git worktree of the repo, derives branch and slug from \
             on-disk state, and inserts the worktrees row. Refuses if a row already exists for \
             the same slug.",
            schema(&[
                ("repo", "Repo slug", true),
                ("path", "Absolute path to the existing worktree directory", true),
                (
                    "branch",
                    "Branch name override (auto-detected from `git branch --show-current` if omitted)",
                    false,
                ),
                (
                    "base_branch",
                    "Base branch to record (defaults to repo default branch)",
                    false,
                ),
                (
                    "ticket_id",
                    "Ticket ID to link (optional) — accepts internal ULID or external source ID",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_set_base_branch",
            "Set the recorded base branch for a worktree. Validates that the new base is an ancestor of the worktree HEAD. \
             Pass rebase=true to rebase the branch onto the new base (blocked if the worktree has uncommitted changes). \
             Omit base_branch to reset to the repo default branch (no git validation).",
            schema(&[
                ("repo", "Repo slug", true),
                ("name", "Worktree slug", true),
                (
                    "base_branch",
                    "New base branch to record. Omit to reset to the repo default branch.",
                    false,
                ),
                (
                    "rebase",
                    "Set to 'true' to rebase the worktree branch onto the new base (default: false — rejects on mismatch)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_delete_worktree",
            "Delete a worktree (destructive — removes the git branch and working directory).",
            schema(&[
                ("repo", "Repo slug", true),
                ("slug", "Worktree slug to delete", true),
            ]),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).read_only(false)),
        Tool::new(
            "conductor_sync_tickets",
            "Sync tickets from the configured issue source (GitHub/Jira) for a repo. \
             When ticket_id is provided, re-fetches only that one ticket without closing others.",
            schema(&[
                ("repo", "Repo slug", true),
                (
                    "ticket_id",
                    "Optional: ULID, external source ID (e.g. GitHub issue number or Jira key), \
                     or GitHub PR URL. When provided, re-fetches only this ticket.",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_run_workflow",
            "Start a workflow. Returns run_id immediately; block on it with conductor_wait_run. \
             The run executes in its own process on the installed conductor, so it survives an MCP restart. \
             Provide worktree or pr to target a specific context. \
             Use pr (PR number or URL) as a shortcut — it resolves the linked worktree automatically.",
            {
                let mut props = serde_json::Map::new();
                props.insert(
                    "workflow".into(),
                    json!({ "type": "string", "description": "Workflow name" }),
                );
                props.insert(
                    "repo".into(),
                    json!({ "type": "string", "description": "Repo slug" }),
                );
                props.insert(
                    "worktree".into(),
                    json!({ "type": "string", "description": "Worktree slug or branch name (optional)" }),
                );
                props.insert(
                    "pr".into(),
                    json!({ "type": "string", "description": "PR number or URL to target. Resolves the linked worktree automatically. Mutually exclusive with `worktree`." }),
                );
                props.insert(
                    "inputs".into(),
                    json!({
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Input key=value pairs (optional). Recognized keys: \
                            `ticket_id` — accepts either the internal ULID or the external source ID \
                            (e.g. GitHub issue number '382'); required for ticket-targeted workflows \
                            that use {{ticket_url}} or other {{ticket_*}} template variables. \
                            `pr_number` — PR number when targeting a pull request context."
                    }),
                );
                props.insert(
                    "dry_run".into(),
                    json!({
                        "type": "boolean",
                        "description": "If true, run in dry-run mode: gates are auto-approved, committing agents are prefixed with 'DO NOT commit or push', and the {{dry_run}} template variable is set to \"true\". No code is pushed and no GitHub side effects occur."
                    }),
                );
                let mut s = serde_json::Map::new();
                s.insert("type".into(), Value::String("object".into()));
                s.insert("properties".into(), Value::Object(props));
                s.insert(
                    "required".into(),
                    Value::Array(vec![
                        Value::String("workflow".into()),
                        Value::String("repo".into()),
                    ]),
                );
                Arc::new(s)
            },
        ),
        Tool::new(
            "conductor_list_runs",
            "List recent workflow runs, optionally filtered by repo, worktree, and/or status. \
             When repo is omitted, runs across all registered repos are returned and each row \
             includes a repo field. Supports pagination via limit (default 50) and offset (default 0). \
             By default, runs on worktrees that are no longer active (merged or abandoned) are omitted; \
             pass include_inactive=true to include them (repo-level runs with no worktree are always included). \
             Each row also shows, when known: ended_at, worktree_slug and worktree_branch, child: true with \
             parent_run_id for child runs, the ticket (source id), the pr number and the target label. \
             Pass top_level_only=true to hide child runs. \
             Dry runs (`conductor workflow validate --dry-run` smokes and runs started with dry_run) and their \
             child runs are hidden unless include_dry_run=true; each dry-run row shows dry_run: true. \
             For full step detail and conversation log on a specific run, read the `conductor://run/{run_id}` resource.",
            schema_typed(&[
                (
                    "repo",
                    "string",
                    "Repo slug (optional; omit to list runs across all repos)",
                    false,
                ),
                (
                    "worktree",
                    "string",
                    "Worktree slug or branch name to filter by (optional)",
                    false,
                ),
                (
                    "status",
                    "string",
                    "Filter by run status: pending, running, completed, failed, cancelled, waiting (optional)",
                    false,
                ),
                ("limit", "string", "Max runs to return (default 50)", false),
                (
                    "offset",
                    "string",
                    "Number of runs to skip for pagination (default 0)",
                    false,
                ),
                (
                    "include_inactive",
                    "boolean",
                    "Include runs on merged or abandoned worktrees (default false)",
                    false,
                ),
                (
                    "top_level_only",
                    "boolean",
                    "Hide child runs (runs started by another workflow run) (default false)",
                    false,
                ),
                (
                    "include_dry_run",
                    "boolean",
                    "Include dry-run runs (validate --dry-run smokes and runs started with dry_run) and their child runs (default false)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_list_agent_runs",
            "List agent runs with optional filters for repo, worktree, and/or status. \
             Use status=waiting_for_feedback to discover runs that are awaiting human input; \
             pass the run_id to conductor_submit_agent_feedback to unblock them. \
             Use status=idle for interactive sessions between turns (alive, awaiting the developer). \
             When repo is omitted, runs across all repos are returned. \
             Supports pagination via limit (default 50) and offset (default 0).",
            schema(&[
                (
                    "repo",
                    "Repo slug (optional; omit to list runs across all repos)",
                    false,
                ),
                (
                    "worktree",
                    "Worktree slug or branch name to filter by (optional; requires repo)",
                    false,
                ),
                (
                    "status",
                    "Filter by run status: running, waiting_for_feedback, idle, completed, failed, cancelled (optional)",
                    false,
                ),
                ("limit", "Max runs to return (default 50)", false),
                (
                    "offset",
                    "Number of runs to skip for pagination (default 0)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_get_run",
            "Get the status and step details of a workflow run. \
             For richer detail including the conversation log tail (last 20 messages from \
             the Claude Code session), read the `conductor://run/{run_id}` resource — \
             this context is not available from this tool. \
             Appends a warning when any running conductor process predates its installed binary.",
            schema(&[("run_id", "Workflow run ID", true)]),
        ),
        Tool::new(
            "conductor_get_review",
            "A review-pr run's review as structured data: the review-aggregator output \
             (blocking_findings, answered_findings, off_diff_findings, overall_approved, summary, \
             reviewed_by), the run status, the scope reviewed (full, or incremental since a sha), \
             the reviewers that ran or were skipped, and the posted GitHub review (url, id, event, overall_approved). \
             Gate on the top-level `approved`, not review.overall_approved: the submit-review \
             step can override the aggregator, and a note says so. \
             Read-only, DB only. Pass run_id (a review-pr run, or a parent ticket-to-pr or \
             iterate-pr run, which resolves to its latest review) or repo + pr to get the latest \
             review-pr run for that PR.",
            schema_typed(&[
                (
                    "run_id",
                    "string",
                    "review-pr run ID, or a parent run ID (resolves to its latest review-pr run)",
                    false,
                ),
                (
                    "repo",
                    "string",
                    "Repo slug; use with pr instead of run_id",
                    false,
                ),
                ("pr", "integer", "PR number (requires repo)", false),
            ]),
        ),
        Tool::new(
            "conductor_run_stats",
            "Time, review cycles, tokens and cost for a workflow run and its child runs: wall \
             time per step and per review round (one cycle per round), blocking findings by reviewer, triage and verify \
             outcomes, and tokens, cache, cost and turns summed from agent runs. Read-only. \
             Pass several comma-separated run ids to combine them (e.g. a ticket-to-pr run and \
             its follow-up iterate-pr run); set compare_run_id to compare against another run.",
            schema_typed(&[
                (
                    "run_id",
                    "string_list",
                    "Workflow run ID; an array or comma-separated IDs are combined into one report",
                    true,
                ),
                (
                    "compare_run_id",
                    "string_list",
                    "Run ID(s) to compare against (array of strings or comma-separated)",
                    false,
                ),
                (
                    "json",
                    "boolean",
                    "Return JSON instead of the text report",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_wait_run",
            "Block until a workflow run (and its child runs) reaches a terminal status, its \
             engine is detected dead (stalled), or timeout_secs passes. Returns the outcome \
             (completed|failed|cancelled|timeout|stalled|gate_waiting), error, and the last step with its \
             context. gate_waiting means a step in the run or a child run waits at a gate for a person: \
             the result lists each gate's run_id, step, type, prompt, timeout, options and plan path. \
             Approve it (conductor_approve_gate with the gate's run_id), then call again. Pass \
             through_gates to keep waiting instead. Prefer this over polling conductor_get_run. If the outcome is timeout the run is \
             still going: call conductor_wait_run again. Without a progress token the wait is \
             capped at 1500s so clients that abort silent calls after 30 minutes don't cut it off.",
            Arc::new({
                let mut m = serde_json::Map::new();
                m.insert("type".into(), json!("object"));
                m.insert("properties".into(), json!({
                    "run_id": { "type": "string", "description": "Workflow run ID" },
                    "timeout_secs": {
                        "type": "integer",
                        "description": format!(
                            "Give up after this many seconds (default {}, max {} when the client sends a progress token, \
                             otherwise capped at {}). On outcome: timeout, call again.",
                            runs::WAIT_DEFAULT_TIMEOUT_SECS,
                            runs::WAIT_MAX_TIMEOUT_SECS,
                            runs::WAIT_MAX_TIMEOUT_NO_PROGRESS_SECS
                        )
                    },
                    "through_gates": {
                        "type": "boolean",
                        "description": "Keep waiting through human gates instead of returning gate_waiting (default false)"
                    },
                    "stall_after_secs": {
                        "type": "integer",
                        "description": format!(
                            "Report a stall when a non-terminal run shows no lease or step activity for this many seconds (default {})",
                            conductor_core::workflow::WaitOptions::default().stall_after.as_secs()
                        )
                    }
                }));
                m.insert("required".into(), json!(["run_id"]));
                m
            }),
        ),
        Tool::new(
            "conductor_approve_gate",
            "Approve a waiting gate in a workflow run. \
             When the gate has multi-select options, pass the chosen values as `selections`. \
             An empty selections array means 'approve with nothing selected' (skip). \
             The run continues on its own once the gate is approved; don't call \
             conductor_resume_run unless the run's process has died.",
            Arc::new({
                let mut m = serde_json::Map::new();
                m.insert("type".into(), json!("object"));
                m.insert("properties".into(), json!({
                    "run_id": { "type": "string", "description": "Workflow run ID" },
                    "feedback": { "type": "string", "description": "Optional feedback or approval message" },
                    "selections": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of selected option values for multi-select gates"
                    }
                }));
                m.insert("required".into(), json!(["run_id"]));
                m
            }),
        ),
        Tool::new(
            "conductor_reject_gate",
            "Reject a waiting gate in a workflow run.",
            schema(&[
                ("run_id", "Workflow run ID", true),
                ("feedback", "Optional feedback or rejection reason", false),
            ]),
        ),
        Tool::new(
            "conductor_push_worktree",
            "Push the current branch of a worktree to the remote.",
            schema(&[("repo", "Repo slug", true), ("slug", "Worktree slug", true)]),
        ),
        Tool::new(
            "conductor_open_session",
            "Open, resume or locate a worktree's interactive Claude Code session in tmux. \
             If a session is already live, returns where it is; otherwise resumes the newest \
             past session, or starts a fresh one (`new`). Never switches the user's tmux focus: \
             it returns the attach command instead.",
            schema_typed(&[
                ("repo", "string", "Repo slug", true),
                ("slug", "string", "Worktree slug or branch", true),
                (
                    "new",
                    "boolean",
                    "Start a fresh session even when a past session exists",
                    false,
                ),
                (
                    "prompt",
                    "string",
                    "Start prompt; overrides any generated opener",
                    false,
                ),
                (
                    "window",
                    "string",
                    "tmux window name (default: ticket key, or the worktree slug)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_take_over_session",
            "Take over a finished (completed/failed/cancelled) headless agent run's Claude \
             session interactively in tmux; records an interactive run linked to it. Never \
             switches the user's tmux focus; returns the attach command.",
            schema_typed(&[
                (
                    "run_id",
                    "string",
                    "Agent run ID to take over (must be completed, failed or cancelled)",
                    true,
                ),
                (
                    "window",
                    "string",
                    "tmux window name (default: ticket key, or the worktree slug)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_cancel_run",
            "Cancel a workflow run that is pending, running, or waiting. \
             In-progress steps and child agent runs are best-effort cancelled before \
             the run is marked cancelled.",
            schema(&[("run_id", "Workflow run ID to cancel", true)]),
        ),
        Tool::new(
            "conductor_list_workflows",
            "List available workflow definitions for a repo. Returns workflow names, descriptions, trigger types, targets, group, and input schemas (name, required, default, description for each input). \
             Full workflow definitions also available at `conductor://workflows/{repo}`.",
            schema(&[
                ("repo", "Repo slug (e.g. my-repo)", true),
                (
                    "worktree",
                    "Worktree slug or branch name (optional). When provided, lists workflows available in the worktree branch.",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_list_repos",
            "List all registered repos and their slugs. Includes active run counts per repo \
             (running, waiting, pending) so you can triage work across repos at a glance. \
             Use this to discover valid repo slugs required by other conductor tools. \
             Also available as resources: `conductor://repos` (full list) and `conductor://repo/{slug}` (single repo detail).",
            schema(&[]),
        ),
        Tool::new(
            "conductor_resume_run",
            "Resume a failed or paused workflow run from its last failed step, in its own process on the installed conductor. \
             Use conductor_get_run to check the run status before resuming. A run waiting at a \
             gate whose engine is still alive resumes itself after approval, so don't resume it.",
            schema(&[
                ("run_id", "Workflow run ID to resume", true),
                (
                    "from_step",
                    "Optional: resume from this specific named step instead of the last failed step. Use `conductor_get_run` to list step names for this run before specifying a value.",
                    false,
                ),
                (
                    "model",
                    "Optional: override the Claude model for resumed agent steps",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_submit_agent_feedback",
            "Submit feedback to an agent run that is waiting for input \
             (status: waiting_for_feedback). Finds the pending feedback request \
             for the given run_id and delivers the response, resuming the agent.",
            schema(&[
                ("run_id", "Agent run ID that is waiting for feedback", true),
                ("feedback", "The feedback or answer to deliver to the agent", true),
            ]),
        ),
        Tool::new(
            "conductor_get_worktree",
            "Get rich detail for a single worktree: branch, status, path, model, \
             linked ticket, associated PR with CI status, and latest agent/workflow run. \
             Also available as the `conductor://worktree/{repo}/{slug}` resource.",
            schema(&[
                ("repo", "Repo slug", true),
                ("slug", "Worktree slug or branch name (e.g. feat/my-feature)", true),
            ]),
        ),
        Tool::new(
            "conductor_get_step_log",
            "Retrieve a workflow step's agent log. Defaults to the agent's final result \
             message (mode=result), with a header showing the step status, agent run id and \
             step error. mode=tail returns the last N log messages (lines, default 20, max \
             200); mode=full returns the whole log, which can be hundreds of KB; mode=prompt returns the \
             prompt the agent was given, as stored on its run, with its length in characters and \
             any run-time prompt warnings (agent steps only). Use this to \
             diagnose step failures. The step must have an associated agent run (gate steps \
             and skipped steps do not have logs). For a script step the log is the script's \
             stderr and any stdout outside FLOW_OUTPUT (the last ~64 KiB of the latest attempt); \
             result mode returns all of it and tail mode its last lines.",
            schema_typed(&[
                ("run_id", "string", "Workflow run ID", true),
                ("step_name", "string", "Step name (as shown in conductor_get_run output)", true),
                (
                    "mode",
                    "string",
                    "One of 'result' (default; the final result message), 'tail' (last N messages), 'full' (whole log) or 'prompt' (the stored prompt)",
                    false,
                ),
                (
                    "lines",
                    "integer",
                    "Number of messages for mode=tail (default 20, max 200)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_list_prs",
            "List open pull requests for a repo. Returns PR number, title, URL, branch, \
             author, draft status, review decision, and CI status for each open PR. \
             When a local conductor worktree is linked to a PR's branch, the output also \
             includes worktree_slug and worktree_status for that PR.",
            schema(&[("repo", "Repo slug (e.g. my-repo)", true)]),
        ),
        Tool::new(
            "conductor_validate_workflow",
            "Validate a workflow definition. Checks for missing agents, missing prompt \
             snippets, cycles, and semantic errors (dataflow, required inputs). \
             Returns pass/fail status and a list of errors if any are found.",
            schema(&[
                ("repo", "Repo slug (e.g. my-repo)", true),
                ("workflow", "Workflow name (without .wf extension)", true),
                (
                    "worktree",
                    "Worktree slug or branch name. Required when the workflow only exists on a feature branch, not on main.",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_register_repo",
            "Register a repo with conductor. The slug is derived from the remote URL \
             (e.g. 'https://github.com/acme/my-repo' → slug 'my-repo'). \
             The local_path defaults to ~/.conductor/workspaces/<slug>/main if not provided. \
             After registering, the repo is available to all other conductor tools.",
            schema(&[
                ("remote_url", "Remote URL of the repo (e.g. https://github.com/acme/my-repo)", true),
                (
                    "local_path",
                    "Local path to the repo checkout (optional; defaults to ~/.conductor/workspaces/<slug>/main)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_unregister_repo",
            "Unregister a repo from conductor. Non-destructive: only removes the conductor \
             DB record — the local directory is not modified or deleted.",
            schema(&[("repo", "Repo slug to unregister (e.g. my-repo)", true)]),
        )
        .with_annotations(ToolAnnotations::new().destructive(false).read_only(false)),
        Tool::new(
            "conductor_delete_ticket",
            "Delete a ticket from conductor by its source key (repo, source_type, source_id). \
             Cleans up associated workflow run links before deleting.",
            schema(&[
                ("repo",        "Repo slug",                                           true),
                ("source_type", "Source identifier, e.g. 'github', 'jira', 'linear'",  true),
                ("source_id",   "Unique ID in the source system",                       true),
            ]),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).read_only(false)),
        Tool::new(
            "conductor_list_templates",
            "List available workflow templates (built-in scaffolding). Returns template name, \
             description, version, target types, and hints for each embedded template.",
            schema(&[]),
        ),
        Tool::new(
            "conductor_instantiate_template",
            "Generate an agent prompt to instantiate a workflow template for a repo. \
             Returns the full prompt that an agent should use to customize and write \
             the workflow file. Use conductor_list_templates first to see available templates.",
            schema(&[
                ("template", "Template name (from conductor_list_templates)", true),
                ("repo", "Repo slug", true),
                (
                    "worktree",
                    "Worktree slug (optional; uses repo root if omitted)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_create_gh_issue",
            "Create a GitHub issue in a registered repo. Returns the issue number and URL. \
             Optionally files it in an open milestone and links it to a conductor run for tracking.",
            schema_typed(&[
                ("repo", "string", "Repo slug (registered conductor repo)", true),
                ("title", "string", "Issue title", true),
                ("body", "string", "Issue body (markdown)", true),
                (
                    "labels",
                    "string_list",
                    "Label names: an array of strings or a comma-separated string (optional)",
                    false,
                ),
                (
                    "milestone",
                    "string",
                    "Milestone title to file the issue in (optional; must be an open milestone in the repo, otherwise the call fails and lists them)",
                    false,
                ),
                (
                    "run_id",
                    "string",
                    "Run to link the issue to (optional): an agent run ID, or a workflow run ID, which links to the agent run that drives that workflow. An unknown ID fails before the issue is created.",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_upsert_ticket",
            "Upsert a ticket from any external source into conductor. Idempotent on (repo, source_type, source_id). \
             Use this from a sync workflow to keep tickets current without modifying conductor-ai source code. \
             Dependency fields use replace semantics: `children` replaces all child relationships on every call \
             (omit children to leave existing children unchanged); `parent` replaces any existing parent relationship.",
            schema_typed(&[
                ("repo", "string", "Repo slug", true),
                ("source_type", "string", "Free-form source identifier, e.g. 'sdlc', 'linear'", true),
                ("source_id", "string", "Unique ID in the source system", true),
                ("title", "string", "Ticket title", true),
                ("state", "string", "open | in_progress | closed", true),
                ("body", "string", "Ticket body/description", false),
                ("url", "string", "URL to the ticket in the source system", false),
                ("labels", "string_list", "Label names (array of strings or comma-separated)", false),
                ("assignee", "string", "Assignee username or name", false),
                ("priority", "string", "Priority string", false),
                ("blocked_by", "string_list", "Source IDs (array of strings or comma-separated) of tickets that block this one", false),
                ("children", "string_list", "Source IDs (array of strings or comma-separated) of child tickets (this ticket is parent). REPLACES existing children on every call — include all children or previously set ones will be removed.", false),
                ("parent", "string", "Source ID of the parent ticket within the same source_type. Sets this ticket as a child of that parent. REPLACES any existing parent — last write wins.", false),
            ]),
        ),
        Tool::new(
            "conductor_get_ready_tickets",
            "Return open tickets that have no unresolved blockers and no active workflow run. \
             A ticket is ready when all tickets that block it are closed (and their workflow runs, if any, \
             are completed), and the ticket itself has no running or waiting workflow run. \
             Optionally scope to direct children of a parent ticket, filter by label, or cap results with limit.",
            schema_typed(&[
                ("repo", "string", "Repo slug (e.g. my-repo)", true),
                (
                    "root_ticket_id",
                    "string",
                    "Scope to direct children of this ticket ULID (optional)",
                    false,
                ),
                ("label", "string", "Filter by label name (optional)", false),
                ("limit", "integer", "Max tickets to return (optional)", false),
            ]),
        ),
    ]
}

pub(super) fn dispatch_tool(
    conductor: &Conductor,
    name: &str,
    args: &serde_json::Map<String, Value>,
    progress: Option<&dyn Fn(String)>,
) -> CallToolResult {
    match name {
        "conductor_list_tickets" => tickets::tool_list_tickets(conductor, args),
        "conductor_list_worktrees" => worktrees::tool_list_worktrees(conductor, args),
        "conductor_create_worktree" => worktrees::tool_create_worktree(conductor, args),
        "conductor_delete_worktree" => worktrees::tool_delete_worktree(conductor, args),
        "conductor_sync_tickets" => tickets::tool_sync_tickets(conductor, args),
        "conductor_run_workflow" => workflows::tool_run_workflow(conductor, args),
        "conductor_list_runs" => runs::tool_list_runs(conductor, args),
        "conductor_list_agent_runs" => agents::tool_list_agent_runs(conductor, args),
        "conductor_get_run" => runs::tool_get_run(conductor, args),
        "conductor_approve_gate" => gates::tool_approve_gate(conductor, args),
        "conductor_reject_gate" => gates::tool_reject_gate(conductor, args),
        "conductor_adopt_worktree" => worktrees::tool_adopt_worktree(conductor, args),
        "conductor_push_worktree" => worktrees::tool_push_worktree(conductor, args),
        "conductor_open_session" => worktrees::tool_open_session(conductor, args),
        "conductor_take_over_session" => worktrees::tool_take_over_session(conductor, args),
        "conductor_set_base_branch" => worktrees::tool_set_base_branch(conductor, args),
        "conductor_cancel_run" => runs::tool_cancel_run(conductor, args),
        "conductor_wait_run" => runs::tool_wait_run(conductor, args, progress),
        "conductor_list_workflows" => workflows::tool_list_workflows(conductor, args),
        "conductor_list_repos" => repos::tool_list_repos(conductor),
        "conductor_resume_run" => runs::tool_resume_run(conductor, args),
        "conductor_submit_agent_feedback" => agents::tool_submit_agent_feedback(conductor, args),
        "conductor_get_worktree" => worktrees::tool_get_worktree(conductor, args),
        "conductor_get_step_log" => runs::tool_get_step_log(conductor, args),
        "conductor_run_stats" => runs::tool_run_stats(conductor, args),
        "conductor_get_review" => runs::tool_get_review(conductor, args),
        "conductor_list_prs" => prs::tool_list_prs(conductor, args),
        "conductor_validate_workflow" => workflows::tool_validate_workflow(conductor, args),
        "conductor_register_repo" => repos::tool_register_repo(conductor, args),
        "conductor_unregister_repo" => repos::tool_unregister_repo(conductor, args),
        "conductor_delete_ticket" => tickets::tool_delete_ticket(conductor, args),
        "conductor_upsert_ticket" => tickets::tool_upsert_ticket(conductor, args),
        "conductor_get_ready_tickets" => tickets::tool_get_ready_tickets(conductor, args),
        "conductor_list_templates" => workflows::tool_list_templates(conductor, args),
        "conductor_instantiate_template" => workflows::tool_instantiate_template(conductor, args),
        "conductor_create_gh_issue" => issues::tool_create_gh_issue(conductor, args),
        _ => tool_err(format!("Unknown tool: {name}")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::test_helpers::make_test_conductor;
    use super::*;
    use serde_json::Value;

    #[test]
    fn create_gh_issue_labels_schema_accepts_string_or_array() {
        let tool = conductor_tools()
            .into_iter()
            .find(|t| t.name == "conductor_create_gh_issue")
            .unwrap();
        let any_of = &tool.input_schema["properties"]["labels"]["anyOf"];
        assert_eq!(any_of[0]["type"], "string");
        assert_eq!(any_of[1]["type"], "array");
        assert_eq!(any_of[1]["items"]["type"], "string");
    }

    fn empty_args() -> serde_json::Map<String, Value> {
        serde_json::Map::new()
    }

    #[test]
    fn test_dispatch_unknown_tool() {
        let (_f, conductor) = make_test_conductor();
        let result = dispatch_tool(&conductor, "conductor_nonexistent", &empty_args(), None);
        assert_eq!(
            result.is_error,
            Some(true),
            "unknown tool should return is_error=true"
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Unknown tool"), "got: {text}");
    }

    #[test]
    fn test_every_tool_is_classified_exactly_once() {
        let mut registered: Vec<String> = conductor_tools()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        let mut classified: Vec<String> = READ_ONLY_TOOLS
            .iter()
            .chain(OTHER_TOOLS)
            .map(|s| s.to_string())
            .collect();
        registered.sort();
        classified.sort();
        assert_eq!(
            registered, classified,
            "every registered tool must be listed once in READ_ONLY_TOOLS or OTHER_TOOLS"
        );
    }
}
