CREATE TABLE notification_log (
  entity_id   TEXT NOT NULL,
  event_type  TEXT NOT NULL,
  fired_at    TEXT NOT NULL,
  PRIMARY KEY (entity_id, event_type)
);
