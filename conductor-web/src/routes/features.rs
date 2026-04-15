use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use conductor_core::db::open_database;
use conductor_core::feature::{Feature, FeatureManager, FeatureRow, FeatureStatus, RunSummary, SyncResult};
use conductor_core::repo::RepoManager;
use conductor_core::tickets::Ticket;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize, utoipa::ToSchema)]
pub struct FeaturesResponse {
    pub features: Vec<FeatureRow>,
    pub stale_feature_days: u32,
}

// FeatureDetailResponse contains Feature and Ticket which do not derive utoipa::ToSchema
// in the current conductor-core; omit ToSchema and skip body annotation.
#[derive(Serialize)]
pub struct FeatureDetailResponse {
    pub feature: Feature,
    pub tickets: Vec<Ticket>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct FeatureSyncResponse {
    pub added: usize,
    pub removed: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct FeatureRunResponse {
    pub dispatched: u32,
    pub failed: u32,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct FeatureStatusResponse {
    pub status: String,
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/features",
    params(
        ("id" = String, Path, description = "Repo ID"),
    ),
    responses(
        (status = 200, description = "List of features for the repo", body = FeaturesResponse),
        (status = 404, description = "Repo not found"),
    ),
    tag = "features",
)]
pub async fn list_features(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<FeaturesResponse>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;

    let repo = RepoManager::new(&db, &config).get_by_id(&repo_id)?;

    let mgr = FeatureManager::new(&db, &config);
    let features = mgr.list(&repo.slug)?;

    Ok(Json(FeaturesResponse {
        features,
        stale_feature_days: config.defaults.stale_feature_days,
    }))
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/features/{name}",
    params(
        ("id" = String, Path, description = "Repo ID"),
        ("name" = String, Path, description = "Feature name"),
    ),
    responses(
        (status = 200, description = "Feature detail with linked tickets"),
        (status = 404, description = "Repo or feature not found"),
    ),
    tag = "features",
)]
pub async fn get_feature(
    State(state): State<AppState>,
    Path((repo_id, name)): Path<(String, String)>,
) -> Result<Json<FeatureDetailResponse>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;

    let repo = RepoManager::new(&db, &config).get_by_id(&repo_id)?;
    let mgr = FeatureManager::new(&db, &config);
    let feature = mgr.get_by_name(&repo.slug, &name)?;
    let tickets = mgr.linked_tickets(&feature.id)?;

    Ok(Json(FeatureDetailResponse { feature, tickets }))
}

#[utoipa::path(
    post,
    path = "/api/repos/{id}/features/{name}/sync",
    params(
        ("id" = String, Path, description = "Repo ID"),
        ("name" = String, Path, description = "Feature name"),
    ),
    responses(
        (status = 200, description = "Sync result", body = FeatureSyncResponse),
        (status = 404, description = "Repo or feature not found"),
        (status = 400, description = "Feature has no milestone source"),
    ),
    tag = "features",
)]
pub async fn sync_feature(
    State(state): State<AppState>,
    Path((repo_id, name)): Path<(String, String)>,
) -> Result<Json<FeatureSyncResponse>, ApiError> {
    let repo_slug = {
        let db = state.db.lock().await;
        let config = state.config.read().await;
        RepoManager::new(&db, &config).get_by_id(&repo_id)?.slug
    };

    let db_path = state.db_path.clone();
    let config = state.config.read().await.clone();

    let result: SyncResult = tokio::task::spawn_blocking(move || {
        let conn = open_database(&db_path)?;
        FeatureManager::new(&conn, &config).sync_from_milestone(&repo_slug, &name)
    })
    .await??;

    Ok(Json(FeatureSyncResponse {
        added: result.added,
        removed: result.removed,
    }))
}

