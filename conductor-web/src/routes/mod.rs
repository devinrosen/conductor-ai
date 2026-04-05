pub mod agents;
pub mod conversations;
pub mod events;
pub mod features;
pub mod health;
pub mod issue_sources;
pub mod model_config;
pub mod notifications;
pub mod push;
pub mod repos;
pub mod stats;
pub mod tickets;
pub mod workflows;
pub mod worktrees;

use axum::http::HeaderValue;
use axum::routing::{delete, get, patch, post};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};

use crate::state::AppState;

/// Build the API router with CORS restricted to the given origins.
///
/// This keeps CORS configuration inside conductor-web so that embedders
/// (e.g. conductor-desktop) don't need to depend on axum/tower-http directly.
pub fn api_router_with_cors(allowed_origins: Vec<HeaderValue>) -> Router<AppState> {
    let cors = CorsLayer::new()
        .allow_origin(allowed_origins)
        .allow_methods(Any)
        .allow_headers(Any);
    api_router().layer(cors)
}

pub fn api_router() -> Router<AppState> {
    Router::new()
        // Health check
        .route("/health", get(health::health))
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
        .route("/api/worktrees", get(worktrees::list_all_worktrees))
        .route(
            "/api/repos/{id}/worktrees",
            get(worktrees::list_worktrees).post(worktrees::create_worktree),
        )
        .route(
            "/api/worktrees/{id}",
            get(worktrees::get_worktree).delete(worktrees::delete_worktree),
        )
        .route(
            "/api/repos/{repo_id}/worktrees/{id}",
            get(worktrees::get_worktree_for_repo).delete(worktrees::delete_worktree_for_repo),
        )
        .route(
            "/api/worktrees/{id}/model",
            patch(worktrees::patch_worktree_model),
        )
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
            "/api/repos/{id}/workflows",
            get(workflows::list_repo_workflow_defs),
        )
        .route(
            "/api/tickets/{ticket_id}/detail",
            get(tickets::ticket_detail),
        )
        // Features
        .route("/api/repos/{slug}/features", get(features::list_features))
        // Agent stats (aggregates)
        .route(
            "/api/worktrees/{id}/agent-runs",
            get(agents::list_agent_runs),
        )
        .route("/api/agent/runs", get(agents::list_all_agent_runs))
        .route("/api/agent/runs/{id}", get(agents::get_agent_run_by_id))
        .route(
            "/api/agent/runs/{id}/feedback",
            get(agents::get_agent_run_feedback_by_run_id),
        )
        .route(
            "/api/agent/runs/{id}/events",
            get(agents::get_agent_run_events_by_id),
        )
        // Conversations
        .route(
            "/api/conversations",
            get(conversations::list_conversations).post(conversations::create_conversation),
        )
        .route(
            "/api/conversations/{id}",
            get(conversations::get_conversation).delete(conversations::delete_conversation),
        )
        .route(
            "/api/conversations/{id}/messages",
            post(conversations::send_message),
        )
        .route(
            "/api/conversations/{id}/messages/{run_id}/respond",
            post(conversations::respond_to_run_feedback),
        )
        .route(
            "/api/conversations/{id}/feedback",
            post(conversations::respond_to_feedback),
        )
        .route(
            "/api/agent/latest-runs",
            get(agents::latest_runs_by_worktree),
        )
        .route("/api/agent/ticket-totals", get(agents::ticket_totals))
        .route(
            "/api/repos/{id}/agent/latest-runs",
            get(agents::latest_runs_by_worktree_for_repo),
        )
        .route(
            "/api/repos/{id}/agent/ticket-totals",
            get(agents::ticket_totals_for_repo),
        )
        // Repo-scoped agents (read-only)
        .route(
            "/api/repos/{id}/agent/start",
            post(agents::start_repo_agent),
        )
        .route(
            "/api/repos/{id}/agent/runs",
            get(agents::list_repo_agent_runs),
        )
        .route(
            "/api/repos/{id}/agent/{run_id}/stop",
            post(agents::stop_repo_agent),
        )
        .route(
            "/api/repos/{id}/agent/{run_id}/events",
            get(agents::repo_agent_events),
        )
        // Agent orchestration
        .route("/api/worktrees/{id}/agent/runs", get(agents::list_runs))
        .route("/api/worktrees/{id}/agent/latest", get(agents::latest_run))
        .route("/api/worktrees/{id}/agent/start", post(agents::start_agent))
        .route("/api/worktrees/{id}/agent/stop", post(agents::stop_agent))
        .route("/api/worktrees/{id}/agent/events", get(agents::get_events))
        .route(
            "/api/worktrees/{id}/agent/runs/{run_id}/restart",
            post(agents::restart_agent),
        )
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
            "/api/worktrees/{id}/workflows/defs/{name}",
            get(workflows::get_workflow_def),
        )
        .route(
            "/api/worktrees/{id}/workflows/run",
            post(workflows::run_workflow),
        )
        .route(
            "/api/worktrees/{id}/workflows/runs",
            get(workflows::list_workflow_runs),
        )
        .route(
            "/api/workflows/runs",
            get(workflows::list_all_workflow_runs_handler).post(workflows::post_workflow_run),
        )
        .route("/api/workflows/runs/{id}", get(workflows::get_workflow_run))
        .route(
            "/api/workflows/runs/{id}/steps",
            get(workflows::get_workflow_steps),
        )
        .route(
            "/api/workflows/runs/{id}/steps/{step_name}/log",
            get(workflows::get_workflow_step_log),
        )
        .route(
            "/api/workflows/runs/{id}/children",
            get(workflows::get_child_workflow_runs),
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
        // Workflow token analytics
        .route(
            "/api/workflows/analytics/aggregates",
            get(workflows::get_token_aggregates),
        )
        .route(
            "/api/workflows/analytics/trend",
            get(workflows::get_token_trend),
        )
        .route(
            "/api/workflows/analytics/heatmap",
            get(workflows::get_step_heatmap),
        )
        // Workflow Templates
        .route("/api/templates", get(workflows::list_templates))
        .route(
            "/api/templates/instantiate",
            post(workflows::instantiate_template),
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
        // Notifications
        .route("/api/notifications", get(notifications::list_notifications))
        .route(
            "/api/notifications/unread-count",
            get(notifications::unread_count),
        )
        .route(
            "/api/notifications/read-all",
            post(notifications::mark_all_read),
        )
        .route(
            "/api/notifications/{id}/read",
            post(notifications::mark_read),
        )
        // Stats
        .route("/api/stats/theme-unlocks", get(stats::theme_unlock_stats))
        // Push Notifications
        .route(
            "/api/push/vapid-public-key",
            get(push::get_vapid_public_key),
        )
        .route(
            "/api/push/subscribe",
            post(push::subscribe_push).delete(push::unsubscribe_push),
        )
        // Model Config
        .route(
            "/api/config/model",
            get(model_config::get_global_model).patch(model_config::patch_global_model),
        )
        .route(
            "/api/config/known-models",
            get(model_config::list_known_models),
        )
        .route(
            "/api/config/suggest-model",
            post(model_config::suggest_model),
        )
}
