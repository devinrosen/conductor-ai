use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::config::Config;
use conductor_core::db::open_database;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::TicketSyncer;
use conductor_core::worktree::{Worktree, WorktreeManager, WorktreeWithStatus};

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

/// Open a fresh SQLite connection inside a `spawn_blocking` closure.
/// Reduces boilerplate shared by all three worktree mutation handlers.
fn open_db_and_config(
    db_path: &std::path::Path,
    config: Config,
) -> conductor_core::error::Result<(rusqlite::Connection, Config)> {
    let conn = open_database(db_path)?;
    Ok((conn, config))
}

#[derive(Deserialize)]
pub struct CreateWorktreeRequest {
    pub name: String,
    pub from_branch: Option<String>,
    pub ticket_id: Option<String>,
    /// When `true`, proceed even if the base branch has uncommitted changes.
    pub force: Option<bool>,
}

/// Structured body returned as HTTP 409 when the base branch is dirty or stale
/// and `force` is not set.
#[derive(Serialize)]
pub struct MainDirtyConflict {
    pub code: &'static str,
    pub message: &'static str,
    pub dirty_files: Vec<String>,
    pub commits_behind: u32,
}

/// Typed success body returned as HTTP 201 when a worktree is created.
/// Extends the core `Worktree` fields with optional runtime metadata.
#[derive(Serialize)]
pub struct CreateWorktreeResponse {
    #[serde(flatten)]
    pub worktree: Worktree,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub commits_behind: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
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
) -> Result<Json<Vec<WorktreeWithStatus>>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    // Verify repo exists
    RepoManager::new(&db, &config).get_by_id(&repo_id)?;
    let mgr = WorktreeManager::new(&db, &config);
    let active_only = !params.show_completed;
    let worktrees = mgr.list_by_repo_id_enriched(&repo_id, active_only)?;
    Ok(Json(worktrees))
}

pub async fn create_worktree(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    Json(body): Json<CreateWorktreeRequest>,
) -> Result<(StatusCode, Json<CreateWorktreeResponse>), ApiError> {
    // Look up repo slug quickly before spawning the blocking work.
    let repo_slug = {
        let db = state.db.lock().await;
        let config = state.config.read().await;
        RepoManager::new(&db, &config).get_by_id(&repo_id)?.slug
    };
    let db_path = state.db_path.clone();
    let config = state.config.read().await.clone();
    let name = body.name.clone();
    let from_branch = body.from_branch.clone();
    let ticket_id = body.ticket_id.clone();
    let force = body.force.unwrap_or(false);

    // Run health check off-thread before creating the worktree.
    let health_result = {
        let db_path2 = db_path.clone();
        let config2 = config.clone();
        let repo_slug2 = repo_slug.clone();
        let from_branch2 = from_branch.clone();
        tokio::task::spawn_blocking(move || {
            let (conn, config) = open_db_and_config(&db_path2, config2)?;
            WorktreeManager::new(&conn, &config)
                .check_main_health(&repo_slug2, from_branch2.as_deref())
        })
        .await??
    };

    // If dirty and not force: return 409 with structured body.
    if health_result.is_dirty && !force {
        let conflict_body = serde_json::to_value(MainDirtyConflict {
            code: "main_dirty",
            message: "base branch has uncommitted changes; pass force=true to proceed anyway",
            dirty_files: health_result.dirty_files,
            commits_behind: health_result.commits_behind,
        })
        .map_err(|e| ApiError::Internal(e.to_string()))?;
        return Err(ApiError::Conflict(conflict_body));
    }

    let commits_behind = health_result.commits_behind;
    let (wt, warnings) = tokio::task::spawn_blocking(move || {
        let (conn, config) = open_db_and_config(&db_path, config)?;
        WorktreeManager::new(&conn, &config).create(
            &repo_slug,
            &name,
            from_branch.as_deref(),
            ticket_id.as_deref(),
            None,
            force,
            Some(&health_result),
        )
    })
    .await??;

    state.events.emit(ConductorEvent::WorktreeCreated {
        id: wt.id.clone(),
        repo_id: wt.repo_id.clone(),
    });

    Ok((
        StatusCode::CREATED,
        Json(CreateWorktreeResponse {
            worktree: wt,
            warnings,
            commits_behind,
        }),
    ))
}

pub async fn get_worktree(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<WorktreeWithStatus>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = WorktreeManager::new(&db, &config);
    let wt = mgr.get_by_id_enriched(&id)?;
    Ok(Json(wt))
}

