use std::path::Path;
use std::sync::Arc;

use rmcp::model::{CallToolResult, Tool, ToolAnnotations};
use serde_json::{json, Value};

use crate::mcp::helpers::{schema, tool_err};

mod agents;
mod gates;
mod issues;
mod prs;
mod repos;
mod runs;
mod tickets;
mod workflows;
mod worktrees;

pub(super) fn conductor_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "conductor_list_tickets",
            "List tickets for a repo. Filters: label, search, include_closed. \
             Individual tickets with full body available at `conductor://ticket/{repo}/{id}`.",
            schema(&[
                ("repo", "Repo slug (e.g. my-repo)", true),
                (
                    "label",
                    "Filter by label name (comma-separated for multiple, e.g. 'bug,enhancement')",
                    false,
                ),
                ("search", "Text search against ticket title and body", false),
                (
                    "include_closed",
                    "Set to 'true' to include closed tickets (default: open only)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_list_worktrees",
            "List worktrees for a repo. Defaults to active worktrees only; pass status=all to include merged/abandoned. \
             Individual worktrees available in detail via the `conductor://worktree/{repo}/{slug}` resource.",
            schema(&[
                ("repo", "Repo slug", true),
                ("status", "Filter by status: 'active' (default) or 'all'", false),
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
            "Start a workflow. Returns run_id immediately; poll with conductor_get_run. \
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
                        "description": "Input key=value pairs (optional)"
                    }),
                );
                props.insert(
                    "feature".into(),
                    json!({
                        "type": "string",
                        "description": "Feature name to associate with the workflow run. When omitted, auto-detects from the worktree's linked ticket."
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
             For full step detail and conversation log on a specific run, read the `conductor://run/{run_id}` resource.",
            schema(&[
                (
                    "repo",
                    "Repo slug (optional; omit to list runs across all repos)",
                    false,
                ),
                (
                    "worktree",
                    "Worktree slug or branch name to filter by (optional)",
                    false,
                ),
                (
                    "status",
                    "Filter by run status: pending, running, completed, failed, cancelled, waiting (optional)",
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
            "conductor_list_agent_runs",
            "List agent runs with optional filters for repo, worktree, and/or status. \
             Use status=waiting_for_feedback to discover runs that are awaiting human input; \
             pass the run_id to conductor_submit_agent_feedback to unblock them. \
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
                    "Filter by run status: running, waiting_for_feedback, completed, failed, cancelled (optional)",
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
             this context is not available from this tool.",
            schema(&[("run_id", "Workflow run ID", true)]),
        ),
        Tool::new(
            "conductor_approve_gate",
            "Approve a waiting gate in a workflow run.",
            schema(&[
                ("run_id", "Workflow run ID", true),
                ("feedback", "Optional feedback or approval message", false),
            ]),
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
            "conductor_cancel_run",
            "Cancel a workflow run that is pending, running, or waiting. \
             In-progress steps and child agent runs are best-effort cancelled before \
             the run is marked cancelled.",
            schema(&[("run_id", "Workflow run ID to cancel", true)]),
        ),
        Tool::new(
            "conductor_list_workflows",
            "List available workflow definitions for a repo. Returns workflow names, descriptions, trigger types, and input schemas (name, required, default, description for each input). \
             Full workflow definitions also available at `conductor://workflows/{repo}`.",
            schema(&[("repo", "Repo slug (e.g. my-repo)", true)]),
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
            "Resume a failed or paused workflow run from its last failed step. \
             Use conductor_get_run to check the run status before resuming.",
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
            "Retrieve the full agent log for a named step in a workflow run. \
             Use this to diagnose step failures. The step must have an associated agent run \
             (gate steps and skipped steps do not have logs).",
            schema(&[
                ("run_id", "Workflow run ID", true),
                ("step_name", "Step name (as shown in conductor_get_run output)", true),
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
            "conductor_create_gh_issue",
            "Create a GitHub issue in a registered repo. Returns the issue number and URL. \
             Optionally links the created issue to an agent run for tracking.",
            schema(&[
                ("repo", "Repo slug (registered conductor repo)", true),
                ("title", "Issue title", true),
                ("body", "Issue body (markdown)", true),
                ("labels", "Comma-separated label names (optional)", false),
                ("run_id", "Agent run ID for tracking (optional)", false),
            ]),
        ),
        Tool::new(
            "conductor_upsert_ticket",
            "Upsert a ticket from any external source into conductor. Idempotent on (repo, source_type, source_id). \
             Use this from a sync workflow to keep tickets current without modifying conductor-ai source code.",
            schema(&[
                ("repo",        "Repo slug",                                           true),
                ("source_type", "Free-form source identifier, e.g. 'sdlc', 'linear'", true),
                ("source_id",   "Unique ID in the source system",                      true),
                ("title",       "Ticket title",                                        true),
                ("state",       "open | in_progress | closed",                         true),
                ("body",        "Ticket body/description",                             false),
                ("url",         "URL to the ticket in the source system",              false),
                ("labels",      "Comma-separated label names",                         false),
                ("assignee",    "Assignee username or name",                           false),
                ("priority",    "Priority string",                                     false),
            ]),
        ),
    ]
}

pub(super) fn dispatch_tool(
    db_path: &Path,
    name: &str,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    match name {
        "conductor_list_tickets" => tickets::tool_list_tickets(db_path, args),
        "conductor_list_worktrees" => worktrees::tool_list_worktrees(db_path, args),
        "conductor_create_worktree" => worktrees::tool_create_worktree(db_path, args),
        "conductor_delete_worktree" => worktrees::tool_delete_worktree(db_path, args),
        "conductor_sync_tickets" => tickets::tool_sync_tickets(db_path, args),
        "conductor_run_workflow" => workflows::tool_run_workflow(db_path, args),
        "conductor_list_runs" => runs::tool_list_runs(db_path, args),
        "conductor_list_agent_runs" => agents::tool_list_agent_runs(db_path, args),
        "conductor_get_run" => runs::tool_get_run(db_path, args),
        "conductor_approve_gate" => gates::tool_approve_gate(db_path, args),
        "conductor_reject_gate" => gates::tool_reject_gate(db_path, args),
        "conductor_push_worktree" => worktrees::tool_push_worktree(db_path, args),
        "conductor_cancel_run" => runs::tool_cancel_run(db_path, args),
        "conductor_list_workflows" => workflows::tool_list_workflows(db_path, args),
        "conductor_list_repos" => repos::tool_list_repos(db_path),
        "conductor_resume_run" => runs::tool_resume_run(db_path, args),
        "conductor_submit_agent_feedback" => agents::tool_submit_agent_feedback(db_path, args),
        "conductor_get_worktree" => worktrees::tool_get_worktree(db_path, args),
        "conductor_get_step_log" => runs::tool_get_step_log(db_path, args),
        "conductor_list_prs" => prs::tool_list_prs(db_path, args),
        "conductor_validate_workflow" => workflows::tool_validate_workflow(db_path, args),
        "conductor_register_repo" => repos::tool_register_repo(db_path, args),
        "conductor_unregister_repo" => repos::tool_unregister_repo(db_path, args),
        "conductor_delete_ticket" => tickets::tool_delete_ticket(db_path, args),
        "conductor_upsert_ticket" => tickets::tool_upsert_ticket(db_path, args),
        "conductor_create_gh_issue" => issues::tool_create_gh_issue(db_path, args),
        _ => tool_err(format!("Unknown tool: {name}")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn make_test_db() -> (tempfile::NamedTempFile, std::path::PathBuf) {
        use conductor_core::db::open_database;
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        open_database(&path).expect("open_database");
        (file, path)
    }

    fn empty_args() -> serde_json::Map<String, Value> {
        serde_json::Map::new()
    }

    #[test]
    fn test_dispatch_unknown_tool() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_nonexistent", &empty_args());
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
}
