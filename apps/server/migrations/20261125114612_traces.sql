-- One SQL file for SQLite and Postgres.
-- OTLP spans, the per-minute service rollup, and the trace id of error events.

-- Times are Unix microseconds. A span sent two times is written one time (ON CONFLICT DO NOTHING).
-- is_entry is 1 for a root span, or a server or consumer span.
CREATE TABLE spans (
    source_id TEXT NOT NULL REFERENCES telemetry_sources (id) ON DELETE CASCADE,
    trace_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    parent_span_id TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL,
    environment TEXT NOT NULL DEFAULT '',
    host TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('internal', 'server', 'client', 'producer', 'consumer')),
    start_us BIGINT NOT NULL,
    duration_us BIGINT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('unset', 'ok', 'error')),
    status_message TEXT NOT NULL DEFAULT '',
    is_entry BIGINT NOT NULL,
    http_method TEXT,
    http_route TEXT,
    http_status BIGINT,
    attributes_json TEXT NOT NULL DEFAULT '{}',
    events_json TEXT NOT NULL DEFAULT '[]',
    links_json TEXT NOT NULL DEFAULT '[]',
    resource_json TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (source_id, trace_id, span_id)
);
CREATE INDEX spans_trace ON spans (trace_id);
CREATE INDEX spans_source_start ON spans (source_id, start_us);
CREATE INDEX spans_source_service_entry ON spans (source_id, service, is_entry, start_us);

-- String, number, and bool attributes, for filters.
CREATE TABLE span_attributes (
    source_id TEXT NOT NULL,
    trace_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    FOREIGN KEY (source_id, trace_id, span_id)
        REFERENCES spans (source_id, trace_id, span_id) ON DELETE CASCADE
);
CREATE INDEX span_attributes_key_value ON span_attributes (key, value);
-- The attribute filter (EXISTS) and the cascade from a deleted span look attributes up by span.
CREATE INDEX span_attributes_span ON span_attributes (source_id, trace_id, span_id);

-- Entry spans for each minute (Unix seconds / 60). operation '' is the service total.
-- b00 counts up to 1 ms; b01 .. b18 up to 2, 5, 10, 25, 50, 100, 250, 500 ms, 1, 2.5, 5, 10, 25,
-- 50, 100, 250, 500, 1000 s; b19 above 1000 s.
CREATE TABLE span_rollups (
    source_id TEXT NOT NULL REFERENCES telemetry_sources (id) ON DELETE CASCADE,
    service TEXT NOT NULL,
    operation TEXT NOT NULL,
    minute BIGINT NOT NULL,
    count BIGINT NOT NULL,
    errors BIGINT NOT NULL,
    duration_sum_us BIGINT NOT NULL,
    b00 BIGINT NOT NULL DEFAULT 0,
    b01 BIGINT NOT NULL DEFAULT 0,
    b02 BIGINT NOT NULL DEFAULT 0,
    b03 BIGINT NOT NULL DEFAULT 0,
    b04 BIGINT NOT NULL DEFAULT 0,
    b05 BIGINT NOT NULL DEFAULT 0,
    b06 BIGINT NOT NULL DEFAULT 0,
    b07 BIGINT NOT NULL DEFAULT 0,
    b08 BIGINT NOT NULL DEFAULT 0,
    b09 BIGINT NOT NULL DEFAULT 0,
    b10 BIGINT NOT NULL DEFAULT 0,
    b11 BIGINT NOT NULL DEFAULT 0,
    b12 BIGINT NOT NULL DEFAULT 0,
    b13 BIGINT NOT NULL DEFAULT 0,
    b14 BIGINT NOT NULL DEFAULT 0,
    b15 BIGINT NOT NULL DEFAULT 0,
    b16 BIGINT NOT NULL DEFAULT 0,
    b17 BIGINT NOT NULL DEFAULT 0,
    b18 BIGINT NOT NULL DEFAULT 0,
    b19 BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (source_id, service, operation, minute)
);
-- The services list reads the service totals of a range.
CREATE INDEX span_rollups_source_operation_minute ON span_rollups (source_id, operation, minute);

-- The operations each service has in the rollup, so the cap of 200 is one small count.
-- Operations past the cap go to '(other)', which is not listed here.
CREATE TABLE span_operations (
    source_id TEXT NOT NULL REFERENCES telemetry_sources (id) ON DELETE CASCADE,
    service TEXT NOT NULL,
    operation TEXT NOT NULL,
    PRIMARY KEY (source_id, service, operation)
);

-- contexts.trace.trace_id of an error event: 32 lowercase hex characters.
ALTER TABLE events ADD COLUMN trace_id TEXT;
CREATE INDEX events_trace ON events (trace_id);
