use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use conductor_core::session::{Session, SessionTracker};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct EndSessionRequest {
    pub notes: Option<String>,
}

pub async fn list_sessions(State(state): State<AppState>) -> Result<Json<Vec<Session>>, ApiError> {
    let db = state.db.lock().await;
    let tracker = SessionTracker::new(&db);
    let sessions = tracker.list()?;
    Ok(Json(sessions))
}

pub async fn start_session(
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<Session>), ApiError> {
    let db = state.db.lock().await;
    let tracker = SessionTracker::new(&db);
    let session = tracker.start()?;
    Ok((StatusCode::CREATED, Json(session)))
}

pub async fn end_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<EndSessionRequest>,
) -> Result<StatusCode, ApiError> {
    let db = state.db.lock().await;
    let tracker = SessionTracker::new(&db);
    tracker.end(&id, body.notes.as_deref())?;
    Ok(StatusCode::NO_CONTENT)
}
