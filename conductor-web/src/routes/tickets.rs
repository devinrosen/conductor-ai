use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use tracing::warn;

use conductor_core::agent::{AgentManager, TicketAgentTotals};
use conductor_core::error::ConductorError;
use conductor_core::github;
use conductor_core::github_app;
use conductor_core::issue_source::IssueSourceManager;
use conductor_core::repo::RepoManager;
use conductor_core::ticket_source::TicketSource;
use conductor_core::tickets::{Ticket, TicketDependencies, TicketInput, TicketLabel, TicketSyncer};
use conductor_core::worktree::{Worktree, WorktreeManager};

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

#[derive(Serialize)]
pub struct SyncResult {
    pub synced: usize,
    pub closed: usize,
}

#[derive(Serialize)]
pub struct TicketDetail {
    pub agent_totals: Option<TicketAgentTotals>,
    pub worktrees: Vec<Worktree>,
    pub dependencies: TicketDependencies,
}

#[derive(Debug, Deserialize)]
pub struct TicketListQuery {
    /// When true, include closed tickets. Defaults to false (closed tickets hidden).
    #[serde(default)]
    pub show_closed: bool,
}

pub async fn list_all_tickets(
    State(state): State<AppState>,
    Query(params): Query<TicketListQuery>,
) -> Result<Json<Vec<Ticket>>, ApiError> {
    let db = state.db.lock().await;
    let syncer = TicketSyncer::new(&db);
    let mut tickets = syncer.list(None)?;
    if !params.show_closed {
        tickets.retain(|t| t.state != "closed");
    }
    Ok(Json(tickets))
}

pub async fn list_tickets(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    Query(params): Query<TicketListQuery>,
) -> Result<Json<Vec<Ticket>>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    RepoManager::new(&db, &config).get_by_id(&repo_id)?;
    let syncer = TicketSyncer::new(&db);
    let mut tickets = syncer.list(Some(&repo_id))?;
    if !params.show_closed {
        tickets.retain(|t| t.state != "closed");
    }
    Ok(Json(tickets))
}

/// Fetch tickets using `fetch`, then apply the sync (upsert + close + mark worktrees).
/// Returns `(synced, closed)` counts. Fetch errors are logged as warnings.
fn sync_source(
    syncer: &TicketSyncer,
    repo_id: &str,
    source_type: &str,
    fetch: impl FnOnce() -> Result<Vec<TicketInput>, ConductorError>,
) -> (usize, usize) {
    match fetch() {
        Ok(tickets) => syncer.sync_and_close_tickets(repo_id, source_type, &tickets),
        Err(e) => {
            warn!("sync {source_type} failed for {repo_id}: {e}");
            (0, 0)
        }
    }
}

pub async fn sync_tickets(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<SyncResult>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let repo = RepoManager::new(&db, &config).get_by_id(&repo_id)?;
    let source_mgr = IssueSourceManager::new(&db);
    let syncer = TicketSyncer::new(&db);

    let sources = source_mgr.list(&repo.id)?;
    let token_res = github_app::resolve_app_token(&config, "github-issues-sync");
    let token = token_res.token();
    let mut total_synced = 0usize;
    let mut total_closed = 0usize;

    if sources.is_empty() {
        // Backward compat: auto-detect GitHub from remote URL
        if let Some((owner, name)) = github::parse_github_remote(&repo.remote_url) {
            let (synced, closed) = sync_source(&syncer, &repo.id, "github", || {
                github::sync_github_issues(&owner, &name, token)
            });
            total_synced += synced;
            total_closed += closed;
        }
    } else {
        for source in sources {
            if let Ok(ts) = TicketSource::from_issue_source(&source) {
                let ts = ts.with_repo_slug(&repo.slug);
                let source_type_str = ts.source_type_str();
                let (synced, closed) =
                    sync_source(&syncer, &repo.id, source_type_str, || ts.sync(token));
                total_synced += synced;
                total_closed += closed;
            }
        }
    }

    state.events.emit(ConductorEvent::TicketsSynced {
        repo_id: repo.id.clone(),
    });
    Ok(Json(SyncResult {
        synced: total_synced,
        closed: total_closed,
    }))
}

