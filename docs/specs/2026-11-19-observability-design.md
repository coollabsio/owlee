# Logs, traces, and metrics design

Status: draft. Nothing in this document is built yet.

## 1. Goal

Owlee tells you that a service is down (monitors) and that it throws (errors). It does not tell you why. This
design adds the three signals that answer "why": logs, traces, and metrics. They come from OpenTelemetry SDKs
and the OpenTelemetry Collector over OTLP/HTTP, and they connect to the error issues that Owlee already has.

The rules that Owlee has now stay the same:

- One Rust binary. No new service, queue, or database engine.
- SQLite is the default. Postgres uses the same schema and the same SQL.
- Every row belongs to a project, and a project belongs to a workspace. A user from another workspace gets 404.
- Ingest is guarded by body limits, token buckets, daily quotas, intake counters, and retention.

The idea comes from [DataCat](https://github.com/replit/datacat). Owlee does not copy its code, its Datadog
intake, or its storage.

## 2. Scope

### In scope

- P0. Telemetry sources, OTLP/HTTP intake, guards, retention, and the settings page for a source.
- P1. Logs: intake, search, filters, live tail, and links to traces and errors.
- P2. Traces: intake, service list, trace list, span waterfall, and links to logs and errors.
- P3. Metrics: intake for gauges, sums, and histograms; a metrics explorer with charts.
- P4. Alerts: monitors that check a log count, a trace error rate or latency, or a metric value.

### Not in scope

- OTLP over gRPC (D2).
- Datadog Agent intake and Datadog tracer formats (D3).
- ClickHouse or another column store (D4).
- Custom dashboards with many widgets. The metrics explorer has one chart for each query (D15).
- Host maps, infrastructure inventory, and an Owlee agent that collects host metrics. Use the OpenTelemetry
  Collector `hostmetrics` receiver (D16).
- Tail sampling and server-side sampling rules.
- Exponential histograms and summaries (D13).

## 3. Concepts

| Term | Meaning |
|---|---|
| Source | A named OTLP intake in a project. It has one token. One source can receive data from many services. |
| Service | The `service.name` resource attribute. Owlee shows it, filters on it, and groups on it. A service is not a database row. |
| Resource | The OTLP resource: attributes that describe the process, such as `service.name`, `host.name`, and `deployment.environment.name`. |
| Log record | One OTLP log record. |
| Span | One OTLP span. A trace is all spans with the same `trace_id`. |
| Series | One metric name with one set of attributes. |
| Point | One value of a series at one time. |

## 4. Shared foundation (P0)

### 4.1 Sources

```sql
CREATE TABLE telemetry_sources (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    token_hint TEXT NOT NULL,          -- last 4 characters, for the UI
    daily_quota BIGINT,                -- NULL inherits the instance default
    created_at BIGINT NOT NULL,
    UNIQUE (project_id, name)
);
```

- The token is shown one time, when the source is made or rotated. Owlee stores only its SHA-256 (D5).
- An editor makes, renames, rotates, and deletes sources. A member sees the list but not the token.
- When a source is deleted, its logs, spans, and metrics are deleted by foreign-key cascade. On a large
  source, a delete is done in batches by the retention task, and the source row is marked `deleted_at`
  first so that intake stops at once.

### 4.2 OTLP/HTTP intake

| Route | Body |
|---|---|
| `POST /otlp/v1/logs` | `ExportLogsServiceRequest` |
| `POST /otlp/v1/traces` | `ExportTraceServiceRequest` |
| `POST /otlp/v1/metrics` | `ExportMetricsServiceRequest` |

A user sets two variables in the SDK or Collector:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=https://owlee.example.com/otlp
OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer <token>"
```

Rules, from the [OTLP specification](https://opentelemetry.io/docs/specs/otlp/):

- `Content-Type: application/x-protobuf` and `application/json` are both accepted. The response uses the
  same encoding as the request.
- `Content-Encoding: gzip` is accepted. The existing `decode_body` is reused.
- Success is `200` with an `Export<Signal>ServiceResponse`. When Owlee drops some items, it fills
  `partial_success` with the rejected count and one message. The client does not retry this.
- A full token bucket or daily quota is `429` with `Retry-After`. The client may retry.
- A bad token is `401`. A bad body is `400`. A body above `OWLEE_OTLP_MAX_BODY_BYTES` is `413`.
- The paths are under `/otlp` so that they do not mix with the Owlee API (D6).

Decoding uses the `opentelemetry-proto` crate with only the message types and serde (D7). The first P0 task
measures what this adds to build time and binary size. If the cost is too high, Owlee uses `prost` with
generated code that is committed to the repository.

### 4.3 Normalization

Each signal is turned into rows by a pure function in a new crate `crates/telemetry`, in the same way as
`crates/errors` (D8). The function takes the decoded request and returns rows plus a list of drops. It does
no I/O, so it is tested with fixed inputs.

Common columns that each log, span, and series gets from its resource:

| Column | Source |
|---|---|
| `service` | `service.name`, else `unknown_service` |
| `environment` | `deployment.environment.name`, else `deployment.environment`, else empty |
| `host` | `host.name`, else empty |
| `resource_json` | all resource attributes, as JSON text |

### 4.4 Attributes and search

Owlee does not use JSON functions in SQL, because SQLite and Postgres use different functions (D9). Each
signal stores its attributes in two forms:

- `attributes_json`: all attributes, as JSON text, for the detail view.
- A key-value table, for filters: one row for each attribute whose value is a string, a number, or a bool.
  This is the same pattern as `insight_event_data`.

Limits for each record: 64 attributes, keys up to 128 characters, values up to 1 KiB. Owlee cuts what is
longer and counts the record as stored.

### 4.5 Privacy

- Attribute values under a sensitive key are replaced with `[Filtered]`. The key check of
  `crates/errors/src/scrub.rs` is made public and reused.
- Log bodies, span names, span event messages, and attribute values go through `scrub_text`.
- `client.address`, `source.address`, and `net.peer.ip` are removed before storage, in the same way as
  `user.ip_address` for errors.
- `PRIVACY.md` gets a section for telemetry. Telemetry is not part of the per-user export and erasure,
  because it has no user key. A user deletes telemetry by source or by retention (D10).

### 4.6 Guards

The guards that exist for errors and analytics are reused:

| Guard | Setting | Default |
|---|---|---|
| Body size | `OWLEE_OTLP_MAX_BODY_BYTES` | 4 MiB |
| Token bucket per source | `OWLEE_OTLP_RATE_SOURCE_PER_MIN`, `_BURST` | 600 requests per minute |
| Token bucket per client IP | `OWLEE_OTLP_RATE_IP_PER_MIN`, `_BURST` | 600 requests per minute |
| Daily quota per source, per signal | `OWLEE_LOG_DAILY_QUOTA`, `OWLEE_SPAN_DAILY_QUOTA`, `OWLEE_METRIC_POINT_DAILY_QUOTA` | 1,000,000 each |
| Intake counters | `IntakeStats`, with one key for each source and signal | in memory |

The quota counts items (log records, spans, points), not requests. One request can be partly accepted:
Owlee stores items until the quota is full and reports the rest in `partial_success`.

The default numbers are guesses. P0 includes a load test (section 10) that measures what one SQLite file can
take, and the defaults are set from that result.

### 4.7 Writes

- One request is one transaction. Rows are written with multi-row `INSERT` statements of up to 500 rows.
- The request waits for the commit. Owlee has no queue in memory, so a crash does not lose data that the
  client was told is stored (D11).
- Rollup rows (sections 6.3 and 7.3) are updated in the same transaction with
  `INSERT ... ON CONFLICT ... DO UPDATE`. That statement works on SQLite and Postgres.

### 4.8 Retention

`retention.rs` gets three more batched deletes:

| Data | Setting | Default |
|---|---|---|
| Logs | `OWLEE_LOG_RETENTION_DAYS` | 7 |
| Spans | `OWLEE_TRACE_RETENTION_DAYS` | 7 |
| Metric points | `OWLEE_METRIC_RETENTION_DAYS` | 7 |
| Span and metric rollups | `OWLEE_ROLLUP_RETENTION_DAYS` | 90 |

### 4.9 UI

A new sidebar group "Observability" has the pages Logs, Traces, and Metrics. Each page uses the project
filter that the other pages use. "Sources" is a tab in each page and also a section in project settings.

The source page shows:

- the endpoint and the two environment variables, ready to copy;
- a Collector config example and SDK examples for Node.js, Python, Go, and Rust;
- the intake counters (stored, dropped, last drop reason) for each signal;
- "Waiting for the first data" until the first item arrives, as the error setup flow does now.

## 5. Logs (P1)

### 5.1 Data model

```sql
CREATE TABLE logs (
    id TEXT PRIMARY KEY,                 -- UUIDv7, so the id sorts by intake time
    source_id TEXT NOT NULL REFERENCES telemetry_sources (id) ON DELETE CASCADE,
    time_ms BIGINT NOT NULL,             -- time_unix_nano, else observed_time_unix_nano
    level TEXT NOT NULL CHECK (level IN ('trace', 'debug', 'info', 'warn', 'error', 'fatal')),
    severity_text TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL,
    environment TEXT NOT NULL DEFAULT '',
    host TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL,                  -- a string body, or other bodies as JSON; max 16 KiB
    trace_id TEXT,                       -- 32 lowercase hex characters
    span_id TEXT,                        -- 16 lowercase hex characters
    attributes_json TEXT NOT NULL DEFAULT '{}',
    resource_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX logs_source_time ON logs (source_id, time_ms);
CREATE INDEX logs_source_service_time ON logs (source_id, service, time_ms);
CREATE INDEX logs_source_level_time ON logs (source_id, level, time_ms);
CREATE INDEX logs_trace ON logs (trace_id);

CREATE TABLE log_attributes (
    log_id TEXT NOT NULL REFERENCES logs (id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL
);
CREATE INDEX log_attributes_key_value ON log_attributes (key, value);
```

`level` comes from `severity_number`: 1–4 trace, 5–8 debug, 9–12 info, 13–16 warn, 17–20 error,
21–24 fatal. When the number is 0, Owlee reads `severity_text`.

### 5.2 Search

One search box with a small query language (D12):

| Input | Meaning |
|---|---|
| `timeout` or `"connection reset"` | the body contains this text (case-insensitive `LIKE`) |
| `service:api` | column filter; also `level:`, `env:`, `host:`, `trace:` |
| `level:>=warn` | warn, error, and fatal |
| `http.route:/checkout` | attribute filter, through `log_attributes` |
| `-env:dev` | not |
| two terms | both must match |

The parser is a pure function that returns a typed filter. A second function turns the filter into SQL with
bound values. No user text goes into the SQL string.

Every search has a time range (default: the last 15 minutes, maximum 7 days) and returns at most 200 rows,
newest first, with a cursor for the next page. Text search uses `LIKE`, so its speed depends on the number of
rows in the time range. The load test in P0 measures this.

The page also shows counts for each level and the top 10 services in the range, as filter buttons.

### 5.3 Live tail

Owlee has no realtime layer. Live tail calls `GET /api/logs?after=<cursor>` every 2 seconds while the tab is
visible and adds new rows at the top. It stops after 10 minutes, and the user can start it again (D14).

### 5.4 Links

- A log with a `trace_id` links to the trace (P2).
- An error event with `contexts.trace.trace_id` shows "Logs for this trace" on the issue page.
- The trace page shows the logs of the trace below the waterfall.

## 6. Traces (P2)

### 6.1 Data model

```sql
CREATE TABLE spans (
    source_id TEXT NOT NULL REFERENCES telemetry_sources (id) ON DELETE CASCADE,
    trace_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    parent_span_id TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL,
    environment TEXT NOT NULL DEFAULT '',
    host TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    kind TEXT NOT NULL,                  -- internal, server, client, producer, consumer
    start_us BIGINT NOT NULL,            -- microseconds; enough for a waterfall
    duration_us BIGINT NOT NULL,
    status TEXT NOT NULL,                -- unset, ok, error
    status_message TEXT NOT NULL DEFAULT '',
    is_entry BIGINT NOT NULL,            -- 1 for a root span, or a server or consumer span
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
```

A span that is sent two times is written one time: the insert uses `ON CONFLICT DO NOTHING`. A retry from
the client is therefore safe.

### 6.2 Pages

- **Services.** One row for each service in the time range: requests, error rate, p50, p95, and p99 of the
  entry spans. The data comes from the rollup in 6.3.
- **Service.** Charts for requests, errors, and latency, and a table of operations (`http_route`, else
  span name) with the same numbers.
- **Trace list.** Entry spans in the time range, filtered with the same query language as logs, plus
  `duration:>500ms` and `status:error`. Newest first, 200 rows, cursor.
- **Trace.** The waterfall: all spans of the trace in a tree, sorted by start time, with the service as the
  color. A click on a span shows its attributes, events, and links. Below the waterfall: the logs of the trace
  and the error events of the trace.

A trace can have spans that arrive late or never. The waterfall shows a span whose parent is missing under
the root, with a "parent not received" note.

### 6.3 Service rollup

Percentiles cannot be computed in SQL in the same way on SQLite and Postgres. Owlee keeps a histogram with
fixed buckets for each minute, and computes percentiles in Rust:

```sql
CREATE TABLE span_rollups (
    source_id TEXT NOT NULL REFERENCES telemetry_sources (id) ON DELETE CASCADE,
    service TEXT NOT NULL,
    operation TEXT NOT NULL,             -- empty string for the service total
    minute BIGINT NOT NULL,              -- unix seconds / 60
    count BIGINT NOT NULL,
    errors BIGINT NOT NULL,
    duration_sum_us BIGINT NOT NULL,
    b00 BIGINT NOT NULL DEFAULT 0,       -- up to 1 ms
    -- b01 .. b18: 2, 5, 10, 25, 50, 100, 250, 500 ms, 1, 2.5, 5, 10, 25, 50, 100, 250, 500, 1000 s
    b19 BIGINT NOT NULL DEFAULT 0,       -- above 1000 s
    PRIMARY KEY (source_id, service, operation, minute)
);
```

Each entry span adds 1 to two rows (the operation and the service total) in the ingest transaction. The
buckets are columns, not JSON, so that `ON CONFLICT DO UPDATE SET b03 = span_rollups.b03 + excluded.b03`
works on both databases. A percentile is read from the buckets, so it is exact to the bucket bounds.
The rollup is kept 90 days, so the service pages show 90 days after the spans are deleted.

The operation count for each service is limited to 200. Owlee adds more operations to `(other)`.

### 6.4 Errors and traces

- `events` gets a `trace_id` column and an index. Error ingest fills it from `contexts.trace.trace_id`.
- The issue page shows "View trace" when spans exist for that trace id.
- The trace page lists the error events of the trace, with links to their issues.
- A span event named `exception` is shown in the span detail. It does not make an error issue in P2. The
  Sentry SDK path stays the way to make issues.

## 7. Metrics (P3)

### 7.1 Supported types

| OTLP type | Stored as |
|---|---|
| Gauge | the value |
| Sum, monotonic, cumulative | the value; the query computes the rate and handles a counter reset |
| Sum, monotonic, delta | the value; the query sums it |
| Sum, not monotonic | the value, as a gauge |
| Histogram (explicit buckets) | count, sum, min, max, and bucket counts |
| Exponential histogram, Summary | dropped, counted as `unsupported_type` in `partial_success` (D13) |

### 7.2 Data model

```sql
CREATE TABLE metric_series (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES telemetry_sources (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,                  -- gauge, counter, delta_counter, histogram
    unit TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL,
    attributes_key TEXT NOT NULL,        -- SHA-256 of the sorted attributes
    attributes_json TEXT NOT NULL,
    last_seen_ms BIGINT NOT NULL,
    UNIQUE (source_id, name, attributes_key)
);
CREATE INDEX metric_series_source_name ON metric_series (source_id, name);

CREATE TABLE metric_series_attributes (
    series_id TEXT NOT NULL REFERENCES metric_series (id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL
);
CREATE INDEX metric_series_attributes_key_value ON metric_series_attributes (key, value);

CREATE TABLE metric_points (
    series_id TEXT NOT NULL REFERENCES metric_series (id) ON DELETE CASCADE,
    time_s BIGINT NOT NULL,
    value DOUBLE PRECISION NOT NULL,     -- for a histogram: sum
    count BIGINT,                        -- histogram only
    min DOUBLE PRECISION,
    max DOUBLE PRECISION,
    buckets_json TEXT,                   -- histogram only: bounds and counts
    PRIMARY KEY (series_id, time_s)
);

CREATE TABLE metric_rollups (
    series_id TEXT NOT NULL REFERENCES metric_series (id) ON DELETE CASCADE,
    bucket_s BIGINT NOT NULL,            -- start of a 5-minute bucket
    count BIGINT NOT NULL,
    sum DOUBLE PRECISION NOT NULL,
    min DOUBLE PRECISION NOT NULL,
    max DOUBLE PRECISION NOT NULL,
    last DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (series_id, bucket_s)
);
```

- Points are stored at one-second resolution. A second point for the same second replaces the first.
- The rollup is updated in the ingest transaction. `min` and `max` use `CASE WHEN`, because SQLite and
  Postgres do not share `LEAST` and `GREATEST`.
- A query for more than 24 hours reads the rollup. A shorter query reads the points.

### 7.3 Cardinality

A source may have 10,000 series (`OWLEE_METRIC_SERIES_PER_SOURCE`). A point for a new series above the limit
is dropped with the reason `cardinality`, and the source page shows a warning with the metric names that
have the most series. Points for existing series are still accepted.

### 7.4 Explorer

One query is: metric name, filters (the same language), aggregation over series (avg, sum, min, max, count),
a group-by attribute (at most 10 groups shown, the rest as "other"), and a time range. A counter is shown as
a rate per second. A histogram can show p50, p95, p99, or the average.

The server returns one array of points for each group, with at most 300 points for each line. The chart uses
the chart component that the error pages use now.

The explorer state is in the URL, so that a user can share a chart. A "Saved charts" list for each project
is the only saved state in P3 (D15).

## 8. Alerts (P4)

Alerts reuse monitors. A monitor gets three more job types (D17):

| Job type | Check | Example |
|---|---|---|
| `log` | count of logs that match a query in a window | `level:error service:api` > 50 in 5 minutes |
| `trace` | error rate or a latency percentile of a service or an operation | p95 of `POST /checkout` > 800 ms in 10 minutes |
| `metric` | an aggregation of a metric query in a window | avg `system.cpu.utilization` > 0.9 in 5 minutes |

- The check runs in the scheduler at the monitor interval, in the API process, as HTTP checks do now.
- A check result is `up` or `down`. The failure threshold, incidents, maintenance windows, status page
  components, and notifications work as they do now. A telemetry monitor can be a status page component.
- A window with no data is `up` by default. A setting can make it `down` ("no data is a failure").
- The `job_type` CHECK in `monitors` gets the three values. The query and the threshold are in a new
  `query_json` column. `url` is empty for these types.

## 9. API

| Method and path | Use |
|---|---|
| `GET/POST /api/telemetry-sources` | list, create (returns the token one time) |
| `PATCH/DELETE /api/telemetry-sources/{id}` | rename, set quota, delete |
| `POST /api/telemetry-sources/{id}/rotate` | new token (returns it one time) |
| `GET /api/logs?q=&from=&to=&cursor=&after=` | search and live tail |
| `GET /api/logs/facets?q=&from=&to=` | level counts and top services |
| `GET /api/services?from=&to=` | service list |
| `GET /api/services/{service}?from=&to=` | service charts and operations |
| `GET /api/traces?q=&from=&to=&cursor=` | trace list |
| `GET /api/traces/{trace_id}` | spans, logs, and error events of a trace |
| `GET /api/metrics/names?q=` | metric names, for the picker |
| `POST /api/metrics/query` | one explorer query |
| `GET/POST/DELETE /api/metric-charts` | saved charts |

Every route takes the project filter and checks the workspace, as the error routes do. A `trace_id` from
another workspace returns 404.

## 10. Build order

Each step ends in a state that is tested and that can ship.

1. P0.1. Add `opentelemetry-proto`; measure build time and binary size. Decide D7.
2. P0.2. `crates/telemetry` with log normalization and fixed-input tests.
3. P0.3. `telemetry_sources`, token auth, the three routes (logs stored, traces and metrics return 200 with
   all items in `partial_success` until their phase), guards, and counters.
4. P0.4. Load test: a script sends OTLP logs at a fixed rate to SQLite and Postgres and reports stored rows
   per second, p95 request time, and search time over 1, 10, and 50 million rows. Set the defaults in 4.6
   from the result.
5. P1. Logs pages, search parser, live tail, retention. Error to log links.
6. P2. Spans, rollups, service and trace pages, waterfall, `events.trace_id`.
7. P3. Metrics tables, explorer, saved charts.
8. P4. Telemetry monitors.

## 11. Tests

- `crates/telemetry`: each signal has fixed protobuf and JSON inputs and literal expected rows. This includes
  severity mapping, resource fallbacks, attribute limits, scrubbing, cumulative counter resets, and
  histogram buckets.
- The search parser: input strings and the expected filter; bad input and the expected error.
- Integration tests in `apps/server/tests/telemetry.rs`: an OTLP request with a real token is stored; a
  wrong token is 401; another workspace gets 404; quota gives `partial_success` and then 429; a repeated span
  is stored one time; retention deletes old rows and keeps the rollups.
- The same integration tests run on Postgres when `OWLEE_APP_POSTGRES_URL` is set, as the other suites do.
- An end-to-end check with the real OpenTelemetry Collector (`otlphttp` exporter) in CI, at least for logs
  and traces.

## 12. Limits

| Limit | Value |
|---|---|
| Body | 4 MiB after decompression |
| Attributes for each record | 64 |
| Attribute key | 128 characters |
| Attribute value | 1 KiB |
| Log body | 16 KiB |
| Span events for each span | 32 |
| Operations for each service in the rollup | 200 |
| Series for each source | 10,000 |
| Search time range | 7 days |
| Rows for each search page | 200 |
| Points for each chart line | 300 |

## 13. Open questions

1. Should a source and an error project be one object, so that one setup gives one DSN and one OTLP token?
   This design keeps them apart (D18). The setup page can make both in one step.
2. What load must one SQLite file take? P0.4 answers this. If the result is too low, the fallback is to
   require Postgres for telemetry, not to add another engine.
3. Should Owlee accept Sentry SDK transactions as traces? Many error users already send them. It would give
   traces with no new SDK. This design leaves it out of P2.

## 14. Decisions

| # | Decision | Date |
|---|---|---|
| D1 | The three signals come over OTLP only, from OpenTelemetry SDKs and the Collector. Owlee has no SDK or agent of its own for them. | 2026-11-19 |
| D2 | OTLP/HTTP only, no gRPC. One axum server on one port is simpler, and every SDK and the Collector support OTLP/HTTP. | 2026-11-19 |
| D3 | No Datadog intake. DataCat supports it, but it is a second protocol to maintain, and OTLP covers the same senders through the Collector. | 2026-11-19 |
| D4 | Telemetry is stored in the same SQLite or Postgres database, with the same SQL. No ClickHouse. If SQLite is too slow (P0.4), the answer is Postgres. | 2026-11-19 |
| D5 | A source has one bearer token, stored as a SHA-256 hash and shown one time. It is not a public key like a DSN, because OTLP senders are servers, not browsers. | 2026-11-19 |
| D6 | Intake paths are `/otlp/v1/{logs,traces,metrics}`, so `OTEL_EXPORTER_OTLP_ENDPOINT` is `https://<host>/otlp`. | 2026-11-19 |
| D7 | Decode with `opentelemetry-proto` (messages and serde only). If P0.1 shows the cost is too high, use `prost` with committed generated code. | 2026-11-19 |
| D8 | Normalization lives in a new pure crate `crates/telemetry`, as `crates/errors` does for Sentry. HTTP and storage stay in `apps/server`. | 2026-11-19 |
| D9 | No JSON functions in SQL. Filterable attributes go into key-value tables; the full set is JSON text for display. | 2026-11-19 |
| D10 | Telemetry is not part of the per-user privacy export and erasure. It is deleted by source and by retention. Bodies and attributes are scrubbed at intake. | 2026-11-19 |
| D11 | The request waits for the commit. No queue in memory, so a crash never loses data that the client was told is stored. | 2026-11-19 |
| D12 | One small search language for logs, traces, and metric filters, parsed into a typed filter and bound into SQL. Text search is `LIKE`, not full-text search, because SQLite FTS5 and Postgres `tsvector` need different SQL. | 2026-11-19 |
| D13 | Metrics P3 supports gauges, sums, and explicit histograms. Exponential histograms and summaries are dropped with a reason, and can come later. | 2026-11-19 |
| D14 | Live tail is polling every 2 seconds, because Owlee has no realtime layer. | 2026-11-19 |
| D15 | No multi-widget dashboards. The metrics explorer has one chart for each query, the state is in the URL, and a project can save charts. | 2026-11-19 |
| D16 | No host inventory and no Owlee host agent. Host metrics come from the Collector `hostmetrics` receiver. | 2026-11-19 |
| D17 | Alerts on telemetry are monitors with new job types, so incidents, maintenance, status pages, and notifications work with no new code paths. | 2026-11-19 |
| D18 | A telemetry source is separate from an error project. Errors and telemetry connect by `trace_id`, not by the object that received them. | 2026-11-19 |
| D19 | Order: logs, then traces, then metrics, then alerts. Logs are the simplest and pair at once with errors. Traces reuse the log foundations. Metrics need the most new logic (temporality, cardinality, rollups). | 2026-11-19 |
