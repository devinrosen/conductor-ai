use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::notification_manager::{Notification, NotificationManager};

use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListNotificationsQuery {
    pub unread_only: Option<bool>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Serialize)]
pub struct UnreadCountResponse {
    pub count: usize,
}

pub async fn list_notifications(
    State(state): State<AppState>,
    Query(query): Query<ListNotificationsQuery>,
) -> Result<Json<Vec<Notification>>, (StatusCode, String)> {
    let db = state.db.lock().await;
    let mgr = NotificationManager::new(&db);
    let notifications = if query.unread_only.unwrap_or(false) {
        mgr.list_unread()
    } else {
        let limit = query.limit.unwrap_or(50);
        let offset = query.offset.unwrap_or(0);
        mgr.list_recent(limit, offset)
    };
    notifications
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

pub async fn mark_read(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let db = state.db.lock().await;
    let mgr = NotificationManager::new(&db);
    mgr.mark_read(&id)
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

pub async fn mark_all_read(
    State(state): State<AppState>,
) -> Result<StatusCode, (StatusCode, String)> {
    let db = state.db.lock().await;
    let mgr = NotificationManager::new(&db);
    mgr.mark_all_read()
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

pub async fn unread_count(
    State(state): State<AppState>,
) -> Result<Json<UnreadCountResponse>, (StatusCode, String)> {
    let db = state.db.lock().await;
    let mgr = NotificationManager::new(&db);
    mgr.unread_count()
        .map(|count| Json(UnreadCountResponse { count }))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}
