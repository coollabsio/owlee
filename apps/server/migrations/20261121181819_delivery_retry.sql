-- One SQL file for SQLite and Postgres.
-- Failed deliveries are retried, so a delivery waits as pending with its payload.
-- SQLite cannot change a CHECK constraint in place, so the table is rebuilt.

-- Whether the channel gets new and regressed error-tracking issues.
ALTER TABLE notifications ADD COLUMN error_issues BIGINT NOT NULL DEFAULT 1;

CREATE TABLE notification_deliveries_next (
    id TEXT PRIMARY KEY,
    notification_id TEXT NOT NULL REFERENCES notifications (id) ON DELETE CASCADE,
    monitor_id TEXT,
    incident_id TEXT,
    event TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'sent', 'failed', 'skipped')),
    error TEXT,
    created_at BIGINT NOT NULL,
    attempts BIGINT NOT NULL DEFAULT 0,
    -- Set while pending: when the next attempt may start.
    next_attempt_at BIGINT,
    -- The payload, kept while pending so a retry sends the same body.
    payload_json TEXT
);

INSERT INTO notification_deliveries_next
    (id, notification_id, monitor_id, incident_id, event, status, error, created_at, attempts)
SELECT id, notification_id, monitor_id, incident_id, event, status, error, created_at, 1
FROM notification_deliveries;

DROP TABLE notification_deliveries;

ALTER TABLE notification_deliveries_next RENAME TO notification_deliveries;

CREATE INDEX notification_deliveries_time ON notification_deliveries (notification_id, created_at);
CREATE INDEX notification_deliveries_due ON notification_deliveries (status, next_attempt_at);