pub async fn delete_worktree(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Worktree>, ApiError> {
    let db_path = state.db_path.clone();
    let config = state.config.read().await.clone();
    let wt = tokio::task::spawn_blocking(move || {
        let (conn, config) = open_db_and_config(&db_path, config)?;
        WorktreeManager::new(&conn, &config).delete_by_id(&id)
    })
    .await??;
    state.events.emit(ConductorEvent::WorktreeDeleted {
        id: wt.id.clone(),
        repo_id: wt.repo_id.clone(),
    });
    Ok(Json(wt))
}

pub async fn get_worktree_for_repo(
    State(state): State<AppState>,
    Path((repo_id, id)): Path<(String, String)>,
) -> Result<Json<WorktreeWithStatus>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = WorktreeManager::new(&db, &config);
    let wt = mgr.get_by_id_for_repo_enriched(&id, &repo_id)?;
    Ok(Json(wt))
}

pub async fn delete_worktree_for_repo(
    State(state): State<AppState>,
    Path((repo_id, id)): Path<(String, String)>,
) -> Result<Json<Worktree>, ApiError> {
    let db_path = state.db_path.clone();
    let config = state.config.read().await.clone();
    let wt = tokio::task::spawn_blocking(move || {
        let (conn, config) = open_db_and_config(&db_path, config)?;
        WorktreeManager::new(&conn, &config).delete_by_id_for_repo(&id, &repo_id)
    })
    .await??;
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

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::routes::api_router;
    use crate::test_helpers::seeded_state;

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
        let (state, _tmp) = seeded_state();
        let (status, body) = send_get("/api/worktrees/w1", state).await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], "w1");
        assert_eq!(json["repo_id"], "r1");
        assert_eq!(json["slug"], "feat-test");
    }

    #[tokio::test]
    async fn get_worktree_returns_404_when_not_found() {
        let (state, _tmp) = seeded_state();
        let (status, _) = send_get("/api/worktrees/nonexistent", state).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_worktree_for_repo_returns_200_with_matching_repo() {
        let (state, _tmp) = seeded_state();
        let (status, body) = send_get("/api/repos/r1/worktrees/w1", state).await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], "w1");
        assert_eq!(json["repo_id"], "r1");
    }

    #[tokio::test]
    async fn get_worktree_for_repo_returns_404_for_mismatched_repo() {
        let (state, _tmp) = seeded_state();
        // Insert a second repo so the route can be exercised
        {
            let db = state.db.lock().await;
            conductor_core::test_helpers::insert_test_repo(&db, "r2", "other-repo", "/tmp/repo2");
        }
        // w1 belongs to r1 — requesting it under r2 must return 404
        let (status, _) = send_get("/api/repos/r2/worktrees/w1", state).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_worktree_for_repo_returns_404_when_not_found() {
        let (state, _tmp) = seeded_state();
        let (status, _) = send_get("/api/repos/r1/worktrees/nonexistent", state).await;
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
        let (state, _tmp) = seeded_state();
        let (status, body) = send_delete("/api/repos/r1/worktrees/w1", state).await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], "w1");
    }

    #[tokio::test]
    async fn delete_worktree_for_repo_returns_404_for_mismatched_repo() {
        let (state, _tmp) = seeded_state();
        {
            let db = state.db.lock().await;
            conductor_core::test_helpers::insert_test_repo(&db, "r2", "other-repo", "/tmp/repo2");
        }
        // w1 belongs to r1 — deleting it under r2 must return 404
        let (status, _) = send_delete("/api/repos/r2/worktrees/w1", state).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_worktree_for_repo_returns_404_when_not_found() {
        let (state, _tmp) = seeded_state();
        let (status, _) = send_delete("/api/repos/r1/worktrees/nonexistent", state).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_worktree_returns_200() {
        let (state, _tmp) = seeded_state();
        let (status, body) = send_delete("/api/worktrees/w1", state).await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], "w1");
    }

    #[tokio::test]
    async fn delete_worktree_returns_404_when_not_found() {
        let (state, _tmp) = seeded_state();
        let (status, _) = send_delete("/api/worktrees/nonexistent", state).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    async fn send_post(uri: &str, body: &str, state: AppState) -> (StatusCode, Vec<u8>) {
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, bytes)
    }

    /// Verify that the spawn_blocking path in create_worktree propagates git errors correctly.
    /// /tmp/repo is not a real git repo, so WorktreeManager::create will fail at the git layer,
    /// exercising the full spawn_blocking → error-propagation → ApiError path.
    #[tokio::test]
    async fn create_worktree_propagates_error_when_git_fails() {
        let (state, _tmp) = seeded_state();
        let (status, _) = send_post(
            "/api/repos/r1/worktrees",
            r#"{"name":"new-feature"}"#,
            state,
        )
        .await;
        // Git will fail since /tmp/repo is not a git repo — expect 500, not 201 and not a panic.
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn create_worktree_returns_404_for_nonexistent_repo() {
        let (state, _tmp) = seeded_state();
        let (status, _) = send_post(
            "/api/repos/no-such-repo/worktrees",
            r#"{"name":"new-feature"}"#,
            state,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
