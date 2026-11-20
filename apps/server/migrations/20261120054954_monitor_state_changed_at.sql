-- One SQL file for SQLite and Postgres.
-- When the shown state last changed: engine state, or pause and resume.
ALTER TABLE monitors ADD COLUMN state_changed_at BIGINT NOT NULL DEFAULT 0;
UPDATE monitors SET state_changed_at = created_at;
