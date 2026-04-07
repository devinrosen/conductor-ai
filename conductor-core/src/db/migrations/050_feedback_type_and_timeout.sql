-- Migration 050: Add structured feedback type, options, and timeout support.
ALTER TABLE feedback_requests ADD COLUMN feedback_type TEXT NOT NULL DEFAULT 'text';
ALTER TABLE feedback_requests ADD COLUMN options_json TEXT;
ALTER TABLE feedback_requests ADD COLUMN timeout_secs INTEGER;
