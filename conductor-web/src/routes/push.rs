use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::push::PushSubscriptionManager;
use crate::state::AppState;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PushSubscribeRequest {
    pub endpoint: String,
    pub keys: PushSubscriptionKeys,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PushSubscriptionKeys {
    pub p256dh: String,
    pub auth: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct VapidPublicKeyResponse {
    pub public_key: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PushSubscribeResponse {
    pub success: bool,
    pub message: String,
}

/// GET /api/push/vapid-public-key
/// Returns the VAPID public key for push subscription
#[utoipa::path(
    get,
    path = "/api/push/vapid-public-key",
    responses(
        (status = 200, description = "VAPID public key", body = VapidPublicKeyResponse),
        (status = 503, description = "Push notifications not configured"),
    ),
    tag = "push",
)]
pub async fn get_vapid_public_key(
    State(state): State<AppState>,
) -> Result<Json<VapidPublicKeyResponse>, ApiError> {
    let config = state.config.read().await;

    match &config.web_push.vapid_public_key {
        Some(public_key) => Ok(Json(VapidPublicKeyResponse {
            public_key: public_key.clone(),
        })),
        None => Err(ApiError::ServiceUnavailable(
            "Push notifications not configured - VAPID keys not found".to_string(),
        )),
    }
}

/// POST /api/push/subscribe
/// Subscribe to push notifications
#[utoipa::path(
    post,
    path = "/api/push/subscribe",
    request_body(content = PushSubscribeRequest, description = "Push subscription details"),
    responses(
        (status = 200, description = "Successfully subscribed", body = PushSubscribeResponse),
        (status = 503, description = "Push notifications not configured"),
    ),
    tag = "push",
)]
pub async fn subscribe_push(
    State(state): State<AppState>,
    Json(request): Json<PushSubscribeRequest>,
) -> Result<Json<PushSubscribeResponse>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;

    // Verify we have VAPID keys configured
    let (vapid_private_key, vapid_public_key, vapid_subject) = match (
        &config.web_push.vapid_private_key,
        &config.web_push.vapid_public_key,
        &config.web_push.vapid_subject,
    ) {
        (Some(private_key), Some(public_key), Some(subject)) => {
            (private_key.clone(), public_key.clone(), subject.clone())
        }
        _ => {
            return Err(ApiError::ServiceUnavailable(
                "Push notifications not configured - VAPID keys not found".to_string(),
            ));
        }
    };

    let manager =
        PushSubscriptionManager::new(&db, vapid_private_key, vapid_public_key, vapid_subject);

    match manager.upsert_subscription(&request.endpoint, &request.keys.p256dh, &request.keys.auth) {
        Ok(_) => Ok(Json(PushSubscribeResponse {
            success: true,
            message: "Successfully subscribed to push notifications".to_string(),
        })),
        Err(e) => Err(ApiError::Internal(format!(
            "Failed to subscribe to push notifications: {}",
            e
        ))),
    }
}

/// DELETE /api/push/subscribe
/// Unsubscribe from push notifications
#[utoipa::path(
    delete,
    path = "/api/push/subscribe",
    request_body(content = PushSubscribeRequest, description = "Push subscription to remove"),
    responses(
        (status = 204, description = "Successfully unsubscribed"),
        (status = 404, description = "Subscription not found"),
        (status = 503, description = "Push notifications not configured"),
    ),
    tag = "push",
)]
pub async fn unsubscribe_push(
    State(state): State<AppState>,
    Json(request): Json<PushSubscribeRequest>,
) -> Result<StatusCode, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;

    // Verify we have VAPID keys configured
    let (vapid_private_key, vapid_public_key, vapid_subject) = match (
        &config.web_push.vapid_private_key,
        &config.web_push.vapid_public_key,
        &config.web_push.vapid_subject,
    ) {
        (Some(private_key), Some(public_key), Some(subject)) => {
            (private_key.clone(), public_key.clone(), subject.clone())
        }
        _ => {
            return Err(ApiError::ServiceUnavailable(
                "Push notifications not configured - VAPID keys not found".to_string(),
            ));
        }
    };

    let manager =
        PushSubscriptionManager::new(&db, vapid_private_key, vapid_public_key, vapid_subject);

    match manager.delete_subscription(&request.endpoint) {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::NotFound(
            "Push subscription not found".to_string(),
        )),
        Err(e) => Err(ApiError::Internal(format!(
            "Failed to unsubscribe from push notifications: {}",
            e
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use conductor_core::config::{Config, WebPushConfig};
    use tempfile::NamedTempFile;

    fn setup_test_state() -> (AppState, NamedTempFile) {
        let tmp = NamedTempFile::new().expect("create temp db file");
        let db = conductor_core::db::open_database(tmp.path()).expect("open temp db");
        let config = Config {
            web_push: WebPushConfig {
                vapid_public_key: Some("test_public_key".to_string()),
                vapid_private_key: Some("test_private_key".to_string()),
                vapid_subject: Some("mailto:test@example.com".to_string()),
            },
            ..Default::default()
        };
        let db_path = tmp.path().to_path_buf();
        (AppState::new(db, config, db_path, 100), tmp)
    }

    #[tokio::test]
    async fn test_get_vapid_public_key() {
        let (state, _tmp) = setup_test_state();

        let result = get_vapid_public_key(State(state)).await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.public_key, "test_public_key");
    }

    #[tokio::test]
    async fn test_subscribe_push() {
        let (state, _tmp) = setup_test_state();

        let request = PushSubscribeRequest {
            endpoint: "https://example.com/push".to_string(),
            keys: PushSubscriptionKeys {
                p256dh: "p256dh_key".to_string(),
                auth: "auth_secret".to_string(),
            },
        };

        let result = subscribe_push(State(state), Json(request)).await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(response.success);
    }

    #[tokio::test]
    async fn test_get_vapid_public_key_not_configured() {
        let tmp = NamedTempFile::new().expect("create temp db file");
        let db = conductor_core::db::open_database(tmp.path()).expect("open temp db");
        let db_path = tmp.path().to_path_buf();
        let config = Config::default(); // No VAPID keys configured
        let state = AppState::new(db, config, db_path, 100);

        let result = get_vapid_public_key(State(state)).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
