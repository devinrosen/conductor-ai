pub mod agents;
pub mod events;
pub mod issue_sources;
pub mod repos;
pub mod tickets;
pub mod work_targets;
pub mod workflows;
pub mod worktrees;

use axum::routing::{delete, get, patch, post};
use axum::Router;

use crate::state::AppState;

pub fn api_router() -> Router<AppState> {
    Router::new()
        // SSE event stream
        .route("/api/events", get(events::event_stream))
        // Repos
        .route(
            "/api/repos",
            get(repos::list_repos).post(repos::register_repo),
        )
        .route("/api/repos/{id}", delete(repos::unregister_repo))
        .route("/api/repos/{id}/model", patch(repos::patch_repo_model))
        .route(
            "/api/repos/{id}/settings",
            patch(repos::update_repo_settings),
        )
        // GitHub repo discovery
        .route("/api/github/orgs", get(repos::list_github_orgs_handler))
        .route(
            "/api/github/repos",
            get(repos::discover_github_repos_handler),
        )
        // Worktrees
        .route(
            "/api/repos/{id}/worktrees",
            get(worktrees::list_worktrees).post(worktrees::create_worktree),
        )
        .route("/api/worktrees/{id}", delete(worktrees::delete_worktree))
        .route(
            "/api/worktrees/{id}/model",
            patch(worktrees::patch_worktree_model),
        )
        .route("/api/worktrees/{id}/push", post(worktrees::push_worktree))
        .route("/api/worktrees/{id}/pr", post(worktrees::create_pr))
        .route(
            "/api/worktrees/{id}/link-ticket",
            post(worktrees::link_ticket),
        )
        // Tickets
        .route("/api/ticket-labels", get(tickets::list_ticket_labels))
        .route("/api/tickets", get(tickets::list_all_tickets))
        .route("/api/repos/{id}/tickets", get(tickets::list_tickets))
        .route("/api/repos/{id}/tickets/sync", post(tickets::sync_tickets))
        .route(
            "/api/tickets/{ticket_id}/detail",
            get(tickets::ticket_detail),
        )
        // Agent stats (aggregates)
        .route(
            "/api/worktrees/{id}/agent-runs",
            get(agents::list_agent_runs),
        )
        .route(
            "/api/agent/latest-runs",
            get(agents::latest_runs_by_worktree),
        )
        .route("/api/agent/ticket-totals", get(agents::ticket_totals))
        // Agent orchestration
        .route("/api/worktrees/{id}/agent/runs", get(agents::list_runs))
        .route("/api/worktrees/{id}/agent/latest", get(agents::latest_run))
        .route("/api/worktrees/{id}/agent/start", post(agents::start_agent))
        .route("/api/worktrees/{id}/agent/stop", post(agents::stop_agent))
        .route("/api/worktrees/{id}/agent/events", get(agents::get_events))
        .route(
            "/api/worktrees/{id}/agent/runs/{run_id}/events",
            get(agents::get_run_events),
        )
        .route(
            "/api/worktrees/{id}/agent/runs/{run_id}/children",
            get(agents::list_child_runs),
        )
        .route(
            "/api/worktrees/{id}/agent/runs/{run_id}/tree",
            get(agents::get_run_tree),
        )
        .route(
            "/api/worktrees/{id}/agent/runs/{run_id}/tree-totals",
            get(agents::get_run_tree_totals),
        )
        .route("/api/worktrees/{id}/agent/prompt", get(agents::get_prompt))
        .route(
            "/api/worktrees/{id}/agent/orchestrate",
            post(agents::orchestrate_agent),
        )
        .route(
            "/api/worktrees/{id}/agent/created-issues",
            get(agents::list_created_issues),
        )
        // Agent feedback (human-in-the-loop)
        .route(
            "/api/worktrees/{id}/agent/feedback",
            get(agents::get_pending_feedback).post(agents::request_feedback),
        )
        .route(
            "/api/worktrees/{id}/agent/feedback/{feedback_id}/respond",
            post(agents::submit_feedback),
        )
        .route(
            "/api/worktrees/{id}/agent/feedback/{feedback_id}/dismiss",
            post(agents::dismiss_feedback),
        )
        .route(
            "/api/worktrees/{id}/agent/runs/{run_id}/feedback",
            get(agents::list_run_feedback),
        )
        // Workflows
        .route(
            "/api/worktrees/{id}/workflows/defs",
            get(workflows::list_workflow_defs),
        )
        .route(
            "/api/worktrees/{id}/workflows/run",
            post(workflows::run_workflow),
        )
        .route(
            "/api/worktrees/{id}/workflows/runs",
            get(workflows::list_workflow_runs),
        )
        .route("/api/workflows/runs/{id}", get(workflows::get_workflow_run))
        .route(
            "/api/workflows/runs/{id}/steps",
            get(workflows::get_workflow_steps),
        )
        .route(
            "/api/workflows/runs/{id}/cancel",
            post(workflows::cancel_workflow),
        )
        .route(
            "/api/workflows/runs/{id}/resume",
            post(workflows::resume_workflow_endpoint),
        )
        .route(
            "/api/workflows/runs/{id}/gate/approve",
            post(workflows::approve_gate),
        )
        .route(
            "/api/workflows/runs/{id}/gate/reject",
            post(workflows::reject_gate),
        )
        // Issue Sources
        .route(
            "/api/repos/{id}/sources",
            get(issue_sources::list_issue_sources).post(issue_sources::create_issue_source),
        )
        .route(
            "/api/repos/{id}/sources/{source_id}",
            delete(issue_sources::delete_issue_source),
        )
        // Work Targets
        .route(
            "/api/config/model",
            get(work_targets::get_global_model).patch(work_targets::patch_global_model),
        )
        .route(
            "/api/config/known-models",
            get(work_targets::list_known_models),
        )
        .route(
            "/api/config/suggest-model",
            post(work_targets::suggest_model),
        )
        .route(
            "/api/config/work-targets",
            get(work_targets::list_work_targets)
                .post(work_targets::create_work_target)
                .put(work_targets::replace_work_targets),
        )
        .route(
            "/api/config/work-targets/{index}",
            delete(work_targets::delete_work_target),
        )
}
