use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use conductor_core::feature::{FeatureManager, FeatureRow};
use conductor_core::repo::RepoManager;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
pub struct FeaturesResponse {
    pub features: Vec<FeatureRow>,
    pub stale_feature_days: u32,
}

pub async fn list_features(
    State(state): State<AppState>,
    Path(repo_slug): Path<String>,
) -> Result<Json<FeaturesResponse>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;

    // Verify repo exists
    RepoManager::new(&db, &config).get_by_slug(&repo_slug)?;

    let mgr = FeatureManager::new(&db, &config);
    let features = mgr.list(&repo_slug)?;

    Ok(Json(FeaturesResponse {
        features,
        stale_feature_days: config.defaults.stale_feature_days,
    }))
}
