use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;

use conductor_core::merge_queue::{MergeQueueEntry, MergeQueueManager, QueueStats};
use conductor_core::worktree::WorktreeManager;

use conductor_core::error::ConductorError;

use crate::error::ApiError;
use crate::state::AppState;

/// List all merge queue entries for a repo.
pub async fn list_entries(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<Vec<MergeQueueEntry>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = MergeQueueManager::new(&db);
    let entries = mgr.list_for_repo(&repo_id)?;
    Ok(Json(entries))
}

/// List only pending (queued/processing) entries for a repo.
pub async fn list_pending(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<Vec<MergeQueueEntry>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = MergeQueueManager::new(&db);
    let entries = mgr.list_pending(&repo_id)?;
    Ok(Json(entries))
}

#[derive(Deserialize)]
pub struct EnqueueRequest {
    pub worktree_id: String,
    pub run_id: Option<String>,
    pub target_branch: Option<String>,
}

/// Add a worktree to the merge queue.
pub async fn enqueue(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    Json(body): Json<EnqueueRequest>,
) -> Result<Json<MergeQueueEntry>, ApiError> {
    let db = state.db.lock().await;
    let mgr = MergeQueueManager::new(&db);
    let entry = mgr.enqueue(
        &repo_id,
        &body.worktree_id,
        body.run_id.as_deref(),
        body.target_branch.as_deref(),
    )?;
    Ok(Json(entry))
}

/// Pop the next queued entry and mark it as processing.
pub async fn pop_next(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<Option<MergeQueueEntry>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = MergeQueueManager::new(&db);
    let entry = mgr.pop_next(&repo_id)?;
    Ok(Json(entry))
}

/// Mark an entry as merged.
pub async fn mark_merged(
    State(state): State<AppState>,
    Path(entry_id): Path<String>,
) -> Result<Json<()>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = MergeQueueManager::new(&db);
    let entry = mgr.get(&entry_id)?.ok_or_else(|| {
        ApiError(ConductorError::MergeQueueEntryNotFound {
            id: entry_id.clone(),
        })
    })?;
    mgr.mark_merged(&entry_id)?;
    let wt_mgr = WorktreeManager::new(&db, &config);
    if let Err(e) = wt_mgr.delete_by_id_as_merged(&entry.worktree_id) {
        tracing::warn!(
            "mark_merged: could not clean up worktree {}: {e}",
            entry.worktree_id
        );
    }
    Ok(Json(()))
}

/// Mark an entry as failed.
pub async fn mark_failed(
    State(state): State<AppState>,
    Path(entry_id): Path<String>,
) -> Result<Json<()>, ApiError> {
    let db = state.db.lock().await;
    let mgr = MergeQueueManager::new(&db);
    mgr.mark_failed(&entry_id)?;
    Ok(Json(()))
}

/// Remove an entry from the queue.
pub async fn remove_entry(
    State(state): State<AppState>,
    Path(entry_id): Path<String>,
) -> Result<Json<()>, ApiError> {
    let db = state.db.lock().await;
    let mgr = MergeQueueManager::new(&db);
    mgr.remove(&entry_id)?;
    Ok(Json(()))
}

/// Get queue statistics for a repo.
pub async fn queue_stats(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<QueueStats>, ApiError> {
    let db = state.db.lock().await;
    let mgr = MergeQueueManager::new(&db);
    let stats = mgr.queue_stats(&repo_id)?;
    Ok(Json(stats))
}
