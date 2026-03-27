CREATE TABLE push_subscriptions (
    id TEXT NOT NULL PRIMARY KEY,
    endpoint TEXT NOT NULL UNIQUE,
    p256dh TEXT NOT NULL,
    auth TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Index on endpoint for efficient lookups
CREATE INDEX idx_push_subscriptions_endpoint ON push_subscriptions(endpoint);