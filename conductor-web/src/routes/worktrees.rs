use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use conductor_core::repo::RepoManager;
use conductor_core::tickets::TicketSyncer;
use conductor_core::worktree::{Worktree, WorktreeManager};

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
pub struct CreatePrRequest {
    #[serde(default)]
    pub draft: bool,
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

pub async fn delete_worktree(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Worktree>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = WorktreeManager::new(&db, &config);
    let wt = mgr.delete_by_id(&id)?;
    if let Ok(repo) = conductor_core::repo::RepoManager::new(&db, &config).get_by_id(&wt.repo_id) {
        let fm = conductor_core::feature::FeatureManager::new(&db, &config);
        if let Err(e) = fm.auto_close_after_worktree_delete(&repo, &wt) {
            tracing::warn!(error = %e, "failed to auto-close orphaned feature");
        }
    }
    state.events.emit(ConductorEvent::WorktreeDeleted {
        id: wt.id.clone(),
        repo_id: wt.repo_id.clone(),
    });
    Ok(Json(wt))
}

pub async fn push_worktree(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = WorktreeManager::new(&db, &config);
    let wt = mgr.get_by_id(&id)?;
    let repo = RepoManager::new(&db, &config).get_by_id(&wt.repo_id)?;
    let message = mgr.push(&repo.slug, &wt.slug)?;
    Ok(Json(serde_json::json!({ "message": message })))
}

pub async fn create_pr(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CreatePrRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = WorktreeManager::new(&db, &config);
    let wt = mgr.get_by_id(&id)?;
    let repo = RepoManager::new(&db, &config).get_by_id(&wt.repo_id)?;
    let url = mgr.create_pr(&repo.slug, &wt.slug, body.draft)?;
    Ok(Json(serde_json::json!({ "url": url })))
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
