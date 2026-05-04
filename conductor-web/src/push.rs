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
    subscriptions: &[PushSubscription],
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

    for subscription in subscriptions {
        let result =
            send_to_subscription(&private_key, &subject, subscription, &payload_bytes).await;
        process_send_result(result, &subscription.endpoint, &mut expired_endpoints);
    }

    Ok(expired_endpoints)
}

fn process_send_result(
    result: std::result::Result<(), web_push::WebPushError>,
    endpoint: &str,
    expired_endpoints: &mut Vec<String>,
) {
    match result {
        Ok(()) => {}
        Err(e) if is_expired_endpoint_error(&e) => {
            tracing::info!(
                "Push subscription expired (410/404), removing: {}",
                endpoint
            );
            expired_endpoints.push(endpoint.to_string());
        }
        Err(e) => {
            tracing::warn!("Push send failed for {}: {e}", endpoint);
        }
    }
}

fn is_expired_endpoint_error(err: &web_push::WebPushError) -> bool {
    matches!(
        err,
        web_push::WebPushError::EndpointNotValid | web_push::WebPushError::EndpointNotFound
    )
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
        let result = send_all(&[], &vapid, &payload).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_is_expired_endpoint_error() {
        assert!(is_expired_endpoint_error(
            &web_push::WebPushError::EndpointNotValid
        ));
        assert!(is_expired_endpoint_error(
            &web_push::WebPushError::EndpointNotFound
        ));
        assert!(!is_expired_endpoint_error(
            &web_push::WebPushError::Unauthorized
        ));
        assert!(!is_expired_endpoint_error(
            &web_push::WebPushError::ServerError(None)
        ));
    }

    #[test]
    fn test_process_send_result_ok_not_collected() {
        let mut expired = Vec::new();
        process_send_result(Ok(()), "https://example.com", &mut expired);
        assert!(expired.is_empty());
    }

    #[test]
    fn test_process_send_result_endpoint_not_valid_collected() {
        let mut expired = Vec::new();
        process_send_result(
            Err(web_push::WebPushError::EndpointNotValid),
            "https://example.com",
            &mut expired,
        );
        assert_eq!(expired, vec!["https://example.com"]);
    }

    #[test]
    fn test_process_send_result_endpoint_not_found_collected() {
        let mut expired = Vec::new();
        process_send_result(
            Err(web_push::WebPushError::EndpointNotFound),
            "https://example.com/push",
            &mut expired,
        );
        assert_eq!(expired, vec!["https://example.com/push"]);
    }

    #[test]
    fn test_process_send_result_other_error_not_collected() {
        let mut expired = Vec::new();
        process_send_result(
            Err(web_push::WebPushError::Unauthorized),
            "https://example.com",
            &mut expired,
        );
        assert!(expired.is_empty());
    }

    #[test]
    fn test_process_send_result_multiple_endpoints() {
        let mut expired = Vec::new();
        process_send_result(Ok(()), "https://ok.com", &mut expired);
        process_send_result(
            Err(web_push::WebPushError::EndpointNotValid),
            "https://gone1.com",
            &mut expired,
        );
        process_send_result(
            Err(web_push::WebPushError::Unauthorized),
            "https://err.com",
            &mut expired,
        );
        process_send_result(
            Err(web_push::WebPushError::EndpointNotFound),
            "https://gone2.com",
            &mut expired,
        );
        assert_eq!(expired, vec!["https://gone1.com", "https://gone2.com"]);
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
        let result = send_all(&[], &vapid, &payload).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
