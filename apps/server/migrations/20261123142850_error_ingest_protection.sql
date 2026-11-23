-- One SQL file for SQLite and Postgres.
-- Error ingest protection: allowed domains, inbound filters, spike protection.

-- One host per line. Empty allows every origin.
ALTER TABLE error_apps ADD COLUMN allowed_domains TEXT NOT NULL DEFAULT '';
-- A comma list of inbound filter ids.
ALTER TABLE error_apps ADD COLUMN inbound_filters TEXT NOT NULL DEFAULT 'browser_extensions,web_crawlers';
ALTER TABLE error_apps ADD COLUMN spike_protection BIGINT NOT NULL DEFAULT 1;

-- Stored events per app and UTC hour. `hour` is the Unix second of the hour start.
CREATE TABLE error_hourly_usage (
    app_id TEXT NOT NULL REFERENCES error_apps (id) ON DELETE CASCADE,
    hour BIGINT NOT NULL,
    count BIGINT NOT NULL,
    PRIMARY KEY (app_id, hour)
);
