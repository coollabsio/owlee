# Changelog

All notable changes to Owlee are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-11-21

The first release. From this version on, each release migrates the database forward when it starts. A binary refuses a database that a newer release migrated, so back up before you upgrade. See [Upgrading](README.md#upgrading).

### Added

- HTTP and TCP monitors with an interval, a timeout, an expected status, a body assertion, and a latency limit that marks a check degraded. Repeated failures open an incident, and a later success resolves it.
- Public status pages with components, 90-day uptime bars, and a banner. Incidents follow the investigating, identified, monitoring, and resolved timeline. Checks inside a maintenance window do not count as downtime.
- Notifications by email (SMTP), signed webhooks, Slack, and Discord.
- Agents that run HTTP, TCP, Postgres, MySQL, and Redis checks inside private networks. The checks come from environment variables or a config file on the agent host. When an agent is silent for 2 minutes, its monitors go down with an incident that names the agent, and they recover when the agent reports again.
- Cookieless web analytics with a `/data.js` snippet, custom events, Core Web Vitals, and public share pages.
- A link shortener on your own hostnames and on `/go/{slug}`, with click counts and referrers.
- Error tracking that accepts events from the official Sentry SDKs, with issues grouped on a fingerprint.
- Notifications for a new error issue and for a regression, on channels that opt in.
- Retries for failed notification deliveries.
- Paging for the incident, error issue, and error event lists.
- Account management without SMTP. A workspace invite link from an installation admin can create the account. An invite from anyone else needs an existing account. An installation admin can create, disable, enable, and delete accounts and set passwords in Admin > Users. `owlee user` does the same on the host and can reset the root password.
- `owlee backup <path>`, which copies the SQLite database while Owlee runs. On Postgres it points to `pg_dump`.
- `owlee db`, which opens a `sqlite3` or `psql` shell on the configured database. The Docker image includes both clients.
- Incremental auto-vacuum for new SQLite databases. The hourly retention pass now returns freed pages to the file system.
- `OWLEE_CHECK_RETENTION_DAYS` sets how long monitor check results are kept. The default is 14 days.
- Security headers on every response, including a `Content-Security-Policy` and `X-Frame-Options: DENY`. Status pages and share pages can still be embedded.
- CI runs a static musl build. A separate audit workflow runs `cargo deny` and `bun audit` on each push and weekly, and does not block a release. Dependabot watches Cargo, Bun, GitHub Actions, and Docker images.

### Changed

- An unset `OWLEE_ENV` now means production. Production needs `OWLEE_APP_KEY` and `OWLEE_PUBLIC_ORIGIN`, and `owlee serve` refuses an origin that is not a bare `http://` or `https://` origin.
- `owlee seed` refuses to run in production.
- Owlee encrypts notification channel settings and monitor request headers and bodies with `OWLEE_APP_KEY`, as it already did for the SMTP password. The API masks channel secrets. A blank header value or body keeps the saved one only while the monitor URL stays on the same origin.
- Slack and Discord alerts no longer ping `@everyone`, `@here`, `<!channel>`, or users named in monitor names or error titles.
- Release binaries and container images are built only after the CI tests pass. A `v*` tag publishes the image as `0.1.0`, `0.1`, and `latest`. A push to `main` publishes `main` and the commit SHA, not `latest`.
- The Dockerfile pins its base images to exact versions.

[Unreleased]: https://github.com/coollabsio/owlee/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/coollabsio/owlee/releases/tag/v0.1.0
