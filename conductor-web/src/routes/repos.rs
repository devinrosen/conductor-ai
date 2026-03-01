use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use conductor_core::repo::{derive_local_path, derive_slug_from_url, Repo, RepoManager};

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateRepoRequest {
    pub remote_url: String,
    pub slug: Option<String>,
    pub local_path: Option<String>,
    pub workspace_dir: Option<String>,
}

pub async fn list_repos(State(state): State<AppState>) -> Result<Json<Vec<Repo>>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = RepoManager::new(&db, &config);
    let repos = mgr.list()?;
    Ok(Json(repos))
}

pub async fn create_repo(
    State(state): State<AppState>,
    Json(body): Json<CreateRepoRequest>,
) -> Result<(StatusCode, Json<Repo>), ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = RepoManager::new(&db, &config);
    let slug = body
        .slug
        .unwrap_or_else(|| derive_slug_from_url(&body.remote_url));
    let local_path = body
        .local_path
        .unwrap_or_else(|| derive_local_path(&config, &slug));
    let repo = mgr.add(
        &slug,
        &local_path,
        &body.remote_url,
        body.workspace_dir.as_deref(),
    )?;
    state.events.emit(ConductorEvent::RepoCreated {
        id: repo.id.clone(),
    });
    Ok((StatusCode::CREATED, Json(repo)))
}

pub async fn delete_repo(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = RepoManager::new(&db, &config);
    mgr.remove_by_id(&id)?;
    state.events.emit(ConductorEvent::RepoDeleted { id });
    Ok(StatusCode::NO_CONTENT)
}
