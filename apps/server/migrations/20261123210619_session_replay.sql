-- One SQL file for SQLite and Postgres.
-- Session replay: per-app switch and caps, replays, their segments, URLs, user hashes, and usage counters.

ALTER TABLE error_apps ADD COLUMN replays_enabled BIGINT NOT NULL DEFAULT 0;
-- 512 MiB of compressed segments per UTC day.
ALTER TABLE error_apps ADD COLUMN replay_daily_bytes BIGINT NOT NULL DEFAULT 536870912;
-- New replays per UTC hour.
ALTER TABLE error_apps ADD COLUMN replay_hourly_limit BIGINT NOT NULL DEFAULT 1000;

-- replay_id is 32 lowercase hex chars chosen by the SDK. Times are Unix milliseconds.
-- started_ms and last_ms come from the client clock, clamped. received_ms and last_received_ms are the server clock.
CREATE TABLE replays (
    app_id TEXT NOT NULL REFERENCES error_apps (id) ON DELETE CASCADE,
    replay_id TEXT NOT NULL,
    replay_type TEXT NOT NULL,
    has_start BIGINT NOT NULL,
    started_ms BIGINT,
    last_ms BIGINT NOT NULL,
    received_ms BIGINT NOT NULL,
    last_received_ms BIGINT NOT NULL,
    segment_count BIGINT NOT NULL,
    bytes BIGINT NOT NULL,
    first_url TEXT,
    url_count BIGINT NOT NULL,
    release TEXT,
    environment TEXT,
    browser TEXT,
    os TEXT,
    user_label TEXT,
    PRIMARY KEY (app_id, replay_id)
);
CREATE INDEX replays_app_received ON replays (app_id, received_ms);
CREATE INDEX replays_last_received ON replays (last_received_ms);

-- BYTEA is binary on Postgres. SQLite gives the name NUMERIC affinity, which stores blobs unchanged.
CREATE TABLE replay_segments (
    app_id TEXT NOT NULL,
    replay_id TEXT NOT NULL,
    segment_id BIGINT NOT NULL,
    time_ms BIGINT NOT NULL,
    encoding TEXT NOT NULL,
    data BYTEA NOT NULL,
    PRIMARY KEY (app_id, replay_id, segment_id),
    FOREIGN KEY (app_id, replay_id) REFERENCES replays (app_id, replay_id) ON DELETE CASCADE
);

CREATE TABLE replay_urls (
    app_id TEXT NOT NULL,
    replay_id TEXT NOT NULL,
    url TEXT NOT NULL,
    PRIMARY KEY (app_id, replay_id, url),
    FOREIGN KEY (app_id, replay_id) REFERENCES replays (app_id, replay_id) ON DELETE CASCADE
);

-- Same shape and same keyed hash as event_users.
CREATE TABLE replay_users (
    user_hash TEXT NOT NULL,
    app_id TEXT NOT NULL,
    replay_id TEXT NOT NULL,
    PRIMARY KEY (user_hash, app_id, replay_id),
    FOREIGN KEY (app_id, replay_id) REFERENCES replays (app_id, replay_id) ON DELETE CASCADE
);
CREATE INDEX replay_users_replay ON replay_users (app_id, replay_id);

-- day is the Unix second of the UTC day start.
CREATE TABLE replay_daily_usage (
    app_id TEXT NOT NULL REFERENCES error_apps (id) ON DELETE CASCADE,
    day BIGINT NOT NULL,
    bytes BIGINT NOT NULL,
    PRIMARY KEY (app_id, day)
);

-- hour is the Unix second of the UTC hour start.
CREATE TABLE replay_hourly_usage (
    app_id TEXT NOT NULL REFERENCES error_apps (id) ON DELETE CASCADE,
    hour BIGINT NOT NULL,
    replays BIGINT NOT NULL,
    PRIMARY KEY (app_id, hour)
);

ALTER TABLE events ADD COLUMN replay_id TEXT;
CREATE INDEX events_app_replay ON events (app_id, replay_id);
