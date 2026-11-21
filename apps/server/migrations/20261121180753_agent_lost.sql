-- One SQL file for SQLite and Postgres.
-- Set while a monitor is down because its agent stopped reporting, not because a check failed.
ALTER TABLE monitors ADD COLUMN agent_lost_at BIGINT;
