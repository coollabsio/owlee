-- One SQL file for SQLite and Postgres.
-- A link hostname redirects only after its TXT record proves ownership,
-- the same record a site domain uses.
ALTER TABLE insight_domains ADD COLUMN verified BIGINT NOT NULL DEFAULT 0;
ALTER TABLE insight_domains ADD COLUMN verify_token TEXT NOT NULL DEFAULT '';
ALTER TABLE insight_domains ADD COLUMN verified_at BIGINT;
