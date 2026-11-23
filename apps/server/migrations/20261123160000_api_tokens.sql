-- One SQL file for SQLite and Postgres.
-- Personal API tokens. A token acts as its user in one workspace, limited to its scopes.

CREATE TABLE api_tokens (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    token_prefix TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    -- JSON array of scope strings such as "errors:read".
    scopes_json TEXT NOT NULL,
    -- NULL: the token reaches all resources. Else {"projects": [id], "error_apps": [id]}.
    resources_json TEXT,
    created_at BIGINT NOT NULL,
    last_used_at BIGINT,
    expires_at BIGINT,
    -- NULL: the token works. Else the time its user paused it; it answers 401 until resumed.
    paused_at BIGINT,
    revoked_at BIGINT
);

CREATE INDEX api_tokens_owner ON api_tokens (workspace_id, user_id, revoked_at);
