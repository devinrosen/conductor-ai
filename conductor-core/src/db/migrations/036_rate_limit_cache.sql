CREATE TABLE IF NOT EXISTS rate_limit_cache (
    rate_limit_type TEXT PRIMARY KEY,
    resets_at       INTEGER NOT NULL,   -- Unix timestamp
    utilization     REAL    NOT NULL,   -- 0.0–1.0
    status          TEXT    NOT NULL,   -- "allowed", "allowed_warning", "rejected"
    last_updated_at TEXT    NOT NULL    -- ISO 8601
);