#[utoipa::path(
    post,
    path = "/api/repos/{id}/features/{name}/run",
    params(
        ("id" = String, Path, description = "Repo ID"),
        ("name" = String, Path, description = "Feature name"),
    ),
    responses(
        (status = 200, description = "Run result with dispatched/failed counts", body = FeatureRunResponse),
        (status = 404, description = "Repo or feature not found"),
    ),
    tag = "features",
)]
pub async fn run_feature(
    State(state): State<AppState>,
    Path((repo_id, name)): Path<(String, String)>,
) -> Result<Json<FeatureRunResponse>, ApiError> {
    let repo_slug = {
        let db = state.db.lock().await;
        let config = state.config.read().await;
        RepoManager::new(&db, &config).get_by_id(&repo_id)?.slug
    };

    let db_path = state.db_path.clone();
    let config = state.config.read().await.clone();

    let result: RunSummary = tokio::task::spawn_blocking(move || {
        let conn = open_database(&db_path)?;
        FeatureManager::new(&conn, &config).run(&repo_slug, &name, None)
    })
    .await??;

    Ok(Json(FeatureRunResponse {
        dispatched: result.dispatched,
        failed: result.failed,
    }))
}

#[utoipa::path(
    post,
    path = "/api/repos/{id}/features/{name}/review",
    params(
        ("id" = String, Path, description = "Repo ID"),
        ("name" = String, Path, description = "Feature name"),
    ),
    responses(
        (status = 200, description = "Feature transitioned to ready_for_review"),
        (status = 404, description = "Repo or feature not found"),
        (status = 400, description = "Invalid state transition"),
    ),
    tag = "features",
)]
pub async fn review_feature(
    State(state): State<AppState>,
    Path((repo_id, name)): Path<(String, String)>,
) -> Result<Json<Feature>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;

    let repo = RepoManager::new(&db, &config).get_by_id(&repo_id)?;
    let feature = FeatureManager::new(&db, &config)
        .transition(&repo.slug, &name, FeatureStatus::ReadyForReview)?;

    Ok(Json(feature))
}

#[utoipa::path(
    post,
    path = "/api/repos/{id}/features/{name}/approve",
    params(
        ("id" = String, Path, description = "Repo ID"),
        ("name" = String, Path, description = "Feature name"),
    ),
    responses(
        (status = 200, description = "Feature transitioned to approved"),
        (status = 404, description = "Repo or feature not found"),
        (status = 400, description = "Invalid state transition"),
    ),
    tag = "features",
)]
pub async fn approve_feature(
    State(state): State<AppState>,
    Path((repo_id, name)): Path<(String, String)>,
) -> Result<Json<Feature>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;

    let repo = RepoManager::new(&db, &config).get_by_id(&repo_id)?;
    let feature = FeatureManager::new(&db, &config)
        .transition(&repo.slug, &name, FeatureStatus::Approved)?;

    Ok(Json(feature))
}

#[utoipa::path(
    post,
    path = "/api/repos/{id}/features/{name}/close",
    params(
        ("id" = String, Path, description = "Repo ID"),
        ("name" = String, Path, description = "Feature name"),
    ),
    responses(
        (status = 200, description = "Feature closed", body = FeatureStatusResponse),
        (status = 404, description = "Repo or feature not found"),
    ),
    tag = "features",
)]
pub async fn close_feature(
    State(state): State<AppState>,
    Path((repo_id, name)): Path<(String, String)>,
) -> Result<Json<FeatureStatusResponse>, ApiError> {
    let repo_slug = {
        let db = state.db.lock().await;
        let config_guard = state.config.read().await;
        RepoManager::new(&db, &config_guard).get_by_id(&repo_id)?.slug
    };

    let db_path = state.db_path.clone();
    let config = state.config.read().await.clone();

    tokio::task::spawn_blocking(move || {
        let conn = open_database(&db_path)?;
        FeatureManager::new(&conn, &config).close(&repo_slug, &name)
    })
    .await??;

    Ok(Json(FeatureStatusResponse {
        status: "closed".to_string(),
    }))
}
