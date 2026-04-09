use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::notification_manager::{Notification, NotificationManager};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Deserialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct ListNotificationsQuery {
    pub unread_only: Option<bool>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct UnreadCountResponse {
    pub count: usize,
}

#[utoipa::path(
    get,
    path = "/api/notifications",
    params(ListNotificationsQuery),
    responses(
        (status = 200, description = "List of notifications", body = Vec<Notification>),
    ),
    tag = "notifications",
)]
pub async fn list_notifications(
    State(state): State<AppState>,
    Query(query): Query<ListNotificationsQuery>,
) -> Result<Json<Vec<Notification>>, ApiError> {
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
        .map_err(|e| ApiError::Internal(e.to_string()))
}

#[utoipa::path(
    post,
    path = "/api/notifications/{id}/read",
    params(
        ("id" = String, Path, description = "Notification ID"),
    ),
    responses(
        (status = 204, description = "Notification marked as read"),
        (status = 404, description = "Notification not found"),
    ),
    tag = "notifications",
)]
pub async fn mark_read(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let db = state.db.lock().await;
    let mgr = NotificationManager::new(&db);
    match mgr.mark_read(&id) {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::NotFound(format!("notification not found: {id}"))),
        Err(e) => Err(ApiError::Internal(e.to_string())),
    }
}

#[utoipa::path(
    post,
    path = "/api/notifications/read",
    responses(
        (status = 204, description = "All notifications marked as read"),
    ),
    tag = "notifications",
)]
pub async fn mark_all_read(State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    let db = state.db.lock().await;
    let mgr = NotificationManager::new(&db);
    mgr.mark_all_read()
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| ApiError::Internal(e.to_string()))
}

#[utoipa::path(
    get,
    path = "/api/notifications/unread-count",
    responses(
        (status = 200, description = "Count of unread notifications", body = UnreadCountResponse),
    ),
    tag = "notifications",
)]
pub async fn unread_count(
    State(state): State<AppState>,
) -> Result<Json<UnreadCountResponse>, ApiError> {
    let db = state.db.lock().await;
    let mgr = NotificationManager::new(&db);
    mgr.unread_count()
        .map(|count| Json(UnreadCountResponse { count }))
        .map_err(|e| ApiError::Internal(e.to_string()))
}
