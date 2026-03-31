use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use conductor_core::repo::RepoManager;
use conductor_core::tickets::TicketSyncer;
use conductor_core::worktree::{Worktree, WorktreeManager, WorktreeWithStatus};

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateWorktreeRequest {
    pub name: String,
    pub from_branch: Option<String>,
    pub ticket_id: Option<String>,
}

#[derive(Deserialize)]
pub struct LinkTicketRequest {
    pub ticket_id: String,
}

#[derive(Debug, Deserialize)]
pub struct WorktreeListQuery {
    /// When true, include merged/abandoned worktrees. Defaults to false (completed worktrees hidden).
    #[serde(default)]
    pub show_completed: bool,
}

pub async fn list_all_worktrees(
    State(state): State<AppState>,
    Query(params): Query<WorktreeListQuery>,
) -> Result<Json<Vec<WorktreeWithStatus>>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = WorktreeManager::new(&db, &config);
    let active_only = !params.show_completed;
    let worktrees = mgr.list_all_with_status(active_only)?;
    Ok(Json(worktrees))
}

pub async fn list_worktrees(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    Query(params): Query<WorktreeListQuery>,
) -> Result<Json<Vec<Worktree>>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    // Verify repo exists
    RepoManager::new(&db, &config).get_by_id(&repo_id)?;
    let mgr = WorktreeManager::new(&db, &config);
    let active_only = !params.show_completed;
    let worktrees = mgr.list_by_repo_id(&repo_id, active_only)?;
    Ok(Json(worktrees))
}

pub async fn create_worktree(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    Json(body): Json<CreateWorktreeRequest>,
) -> Result<(StatusCode, Json<Worktree>), ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let repo = RepoManager::new(&db, &config).get_by_id(&repo_id)?;
    let mgr = WorktreeManager::new(&db, &config);
    let (wt, _warnings) = mgr.create(
        &repo.slug,
        &body.name,
        body.from_branch.as_deref(),
        body.ticket_id.as_deref(),
        None,
    )?;
    state.events.emit(ConductorEvent::WorktreeCreated {
        id: wt.id.clone(),
        repo_id: wt.repo_id.clone(),
    });
    Ok((StatusCode::CREATED, Json(wt)))
}

pub async fn get_worktree(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Worktree>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = WorktreeManager::new(&db, &config);
    let wt = mgr.get_by_id(&id)?;
    Ok(Json(wt))
}

pub async fn delete_worktree(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Worktree>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = WorktreeManager::new(&db, &config);
    let wt = mgr.delete_by_id(&id)?;
    state.events.emit(ConductorEvent::WorktreeDeleted {
        id: wt.id.clone(),
        repo_id: wt.repo_id.clone(),
    });
    Ok(Json(wt))
}

pub async fn get_worktree_for_repo(
    State(state): State<AppState>,
    Path((repo_id, id)): Path<(String, String)>,
) -> Result<Json<Worktree>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = WorktreeManager::new(&db, &config);
    let wt = mgr.get_by_id_for_repo(&id, &repo_id)?;
    Ok(Json(wt))
}

pub async fn delete_worktree_for_repo(
    State(state): State<AppState>,
    Path((repo_id, id)): Path<(String, String)>,
) -> Result<Json<Worktree>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = WorktreeManager::new(&db, &config);
    let wt = mgr.delete_by_id_for_repo(&id, &repo_id)?;
    state.events.emit(ConductorEvent::WorktreeDeleted {
        id: wt.id.clone(),
        repo_id: wt.repo_id.clone(),
    });
    Ok(Json(wt))
}

#[derive(Deserialize)]
pub struct SetModelRequest {
    pub model: Option<String>,
}

pub async fn patch_worktree_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SetModelRequest>,
) -> Result<Json<Worktree>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = WorktreeManager::new(&db, &config);
    let wt = mgr.get_by_id(&id)?;
    let repo = RepoManager::new(&db, &config).get_by_id(&wt.repo_id)?;
    mgr.set_model(&repo.slug, &wt.slug, body.model.as_deref())?;
    let updated = mgr.get_by_id(&id)?;
    Ok(Json(updated))
}

