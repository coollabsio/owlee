-- One SQL file for SQLite and Postgres.
-- Stored user keys were plain ids, emails, and IP addresses. Boot recomputes
-- each NULL key as a keyed hash and scrubs that event's payload.
UPDATE events SET user_key = NULL;

-- Retention deletes these by time.
CREATE INDEX insight_visits_last_at ON insight_visits (last_at);
CREATE INDEX insight_site_sessions_day ON insight_site_sessions (day);
CREATE INDEX insight_link_sessions_day ON insight_link_sessions (day);

-- One keyed hash per identity on an event (id, email, username, address), so a
-- privacy request finds the event by any of them.
CREATE TABLE event_users (
    user_hash TEXT NOT NULL,
    event_id TEXT NOT NULL REFERENCES events (id) ON DELETE CASCADE,
    PRIMARY KEY (user_hash, event_id)
);

CREATE INDEX event_users_event ON event_users (event_id);
