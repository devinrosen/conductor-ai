use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tracing::info;

use conductor_core::error::Result;
use conductor_core::new_id;

use crate::config::WebPushConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushSubscription {
    pub id: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushPayload {
    pub title: String,
    pub body: String,
    pub tag: Option<String>,
    pub url: Option<String>,
}

fn row_to_subscription(row: &rusqlite::Row) -> rusqlite::Result<PushSubscription> {
    Ok(PushSubscription {
        id: row.get(0)?,
        endpoint: row.get(1)?,
        p256dh: row.get(2)?,
        auth: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

pub fn upsert_subscription(
    db: &Connection,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
) -> Result<PushSubscription> {
    let now = chrono::Utc::now().to_rfc3339();
    let id = new_id();

    db.execute(
        "INSERT INTO push_subscriptions (id, endpoint, p256dh, auth, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(endpoint) DO UPDATE SET
            p256dh = excluded.p256dh,
            auth = excluded.auth,
            updated_at = excluded.updated_at",
        rusqlite::params![id, endpoint, p256dh, auth, now, now],
    )?;

    let subscription = db.query_row(
        "SELECT id, endpoint, p256dh, auth, created_at, updated_at
         FROM push_subscriptions WHERE endpoint = ?1",
        rusqlite::params![endpoint],
        row_to_subscription,
    )?;

    Ok(subscription)
}

pub fn delete_subscription(db: &Connection, endpoint: &str) -> Result<bool> {
    let rows_affected = db.execute(
        "DELETE FROM push_subscriptions WHERE endpoint = ?1",
        rusqlite::params![endpoint],
    )?;

    Ok(rows_affected > 0)
}

pub fn get_all_subscriptions(db: &Connection) -> Result<Vec<PushSubscription>> {
    let mut stmt = db.prepare(
        "SELECT id, endpoint, p256dh, auth, created_at, updated_at
         FROM push_subscriptions ORDER BY created_at DESC",
    )?;

    let subscriptions = stmt
        .query_map([], row_to_subscription)?
        .collect::<std::result::Result<Vec<_>, rusqlite::Error>>()?;

    Ok(subscriptions)
}

pub async fn send_all(db: &Connection, vapid: &WebPushConfig, payload: &PushPayload) -> Result<()> {
    let (private_key, subject) = match (&vapid.vapid_private_key, &vapid.vapid_subject) {
        (Some(pk), Some(s)) => (pk.clone(), s.clone()),
        _ => return Ok(()),
    };

    let subscriptions = get_all_subscriptions(db)?;

    if subscriptions.is_empty() {
        info!("No push subscriptions found, skipping push notification");
        return Ok(());
    }

    info!(
        "Sending push notification to {} subscription(s)",
        subscriptions.len()
    );

    let payload_bytes = serde_json::to_vec(payload)
        .map_err(|e| conductor_core::error::ConductorError::Agent(e.to_string()))?;

    for subscription in &subscriptions {
        match send_to_subscription(&private_key, &subject, subscription, &payload_bytes).await {
            Ok(()) => {}
            Err(web_push::WebPushError::EndpointNotValid)
            | Err(web_push::WebPushError::EndpointNotFound) => {
                tracing::info!(
                    "Push subscription expired (410/404), removing: {}",
                    subscription.endpoint
                );
                if let Err(e) = delete_subscription(db, &subscription.endpoint) {
                    tracing::warn!(
                        "Failed to delete expired subscription {}: {e}",
                        subscription.endpoint
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Push send failed for {}: {e}", subscription.endpoint);
            }
        }
    }

    Ok(())
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
    use conductor_core::test_helpers::create_test_conn;

    #[tokio::test]
    async fn test_upsert_subscription() {
        let db = create_test_conn();

        let subscription =
            upsert_subscription(&db, "https://example.com/push", "p256dh_key", "auth_secret")
                .unwrap();

        assert_eq!(subscription.endpoint, "https://example.com/push");
        assert_eq!(subscription.p256dh, "p256dh_key");
        assert_eq!(subscription.auth, "auth_secret");
    }

    #[tokio::test]
    async fn test_delete_subscription() {
        let db = create_test_conn();

        upsert_subscription(&db, "https://example.com/push", "p256dh_key", "auth_secret").unwrap();

        let deleted = delete_subscription(&db, "https://example.com/push").unwrap();
        assert!(deleted);

        let subscriptions = get_all_subscriptions(&db).unwrap();
        assert!(subscriptions.is_empty());
    }

    #[tokio::test]
    async fn test_get_all_subscriptions() {
        let db = create_test_conn();

        upsert_subscription(
            &db,
            "https://example1.com/push",
            "p256dh_key1",
            "auth_secret1",
        )
        .unwrap();
        upsert_subscription(
            &db,
            "https://example2.com/push",
            "p256dh_key2",
            "auth_secret2",
        )
        .unwrap();

        let subscriptions = get_all_subscriptions(&db).unwrap();
        assert_eq!(subscriptions.len(), 2);
    }
}