pub async fn list_ticket_labels(
    State(state): State<AppState>,
) -> Result<Json<Vec<TicketLabel>>, ApiError> {
    let db = state.db.lock().await;
    let syncer = TicketSyncer::new(&db);
    let map = syncer.get_all_labels()?;
    let labels: Vec<TicketLabel> = map.into_values().flatten().collect();
    Ok(Json(labels))
}

pub async fn ticket_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TicketDetail>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;

    let agent_mgr = AgentManager::new(&db);
    let all_totals = agent_mgr.totals_by_ticket_all()?;
    let agent_totals = all_totals.get(&id).cloned();

    let wt_mgr = WorktreeManager::new(&db, &config);
    let worktrees = wt_mgr.list_by_ticket(&id)?;

    let syncer = TicketSyncer::new(&db);
    let dependencies = syncer.get_dependencies(&id)?;

    Ok(Json(TicketDetail {
        agent_totals,
        worktrees,
        dependencies,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use conductor_core::config::Config;
    use conductor_core::tickets::{TicketInput, TicketSyncer};
    use tokio::sync::{Mutex, RwLock};
    use tower::ServiceExt;

    use crate::events::EventBus;
    use crate::routes::api_router;

    /// Build an AppState with an in-memory DB seeded with one open ticket (source_id "10")
    /// and one closed ticket (source_id "11").
    fn seeded_state() -> AppState {
        let conn = conductor_core::test_helpers::setup_db();
        let syncer = TicketSyncer::new(&conn);
        let tickets = vec![
            TicketInput {
                source_type: "github".to_string(),
                source_id: "10".to_string(),
                title: "Open issue".to_string(),
                body: String::new(),
                state: "open".to_string(),
                labels: vec![],
                assignee: None,
                priority: None,
                url: String::new(),
                raw_json: "{}".to_string(),
                label_details: vec![],
                blocked_by: vec![],
                children: vec![],
            },
            TicketInput {
                source_type: "github".to_string(),
                source_id: "11".to_string(),
                title: "Closed issue".to_string(),
                body: String::new(),
                state: "open".to_string(),
                labels: vec![],
                assignee: None,
                priority: None,
                url: String::new(),
                raw_json: "{}".to_string(),
                label_details: vec![],
                blocked_by: vec![],
                children: vec![],
            },
        ];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        // Close ticket 11 by telling the syncer only "10" is still open
        syncer
            .close_missing_tickets("r1", "github", &["10"])
            .unwrap();
        AppState {
            db: Arc::new(Mutex::new(conn)),
            config: Arc::new(RwLock::new(Config::default())),
            events: EventBus::new(1),
            db_path: std::path::PathBuf::new(),
            workflow_done_notify: None,
        }
    }

    async fn get_json(uri: &str, state: AppState) -> (StatusCode, Vec<serde_json::Value>) {
        let app = api_router().with_state(state);
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    #[tokio::test]
    async fn list_all_tickets_hides_closed_by_default() {
        let (status, tickets) = get_json("/api/tickets", seeded_state()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(tickets.len(), 1, "closed ticket must be hidden by default");
        assert_eq!(tickets[0]["state"], "open");
    }

    #[tokio::test]
    async fn list_all_tickets_shows_closed_when_requested() {
        let (status, tickets) = get_json("/api/tickets?show_closed=true", seeded_state()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            tickets.len(),
            2,
            "show_closed=true must include closed tickets"
        );
    }

    #[tokio::test]
    async fn list_repo_tickets_hides_closed_by_default() {
        let (status, tickets) = get_json("/api/repos/r1/tickets", seeded_state()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(tickets.len(), 1, "closed ticket must be hidden by default");
        assert_eq!(tickets[0]["state"], "open");
    }

    #[tokio::test]
    async fn list_repo_tickets_shows_closed_when_requested() {
        let (status, tickets) =
            get_json("/api/repos/r1/tickets?show_closed=true", seeded_state()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            tickets.len(),
            2,
            "show_closed=true must include closed tickets"
        );
    }
}