pub async fn link_ticket(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<LinkTicketRequest>,
) -> Result<Json<Worktree>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    // Verify worktree exists and has no linked ticket
    let mgr = WorktreeManager::new(&db, &config);
    let wt = mgr.get_by_id(&id)?;
    if wt.ticket_id.is_some() {
        return Err(conductor_core::error::ConductorError::TicketAlreadyLinked.into());
    }
    // Verify ticket exists
    let syncer = TicketSyncer::new(&db);
    syncer.get_by_id(&body.ticket_id)?;
    // Link ticket to worktree
    syncer.link_to_worktree(&body.ticket_id, &id)?;
    // Return updated worktree
    let updated = mgr.get_by_id(&id)?;
    Ok(Json(updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use conductor_core::config::Config;
    use tokio::sync::{Mutex, RwLock};
    use tower::ServiceExt;

    use crate::events::EventBus;
    use crate::routes::api_router;

    fn seeded_state() -> AppState {
        // setup_db provides: repo r1 (slug test-repo), worktree w1 (feat-test, active)
        let conn = conductor_core::test_helpers::setup_db();
        AppState {
            db: Arc::new(Mutex::new(conn)),
            config: Arc::new(RwLock::new(Config::default())),
            events: EventBus::new(1),
            workflow_done_notify: None,
        }
    }

    async fn send_get(uri: &str, state: AppState) -> (StatusCode, Vec<u8>) {
        let app = api_router().with_state(state);
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, body)
    }

    #[tokio::test]
    async fn get_worktree_returns_200_with_worktree() {
        let (status, body) = send_get("/api/worktrees/w1", seeded_state()).await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], "w1");
        assert_eq!(json["repo_id"], "r1");
        assert_eq!(json["slug"], "feat-test");
    }

    #[tokio::test]
    async fn get_worktree_returns_404_when_not_found() {
        let (status, _) = send_get("/api/worktrees/nonexistent", seeded_state()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_worktree_for_repo_returns_200_with_matching_repo() {
        let (status, body) =
            send_get("/api/repos/r1/worktrees/w1", seeded_state()).await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], "w1");
        assert_eq!(json["repo_id"], "r1");
    }

    #[tokio::test]
    async fn get_worktree_for_repo_returns_404_for_mismatched_repo() {
        let state = seeded_state();
        // Insert a second repo so the route can be exercised
        {
            let db = state.db.lock().await;
            db.execute(
                "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
                 VALUES ('r2', 'other-repo', '/tmp/repo2', 'https://github.com/test/repo2.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        }
        // w1 belongs to r1 — requesting it under r2 must return 404
        let (status, _) = send_get("/api/repos/r2/worktrees/w1", state).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_worktree_for_repo_returns_404_when_not_found() {
        let (status, _) =
            send_get("/api/repos/r1/worktrees/nonexistent", seeded_state()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    async fn send_delete(uri: &str, state: AppState) -> (StatusCode, Vec<u8>) {
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, body)
    }

    #[tokio::test]
    async fn delete_worktree_for_repo_returns_200_with_matching_repo() {
        let (status, body) =
            send_delete("/api/repos/r1/worktrees/w1", seeded_state()).await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], "w1");
    }

    #[tokio::test]
    async fn delete_worktree_for_repo_returns_404_for_mismatched_repo() {
        let state = seeded_state();
        {
            let db = state.db.lock().await;
            db.execute(
                "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
                 VALUES ('r2', 'other-repo', '/tmp/repo2', 'https://github.com/test/repo2.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        }
        // w1 belongs to r1 — deleting it under r2 must return 404
        let (status, _) = send_delete("/api/repos/r2/worktrees/w1", state).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_worktree_for_repo_returns_404_when_not_found() {
        let (status, _) =
            send_delete("/api/repos/r1/worktrees/nonexistent", seeded_state()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
