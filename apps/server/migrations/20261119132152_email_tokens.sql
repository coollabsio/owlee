-- One SQL file for SQLite and Postgres.
-- Single-use links for email-verified registration and password recovery.

CREATE TABLE email_tokens (
    token_hash TEXT PRIMARY KEY,
    purpose TEXT NOT NULL CHECK (purpose IN ('register', 'recovery')),
    -- Normalized email.
    email TEXT NOT NULL,
    -- Set for recovery.
    user_id TEXT REFERENCES users (id) ON DELETE CASCADE,
    expires_at BIGINT NOT NULL,
    used_at BIGINT,
    created_at BIGINT NOT NULL
);

CREATE INDEX email_tokens_expires ON email_tokens (expires_at);
