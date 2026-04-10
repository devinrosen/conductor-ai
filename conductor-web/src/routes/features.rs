use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use conductor_core::feature::{FeatureManager, FeatureRow};
use conductor_core::repo::RepoManager;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize, utoipa::ToSchema)]
pub struct FeaturesResponse {
    pub features: Vec<FeatureRow>,
    pub stale_feature_days: u32,
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
