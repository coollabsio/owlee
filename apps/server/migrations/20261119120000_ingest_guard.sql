-- One SQL file for SQLite and Postgres.
-- Public ingest guard: DNS verification, daily quotas, payload sampling, retention indexes.

ALTER TABLE insight_sites ADD COLUMN verified BIGINT NOT NULL DEFAULT 0;
ALTER TABLE insight_sites ADD COLUMN verify_token TEXT NOT NULL DEFAULT '';
ALTER TABLE insight_sites ADD COLUMN verified_at BIGINT;
-- NULL inherits the instance analytics cap.
ALTER TABLE insight_sites ADD COLUMN daily_quota BIGINT;

-- NULL inherits the instance error cap.
ALTER TABLE projects ADD COLUMN daily_quota BIGINT;

ALTER TABLE issues ADD COLUMN stored_count BIGINT NOT NULL DEFAULT 0;
ALTER TABLE issues ADD COLUMN sample_window_start TEXT;

-- Safety valve, not a bill. One error a second all day is about 86,400.
ALTER TABLE instance_settings ADD COLUMN error_daily_quota BIGINT NOT NULL DEFAULT 100000;
-- A million pageviews a day is a large site for a self-hosted install.
ALTER TABLE instance_settings ADD COLUMN analytics_daily_quota BIGINT NOT NULL DEFAULT 1000000;
ALTER TABLE instance_settings ADD COLUMN quotas_updated_at BIGINT;

CREATE TABLE ingest_usage (
    kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    day TEXT NOT NULL,
    count BIGINT NOT NULL,
    PRIMARY KEY (kind, subject_id, day)
);

-- Raw pageviews and link clicks share this table. Retention deletes by timestamp.
CREATE INDEX insight_events_timestamp ON insight_events (timestamp);

-- Retention deletes and the 24h trend (`WHERE timestamp >= ?`) both scan by time.
CREATE INDEX events_timestamp ON events (timestamp);
