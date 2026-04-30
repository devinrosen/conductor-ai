use axum::extract::State;
use axum::Json;

use conductor_core::stats::{StatsManager, ThemeUnlockStats};

use crate::error::ApiError;
use crate::state::AppState;

/// GET /api/stats/theme-unlocks
///
/// Returns aggregated stats used to evaluate theme unlock conditions.
#[utoipa::path(
    get,
    path = "/api/stats/theme-unlocks",
    responses(
        (status = 200, description = "Theme unlock stats", body = ThemeUnlockStats),
    ),
    tag = "stats",
)]
pub async fn theme_unlock_stats(
    State(state): State<AppState>,
) -> Result<Json<ThemeUnlockStats>, ApiError> {
    let db = state.db.lock().await;
    let stats = StatsManager::new(&db).theme_unlock_stats()?;
    Ok(Json(stats))
}
