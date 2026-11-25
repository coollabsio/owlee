-- One SQL file for SQLite and Postgres.
-- OTLP telemetry sources and logs. Times are Unix milliseconds.

-- token_hash is the SHA-256 of the bearer token; the token is shown one time.
-- deleted_at stops intake at once; the retention task then deletes the logs in batches, then the row.
-- first_log_at is set by the first stored log, for "waiting for the first data".
CREATE TABLE telemetry_sources (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    token_hint TEXT NOT NULL,
    daily_quota BIGINT,
    created_at BIGINT NOT NULL,
    first_log_at BIGINT,
    deleted_at BIGINT
);
-- A deleted source keeps its row until its logs are gone, so its name is free at once.
CREATE UNIQUE INDEX telemetry_sources_project_name ON telemetry_sources (project_id, name) WHERE deleted_at IS NULL;

-- id is a UUIDv7, so it sorts by intake time. body is at most 16 KiB.
CREATE TABLE logs (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES telemetry_sources (id) ON DELETE CASCADE,
    time_ms BIGINT NOT NULL,
    level TEXT NOT NULL CHECK (level IN ('trace', 'debug', 'info', 'warn', 'error', 'fatal')),
    severity_text TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL,
    environment TEXT NOT NULL DEFAULT '',
    host TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL,
    trace_id TEXT,
    span_id TEXT,
    attributes_json TEXT NOT NULL DEFAULT '{}',
    resource_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX logs_source_time ON logs (source_id, time_ms);
CREATE INDEX logs_source_service_time ON logs (source_id, service, time_ms);
CREATE INDEX logs_source_level_time ON logs (source_id, level, time_ms);
CREATE INDEX logs_trace ON logs (trace_id);

-- String, number, and bool attributes, for filters.
CREATE TABLE log_attributes (
    log_id TEXT NOT NULL REFERENCES logs (id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL
);
CREATE INDEX log_attributes_key_value ON log_attributes (key, value);
-- The cascade from a deleted log looks its attributes up by log_id.
CREATE INDEX log_attributes_log ON log_attributes (log_id);
