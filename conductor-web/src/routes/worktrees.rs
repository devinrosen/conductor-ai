use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use conductor_core::repo::RepoManager;
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

pub async fn list_worktrees(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<Vec<Worktree>>, ApiError> {
    let db = state.db.lock().await;
    // Verify repo exists
    RepoManager::new(&db, &state.config).get_by_id(&repo_id)?;
    let mgr = WorktreeManager::new(&db, &state.config);
    let worktrees = mgr.list_by_repo_id(&repo_id, false)?;
    Ok(Json(worktrees))
}

pub async fn create_worktree(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    Json(body): Json<CreateWorktreeRequest>,
) -> Result<(StatusCode, Json<Worktree>), ApiError> {
    let db = state.db.lock().await;
    let repo = RepoManager::new(&db, &state.config).get_by_id(&repo_id)?;
    let mgr = WorktreeManager::new(&db, &state.config);
    let (wt, _warnings) = mgr.create(
        &repo.slug,
        &body.name,
        body.from_branch.as_deref(),
        body.ticket_id.as_deref(),
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
    let mgr = WorktreeManager::new(&db, &state.config);
    let wt = mgr.delete_by_id(&id)?;
    state.events.emit(ConductorEvent::WorktreeDeleted {
        id: wt.id.clone(),
        repo_id: wt.repo_id.clone(),
    });
    Ok(Json(wt))
}
