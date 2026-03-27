use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tracing::info;

use conductor_core::{error::Result, new_id};

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

pub struct PushSubscriptionManager<'a> {
    db: &'a Connection,
    vapid_private_key: String,
    #[allow(dead_code)]
    // public key is served to browsers via API; not needed for server-side signing
    vapid_public_key: String,
    vapid_subject: String,
}

impl<'a> PushSubscriptionManager<'a> {
    pub fn new(
        db: &'a Connection,
        vapid_private_key: String,
        vapid_public_key: String,
        vapid_subject: String,
    ) -> Self {
        Self {
            db,
            vapid_private_key,
            vapid_public_key,
            vapid_subject,
        }
    }

    /// Create or update a push subscription
    pub fn upsert_subscription(
        &self,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
    ) -> Result<PushSubscription> {
        let now = chrono::Utc::now().to_rfc3339();
        let id = new_id();

        self.db.execute(
            "INSERT INTO push_subscriptions (id, endpoint, p256dh, auth, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(endpoint) DO UPDATE SET
                p256dh = excluded.p256dh,
                auth = excluded.auth,
                updated_at = excluded.updated_at",
            rusqlite::params![id, endpoint, p256dh, auth, now, now],
        )?;

        // Retrieve the final subscription (either inserted or updated)
        let subscription = self.db.query_row(
            "SELECT id, endpoint, p256dh, auth, created_at, updated_at
             FROM push_subscriptions WHERE endpoint = ?1",
            rusqlite::params![endpoint],
            |row| {
                Ok(PushSubscription {
                    id: row.get(0)?,
                    endpoint: row.get(1)?,
                    p256dh: row.get(2)?,
                    auth: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        )?;

        Ok(subscription)
    }

    /// Delete a push subscription by endpoint
    pub fn delete_subscription(&self, endpoint: &str) -> Result<bool> {
        let rows_affected = self.db.execute(
            "DELETE FROM push_subscriptions WHERE endpoint = ?1",
            rusqlite::params![endpoint],
        )?;

        Ok(rows_affected > 0)
    }

    /// Get all active push subscriptions
    pub fn get_all_subscriptions(&self) -> Result<Vec<PushSubscription>> {
        let mut stmt = self.db.prepare(
            "SELECT id, endpoint, p256dh, auth, created_at, updated_at
             FROM push_subscriptions ORDER BY created_at DESC",
        )?;

        let subscriptions = stmt
            .query_map([], |row| {
                Ok(PushSubscription {
                    id: row.get(0)?,
                    endpoint: row.get(1)?,
                    p256dh: row.get(2)?,
                    auth: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, rusqlite::Error>>()?;

        Ok(subscriptions)
    }

    /// Send push notification to all subscriptions
    pub async fn send_all(&self, payload: &PushPayload) -> Result<()> {
        let subscriptions = self.get_all_subscriptions()?;

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
            match self
                .send_to_subscription(subscription, &payload_bytes)
                .await
            {
                Ok(()) => {}
                Err(web_push::WebPushError::EndpointNotValid)
                | Err(web_push::WebPushError::EndpointNotFound) => {
                    tracing::info!(
                        "Push subscription expired (410/404), removing: {}",
                        subscription.endpoint
                    );
                    let _ = self.delete_subscription(&subscription.endpoint);
                }
                Err(e) => {
                    tracing::warn!("Push send failed for {}: {e}", subscription.endpoint);
                }
            }
        }

        Ok(())
    }

    async fn send_to_subscription(
        &self,
        subscription: &PushSubscription,
        payload_json: &[u8],
    ) -> std::result::Result<(), web_push::WebPushError> {
        use web_push::{
            ContentEncoding, IsahcWebPushClient, SubscriptionInfo, VapidSignatureBuilder,
            WebPushClient, WebPushMessageBuilder, URL_SAFE_NO_PAD,
        };

        let sub_info = SubscriptionInfo::new(
            &subscription.endpoint,
            &subscription.p256dh,
            &subscription.auth,
        );
        let mut sig_builder = VapidSignatureBuilder::from_base64(
            &self.vapid_private_key,
            URL_SAFE_NO_PAD,
            &sub_info,
        )?;
        sig_builder.add_claim("sub", self.vapid_subject.as_str());
        let signature = sig_builder.build()?;

        let mut builder = WebPushMessageBuilder::new(&sub_info);
        builder.set_payload(ContentEncoding::Aes128Gcm, payload_json);
        builder.set_vapid_signature(signature);

        let client = IsahcWebPushClient::new()?;
        client.send(builder.build()?).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::test_helpers::create_test_conn;

    async fn setup_test_db() -> Connection {
        // create_test_conn() runs all migrations, including the one that creates
        // push_subscriptions (migration 052).
        create_test_conn()
    }

    #[tokio::test]
    async fn test_upsert_subscription() {
        let db = setup_test_db().await;
        let manager = PushSubscriptionManager::new(
            &db,
            "test_private_key".to_string(),
            "test_public_key".to_string(),
            "mailto:test@example.com".to_string(),
        );

        let subscription = manager
            .upsert_subscription("https://example.com/push", "p256dh_key", "auth_secret")
            .unwrap();

        assert_eq!(subscription.endpoint, "https://example.com/push");
        assert_eq!(subscription.p256dh, "p256dh_key");
        assert_eq!(subscription.auth, "auth_secret");
    }

    #[tokio::test]
    async fn test_delete_subscription() {
        let db = setup_test_db().await;
        let manager = PushSubscriptionManager::new(
            &db,
            "test_private_key".to_string(),
            "test_public_key".to_string(),
            "mailto:test@example.com".to_string(),
        );

        manager
            .upsert_subscription("https://example.com/push", "p256dh_key", "auth_secret")
            .unwrap();

        let deleted = manager
            .delete_subscription("https://example.com/push")
            .unwrap();
        assert!(deleted);

        let subscriptions = manager.get_all_subscriptions().unwrap();
        assert!(subscriptions.is_empty());
    }

    #[tokio::test]
    async fn test_get_all_subscriptions() {
        let db = setup_test_db().await;
        let manager = PushSubscriptionManager::new(
            &db,
            "test_private_key".to_string(),
            "test_public_key".to_string(),
            "mailto:test@example.com".to_string(),
        );

        manager
            .upsert_subscription("https://example1.com/push", "p256dh_key1", "auth_secret1")
            .unwrap();
        manager
            .upsert_subscription("https://example2.com/push", "p256dh_key2", "auth_secret2")
            .unwrap();

        let subscriptions = manager.get_all_subscriptions().unwrap();
        assert_eq!(subscriptions.len(), 2);
    }
}
