pub mod agents;
pub mod events;
pub mod repos;
pub mod tickets;
pub mod worktrees;

use axum::routing::{delete, get, post};
use axum::Router;

use crate::state::AppState;

pub fn api_router() -> Router<AppState> {
    Router::new()
        // SSE event stream
        .route("/api/events", get(events::event_stream))
        // Repos
        .route(
            "/api/repos",
            get(repos::list_repos).post(repos::create_repo),
        )
        .route("/api/repos/{id}", delete(repos::delete_repo))
        // Worktrees
        .route(
            "/api/repos/{id}/worktrees",
            get(worktrees::list_worktrees).post(worktrees::create_worktree),
        )
        .route("/api/worktrees/{id}", delete(worktrees::delete_worktree))
        // Tickets
        .route("/api/repos/{id}/tickets", get(tickets::list_tickets))
        .route("/api/repos/{id}/tickets/sync", post(tickets::sync_tickets))
        // Agent runs & stats
        .route(
            "/api/worktrees/{id}/agent-runs",
            get(agents::list_agent_runs),
        )
        .route(
            "/api/agent/latest-runs",
            get(agents::latest_runs_by_worktree),
        )
        .route("/api/agent/ticket-totals", get(agents::ticket_totals))
}
