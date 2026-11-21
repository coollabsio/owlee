-- One SQL file for SQLite and Postgres.
-- A disabled account keeps its data and memberships but cannot sign in.
ALTER TABLE users ADD COLUMN disabled_at BIGINT;
