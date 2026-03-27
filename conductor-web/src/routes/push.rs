use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::push::PushSubscriptionManager;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct PushSubscribeRequest {
    pub endpoint: String,
    pub keys: PushSubscriptionKeys,
}

#[derive(Debug, Deserialize)]
pub struct PushSubscriptionKeys {
    pub p256dh: String,
    pub auth: String,
}

#[derive(Debug, Serialize)]
pub struct VapidPublicKeyResponse {
    pub public_key: String,
}

#[derive(Debug, Serialize)]
pub struct PushSubscribeResponse {
    pub success: bool,
    pub message: String,
}

/// GET /api/push/vapid-public-key
/// Returns the VAPID public key for push subscription
pub async fn get_vapid_public_key(
    State(state): State<AppState>,
) -> Result<Json<VapidPublicKeyResponse>, (StatusCode, String)> {
    let config = state.config.read().await;

    match &config.web_push.vapid_public_key {
        Some(public_key) => Ok(Json(VapidPublicKeyResponse {
            public_key: public_key.clone(),
        })),
        None => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Push notifications not configured - VAPID keys not found".to_string(),
        )),
    }
}

/// POST /api/push/subscribe
/// Subscribe to push notifications
pub async fn subscribe_push(
    State(state): State<AppState>,
    Json(request): Json<PushSubscribeRequest>,
) -> Result<Json<PushSubscribeResponse>, (StatusCode, String)> {
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
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
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
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to subscribe to push notifications: {}", e),
        )),
    }
}

/// DELETE /api/push/subscribe
/// Unsubscribe from push notifications
pub async fn unsubscribe_push(
    State(state): State<AppState>,
    Json(request): Json<PushSubscribeRequest>,
) -> Result<Json<PushSubscribeResponse>, (StatusCode, String)> {
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
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "Push notifications not configured - VAPID keys not found".to_string(),
            ));
        }
    };

    let manager =
        PushSubscriptionManager::new(&db, vapid_private_key, vapid_public_key, vapid_subject);

    match manager.delete_subscription(&request.endpoint) {
        Ok(deleted) => {
            if deleted {
                Ok(Json(PushSubscribeResponse {
                    success: true,
                    message: "Successfully unsubscribed from push notifications".to_string(),
                }))
            } else {
                Err((
                    StatusCode::NOT_FOUND,
                    "Push subscription not found".to_string(),
                ))
            }
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to unsubscribe from push notifications: {}", e),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use axum::http::StatusCode;
    use conductor_core::{
        config::{Config, WebPushConfig},
        test_helpers::create_test_conn,
    };

    async fn setup_test_state() -> AppState {
        // create_test_conn() runs all migrations, including the one that creates
        // push_subscriptions (migration 052).
        let db = create_test_conn();

        let config = Config {
            web_push: WebPushConfig {
                vapid_public_key: Some("test_public_key".to_string()),
                vapid_private_key: Some("test_private_key".to_string()),
                vapid_subject: Some("mailto:test@example.com".to_string()),
            },
            ..Default::default()
        };

        AppState::new(db, config, 100)
    }

    #[tokio::test]
    async fn test_get_vapid_public_key() {
        let state = setup_test_state().await;

        let result = get_vapid_public_key(State(state)).await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.public_key, "test_public_key");
    }

    #[tokio::test]
    async fn test_subscribe_push() {
        let state = setup_test_state().await;

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
        let db = create_test_conn();
        let config = Config::default(); // No VAPID keys configured
        let state = AppState::new(db, config, 100);

        let result = get_vapid_public_key(State(state)).await;

        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
}
