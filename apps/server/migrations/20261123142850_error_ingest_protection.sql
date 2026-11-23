-- One SQL file for SQLite and Postgres.
-- Error ingest protection: allowed domains and inbound filters.

-- One host per line. Empty allows every origin.
ALTER TABLE error_apps ADD COLUMN allowed_domains TEXT NOT NULL DEFAULT '';
-- A comma list of inbound filter ids.
ALTER TABLE error_apps ADD COLUMN inbound_filters TEXT NOT NULL DEFAULT 'browser_extensions,web_crawlers';

