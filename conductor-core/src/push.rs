use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::new_id;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushSubscription {
    pub id: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub created_at: String,
    pub updated_at: String,
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

    let subscription = db.query_row(
        "INSERT INTO push_subscriptions (id, endpoint, p256dh, auth, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(endpoint) DO UPDATE SET
            p256dh = excluded.p256dh,
            auth = excluded.auth,
            updated_at = excluded.updated_at
         RETURNING id, endpoint, p256dh, auth, created_at, updated_at",
        rusqlite::params![id, endpoint, p256dh, auth, now, now],
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::create_test_conn;

    #[test]
    fn test_upsert_subscription() {
        let db = create_test_conn();

        let subscription =
            upsert_subscription(&db, "https://example.com/push", "p256dh_key", "auth_secret")
                .unwrap();

        assert_eq!(subscription.endpoint, "https://example.com/push");
        assert_eq!(subscription.p256dh, "p256dh_key");
        assert_eq!(subscription.auth, "auth_secret");
    }

    #[test]
    fn test_upsert_subscription_update() {
        let db = create_test_conn();

        let inserted =
            upsert_subscription(&db, "https://example.com/push", "p256dh_key", "auth_secret")
                .unwrap();

        let updated =
            upsert_subscription(&db, "https://example.com/push", "p256dh_new", "auth_new")
                .unwrap();

        assert_eq!(inserted.id, updated.id);
        assert_eq!(inserted.created_at, updated.created_at);
        assert_eq!(updated.p256dh, "p256dh_new");
        assert_eq!(updated.auth, "auth_new");

        let all = get_all_subscriptions(&db).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn test_delete_subscription() {
        let db = create_test_conn();

        upsert_subscription(&db, "https://example.com/push", "p256dh_key", "auth_secret").unwrap();

        let deleted = delete_subscription(&db, "https://example.com/push").unwrap();
        assert!(deleted);

        let subscriptions = get_all_subscriptions(&db).unwrap();
        assert!(subscriptions.is_empty());
    }

    #[test]
    fn test_get_all_subscriptions() {
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
