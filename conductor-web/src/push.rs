use serde::{Deserialize, Serialize};
use tracing::info;

use conductor_core::error::Result;
pub use conductor_core::push::{
    delete_subscription, get_all_subscriptions, upsert_subscription, PushSubscription,
};

use crate::config::WebPushConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushPayload {
    pub title: String,
    pub body: String,
    pub tag: Option<String>,
    pub url: Option<String>,
}

pub async fn send_all(
    subscriptions: Vec<PushSubscription>,
    vapid: &WebPushConfig,
    payload: &PushPayload,
) -> Result<Vec<String>> {
    let (private_key, subject) = match (&vapid.vapid_private_key, &vapid.vapid_subject) {
        (Some(pk), Some(s)) => (pk.clone(), s.clone()),
        _ => return Ok(Vec::new()),
    };

    if subscriptions.is_empty() {
        info!("No push subscriptions found, skipping push notification");
        return Ok(Vec::new());
    }

    info!(
        "Sending push notification to {} subscription(s)",
        subscriptions.len()
    );

    let payload_bytes = serde_json::to_vec(payload)
        .map_err(|e| conductor_core::error::ConductorError::Agent(e.to_string()))?;

    let mut expired_endpoints = Vec::new();

    for subscription in &subscriptions {
        match send_to_subscription(&private_key, &subject, subscription, &payload_bytes).await {
            Ok(()) => {}
            Err(web_push::WebPushError::EndpointNotValid)
            | Err(web_push::WebPushError::EndpointNotFound) => {
                tracing::info!(
                    "Push subscription expired (410/404), removing: {}",
                    subscription.endpoint
                );
                expired_endpoints.push(subscription.endpoint.clone());
            }
            Err(e) => {
                tracing::warn!("Push send failed for {}: {e}", subscription.endpoint);
            }
        }
    }

    Ok(expired_endpoints)
}

async fn send_to_subscription(
    vapid_private_key: &str,
    vapid_subject: &str,
    sub: &PushSubscription,
    payload_json: &[u8],
) -> std::result::Result<(), web_push::WebPushError> {
    use web_push::{
        ContentEncoding, IsahcWebPushClient, SubscriptionInfo, VapidSignatureBuilder,
        WebPushClient, WebPushMessageBuilder, URL_SAFE_NO_PAD,
    };

    let sub_info = SubscriptionInfo::new(&sub.endpoint, &sub.p256dh, &sub.auth);
    let mut sig_builder =
        VapidSignatureBuilder::from_base64(vapid_private_key, URL_SAFE_NO_PAD, &sub_info)?;
    sig_builder.add_claim("sub", vapid_subject);
    let signature = sig_builder.build()?;

    let mut builder = WebPushMessageBuilder::new(&sub_info);
    builder.set_payload(ContentEncoding::Aes128Gcm, payload_json);
    builder.set_vapid_signature(signature);

    let client = IsahcWebPushClient::new()?;
    client.send(builder.build()?).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WebPushConfig;

    #[tokio::test]
    async fn test_send_all_no_vapid_config() {
        let vapid = WebPushConfig {
            vapid_public_key: None,
            vapid_private_key: None,
            vapid_subject: None,
        };
        let payload = PushPayload {
            title: "Test".to_string(),
            body: "Body".to_string(),
            tag: None,
            url: None,
        };

        // Returns Ok without sending when VAPID keys are absent
        let result = send_all(Vec::new(), &vapid, &payload).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_send_all_no_subscriptions() {
        let vapid = WebPushConfig {
            vapid_public_key: Some("pub".to_string()),
            vapid_private_key: Some("priv".to_string()),
            vapid_subject: Some("mailto:test@example.com".to_string()),
        };
        let payload = PushPayload {
            title: "Test".to_string(),
            body: "Body".to_string(),
            tag: None,
            url: None,
        };

        // Returns Ok without error when subscription list is empty
        let result = send_all(Vec::new(), &vapid, &payload).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
