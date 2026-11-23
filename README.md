# Owlee

Owlee is a self-hosted uptime monitor, status page, incident desk, error tracker, and cookieless web analytics tool from [coolLabs](https://github.com/coollabsio), the team behind [Coolify](https://github.com/coollabsio/coolify). One Rust binary serves the API, runs the checks, and hosts the React app. SQLite is the default database. Postgres is the same schema and the same queries, selected with a URL.

> [!WARNING]
> Owlee is early software, and 0.1.0 is its first release. Do not use it for systems where a failure can cause a large financial loss. From 0.1.0 on, each release migrates the database forward when it starts. There is no downgrade. A binary refuses to start on a database that a newer release migrated. Back up before you upgrade, and pin a version tag such as `ghcr.io/coollabsio/owlee:0.1.0` instead of `latest`. The API and the configuration can still change between releases. [CHANGELOG.md](CHANGELOG.md) lists each change, and [Upgrading](#upgrading) has the steps.

![Dashboard](docs/screenshots/dashboard.png)

![Public status page](docs/screenshots/status-page.png)

## What it does

- HTTP and TCP monitors with an interval, a timeout, an expected status, a body assertion, and a latency line that marks a check degraded.
- A scheduler in the API process. Repeated failures move a monitor to down and open an incident. A later success resolves that automatic incident.
- Public status pages on a slug (`/status/coollabs`) with components, 90-day uptime bars, and a banner.
- Incidents with the OpenStatus timeline: investigating, identified, monitoring, resolved.
- Maintenance windows. Checks inside a window are stored and are not counted as downtime.
- Notifications. Email (SMTP) and signed webhooks work now. Slack and Discord are incoming-webhook channels on the same interface.
- Agents for private networks. An agent runs checks on targets that only a VPC or Docker network can reach, such as an internal database.
- Cookieless web analytics. A site gets a public id and a `/data.js` snippet. Page views, custom events, and Core Web Vitals are grouped without cookies. The client address is hashed into a visitor id and is not stored.
- A link shortener. Links live on a hostname you add, and also at `/go/{slug}` on this server, with click counts, referrers, and an optional public page.
- Error tracking. Create a project, copy its Sentry DSN, and point an official Sentry SDK at it. Issues group on a fingerprint, with a short id such as `CHECKOUT-1`, stack traces, breadcrumbs, tags, and the raw event.

## Planned

These features are not built yet. The idea comes from [DataCat](https://github.com/replit/datacat). The design is in [docs/specs/2026-11-19-observability-design.md](docs/specs/2026-11-19-observability-design.md).

- Metrics. Receive OpenTelemetry (OTLP/HTTP) metrics, show charts, and alert when a value crosses a line.
- Logs. Receive OTLP logs, search and filter them, show a live tail, and link a log line to its trace and error.
- Traces. Receive OTLP traces, show a service list and a span waterfall, and link a trace to its error issue.

## Run it

You need Rust 1.99 (see `rust-toolchain.toml`) and [Bun](https://bun.sh) 1.2 or newer.

```bash
cd apps/web && bun install && bun run build
export OWLEE_ENV=development   # unset means production
cargo run -p owlee-server -- seed
cargo run -p owlee-server -- serve --listen 127.0.0.1:43123
```

Open http://127.0.0.1:43123.

`seed` creates a local demo and does nothing if an account already exists. It also marks setup complete, so a seeded database does not print a setup URL:

- email `test@example.com`
- password `password`
- a coolLabs status page, a demo analytics site (`coolify.io`, public id `pub_coollabs_demo`), short links at `/go/coolify` and `/go/docs`, and demo error projects (`Checkout`, `API`) with sample events

`just dev` runs `seed` before it starts the server. `just` recipes set `OWLEE_ENV=development` unless it is already set. `seed` refuses to run in production, which is also the mode when `OWLEE_ENV` is unset. A fresh database uses the setup URL below, not this password.

The database file is `data/owlee.sqlite`. The Docker default is `/var/lib/owlee/owlee.sqlite`. A custom `OWLEE_DATABASE_PATH` is used as written. Migrations run on startup.

Without Bun on the PATH, `cargo test` still works. `apps/server/build.rs` writes a placeholder page when `apps/web/dist` is missing, so the binary compiles before the UI is built.

### Postgres

Leave the SQLite path in place (it is unused for data) and set `OWLEE_DATABASE_URL`.

```bash
export OWLEE_DATABASE_URL=postgres://owlee:owlee@127.0.0.1:5432/owlee
cargo run -p owlee-server -- serve --listen 127.0.0.1:43123
```

The URL must start with `postgres://` or `postgresql://`. Integer columns are `BIGINT` so one SQL file decodes as `i64` on both engines. A migration is applied once, inside a transaction, and its SHA-256 is stored in `schema_migrations`. A later binary with different SQL for that version refuses to start. Make a new migration with `just new-migration <name>`. It makes `apps/server/migrations/<UTC timestamp>_<name>.sql`, so parallel branches do not collide. `apps/server/build.rs` finds the files and refuses a version that is not a timestamp or a duplicate version. Do not edit a migration after it is merged.

Copy `.env.example` to `.env.local` if you want the variables written down. The process reads the environment directly. `just dev` sources `.env.local` when that file exists.

Email delivery needs SMTP. Without it, an email channel is saved and each send is recorded as skipped:

```bash
export OWLEE_SMTP_HOST=smtp.example.com
export OWLEE_SMTP_PORT=587
export OWLEE_SMTP_FROM=status@example.com
```

Webhook channels can set a secret. The body is posted as JSON with `X-Owlee-Signature: sha256=<hmac-sha256>`.

In production, webhook, Slack, and Discord channels refuse loopback, private, link-local, and other non-public addresses. Owlee checks the address that it connects to, after DNS, and does not follow redirects. To send to a receiver on your own network, set `OWLEE_ALLOW_PRIVATE_WEBHOOKS=true`. Development allows private addresses. Monitors are not affected.

### First-run setup

A new database has no open sign-up. The login page does not offer "Create one". On first start the server prints one line to stderr (standard error, not the JSON logs):

```text
Initial setup URL: http://127.0.0.1:43123/setup?token=...
```

Open that URL and create the owner account. That account is the root user. Do not keep a copy of the token. It expires after 30 minutes, and a later start does not print it again while it is still valid. After the owner exists, setup cannot run again and public registration stays closed.

If the token is lost or expired before use, replace it. On SQLite, stop Owlee first (the server owns the database file). On Postgres the command can run while Owlee is up:

```bash
owlee setup-token rotate
# optional: owlee setup-token rotate --origin https://status.example.com
```

`setup-token init` prints a URL only when setup is unfinished and no unused token is stored. Rotation invalidates the previous unused token.

The root user opens registration in Admin. It stays closed until a mail server is saved there, or `OWLEE_SMTP_HOST` and `OWLEE_SMTP_FROM` are set, and the root user turns Open registration on. Clearing the saved mail server closes registration again. `OWLEE_OPEN_SIGNUP` is ignored. A person who opens `/register` enters an email address, and Owlee mails a link that works once, for 24 hours. The link opens a page where they choose a password. If the address already has an account, the mail says so instead, and the page shows the same message in both cases. `/forgot-password` mails a reset link that works once, for 1 hour, and the reset signs out every session of that account. Without a mail server, no reset mail is sent and the server logs a warning. Each address gets at most about 3 of these mails an hour.

New accounts are members. A member can read everything but cannot create, change, or delete anything, and cannot see notification channels, which hold webhook URLs and tokens. The root user can mark another account as an installation admin, which can change everything except the instance settings and other installation admins. Every account can change its own password in Account settings, which signs out its other sessions, and can sign out any of its sessions there.

Without a mail server or open registration, add people in either of two ways. When an installation admin sends a workspace invite to an address with no account, the link lets the person choose a name and password, which creates a member account and joins the workspace. The link works once. An invite from a workspace owner who is not an installation admin needs an existing account, so the person registers first or asks an admin. In Admin > Users, an installation admin can create an account with an email and password, disable or enable it, set a new password, or delete it. Disabling and setting a password sign the account out everywhere. A disabled account keeps its data and memberships but cannot sign in. An account that owns a workspace cannot be deleted, so disable it instead. Only the root user can change another installation admin, and the root account itself cannot be changed. On the host, `owlee user` creates, disables, and enables accounts and sets passwords. On SQLite, stop Owlee first:

```bash
owlee user create --email ada@example.com --name Ada    # asks for the password
echo "$PASSWORD" | owlee user set-password --email ada@example.com --password-stdin
owlee user disable --email ada@example.com
owlee user enable --email ada@example.com
```

`owlee user set-password` also works on the root account, so a lost root password can be reset without mail.

Requests that change data with the session cookie must come from `OWLEE_PUBLIC_ORIGIN`. A `POST`, `PUT`, `PATCH`, or `DELETE` with another `Origin`, or with no `Origin` and `Sec-Fetch-Site: cross-site`, gets `403`. Open the dashboard at exactly that origin (`127.0.0.1` and `localhost` are different origins). Error ingest, `POST /api/receive`, and the agent routes are not checked. The `403` names the origin it received and the one it expected. When `OWLEE_PUBLIC_ORIGIN` is `https://`, the session cookie is `__Host-owlee_session`, otherwise `owlee_session`. It lasts at most 90 days.

In production, `owlee serve` does not start without `OWLEE_PUBLIC_ORIGIN`. Any command refuses a value that is not a bare `http://` or `https://` origin, such as one with a path. A loopback host or plain `http://` starts with a warning, for a test machine or a LAN install.

Every response sends `Content-Security-Policy`, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`, `Cross-Origin-Opener-Policy: same-origin`, and a `Permissions-Policy` that turns off the camera, microphone, geolocation, and payments. Scripts, styles, images, and fonts load only from Owlee's origin. Public status pages (`/status/{slug}`) and share pages (`/share/...`) can be embedded in an iframe on any site. Every other page refuses framing. Owlee does not send `Strict-Transport-Security`. Set it at the TLS proxy if you want it.

Secrets saved in the database are encrypted with `OWLEE_APP_KEY`, base64 of 32 bytes (`openssl rand -base64 32`). Production does not start without it. Development uses a fixed public key and logs a warning. Keep the key with your backups. With another key, Owlee cannot read the saved SMTP password; it logs a warning, and the root user must enter the password again in Admin. A password saved by an older Owlee in plain text is treated the same way.

When a check cannot connect (DNS, connect, or timeout error), Owlee first tries a TCP connection to `1.1.1.1:443`, `8.8.8.8:443` and `9.9.9.9:443`. If none of them answer, the problem is the Owlee host's own network. Owlee then does not record the check, does not change the monitor state, and sends no alert. The result is cached for 10 seconds. If the host cannot reach the internet by design, set hosts that it can always reach, or turn the check off:

```bash
export OWLEE_CONNECTIVITY_TARGETS=10.0.0.1:53,10.0.0.2:53   # or "off"
```

## Development

```bash
just setup
just dev      # API on 127.0.0.1:43124, Vite on 127.0.0.1:43123
just test
just build    # release binary with the UI embedded
```

`just` is optional. The same commands are `bun install --frozen-lockfile` in `apps/web`, `cargo test --workspace`, and `cargo build --release -p owlee-server` after `bun run build`.

Frontend dev proxies `/api` to the Rust process. Production does not proxy: the binary serves both.

## Tests

```bash
cargo test --workspace
```

Domain tests cover assertions, the failure threshold, uptime math (maintenance is excluded), slugs, and incident resolution. Server tests create the owner with a setup token, run an HTTP check against a local server, open an incident after a failure, publish a status page, skip a check during maintenance, deliver a signed webhook, skip checks when the host is offline, and sync agent checks. Other server tests cover setup tokens, registration and SMTP settings, read-only members, email links for registration and password reset, sessions, the origin check, error ingest, and analytics with short links.

Postgres tests run only when these are set:

```bash
export OWLEE_TEST_POSTGRES_URL=postgres://status:status@127.0.0.1:5432/status_platform
export OWLEE_APP_POSTGRES_URL=postgres://status:status@127.0.0.1:5432/status_app
```

`OWLEE_TEST_POSTGRES_URL` runs the migration runner test in `crates/platform`. `OWLEE_APP_POSTGRES_URL` starts the server on Postgres in the `flow`, `errors`, and `insights` tests. Use two empty databases. The platform test resets its own schema.

## Docker

```bash
docker build -t owlee:local .
export OWLEE_APP_KEY="$(openssl rand -base64 32)"   # once; store it with your backups
docker run -d --name owlee -p 8080:8080 -v owlee-data:/var/lib/owlee \
  -e OWLEE_APP_KEY="$OWLEE_APP_KEY" \
  -e OWLEE_PUBLIC_ORIGIN=http://localhost:8080 owlee:local
```

The image runs in production mode, so it needs `OWLEE_APP_KEY` and `OWLEE_PUBLIC_ORIGIN`. A plain `http://` origin starts with a warning, because the session cookie is not `Secure`.

`.github/workflows/image.yml` runs the CI tests, then publishes `ghcr.io/coollabsio/owlee` for `linux/amd64` and `linux/arm64`. A push to `main` publishes the tags `main` and the seven-character commit SHA. A release tag such as `v0.1.0` publishes `0.1.0`, `0.1`, and `latest`. A pre-release tag such as `v0.2.0-rc.1` publishes only `0.2.0-rc.1`. So `latest` is the newest stable release, not the tip of `main`. The same workflow can be run by hand from the Actions tab. If the package is not linked to this repository automatically, link it from the package settings.

To run the published image, pin a version:

```bash
docker run -d --name owlee --restart unless-stopped -p 8080:8080 -v owlee-data:/var/lib/owlee \
  -e OWLEE_APP_KEY="$OWLEE_APP_KEY" \
  -e OWLEE_PUBLIC_ORIGIN=https://status.example.com \
  ghcr.io/coollabsio/owlee:0.1.0
```

The image listens on `0.0.0.0:8080`, stores SQLite in `/var/lib/owlee`, and uses `owlee healthcheck` for the container check. `deploy/compose.yaml` is the same shape as Orbit's compose file, with Postgres left as a commented service. It refuses to start until `OWLEE_APP_KEY` and `OWLEE_PUBLIC_ORIGIN` are set, in the shell or in `deploy/.env`. Without `OWLEE_IMAGE`, it builds `owlee:local` from the repository. To use the published image instead, run these commands from the repository root:

```bash
export OWLEE_APP_KEY="$(openssl rand -base64 32)"   # once; store it with your backups
export OWLEE_PUBLIC_ORIGIN=https://status.example.com
export OWLEE_IMAGE=ghcr.io/coollabsio/owlee:0.1.0
docker compose -f deploy/compose.yaml pull owlee
docker compose -f deploy/compose.yaml up -d --no-build
```

On a fresh volume the container prints `Initial setup URL` to stderr. Read it from the container logs (`docker logs` or `docker compose logs owlee`), open it in a browser, and create the owner. Set `OWLEE_PUBLIC_ORIGIN` to the URL you will open before the first start so the printed link uses that host. The token is not stored in the image.

If you miss the URL, stop the container (required for the default SQLite volume; leave Owlee running when `OWLEE_DATABASE_URL` is Postgres) and issue another:

```bash
docker stop owlee
docker run --rm -v owlee-data:/var/lib/owlee \
  -e OWLEE_PUBLIC_ORIGIN=https://status.example.com \
  -e OWLEE_APP_KEY="$OWLEE_APP_KEY" \
  ghcr.io/coollabsio/owlee:0.1.0 setup-token rotate
```

With Compose, from the repository root, with the same variables set:

```bash
docker compose -f deploy/compose.yaml stop owlee
docker compose -f deploy/compose.yaml run --rm owlee setup-token rotate
docker compose -f deploy/compose.yaml start owlee
```

## Back up and restore

`owlee backup <path>` copies the SQLite database to a new file while Owlee keeps running. It uses SQLite's `VACUUM INTO`, which reads one consistent snapshot, including writes that are still in the `-wal` file. It refuses to replace an existing file, and it prints the size of the copy.

```bash
owlee backup /var/backups/owlee-$(date +%Y%m%d-%H%M%S).sqlite
```

In Docker, write the copy inside the volume, then copy it out of the container:

```bash
name="backup-$(date +%Y%m%d-%H%M%S).sqlite"
docker exec owlee /owlee backup "/var/lib/owlee/$name"
docker cp "owlee:/var/lib/owlee/$name" .
docker exec owlee rm "/var/lib/owlee/$name"
```

With Compose, use `docker compose -f deploy/compose.yaml exec owlee /owlee backup ...` for the second line.

Keep `OWLEE_APP_KEY` with every backup. Owlee encrypts the saved SMTP password, the notification channel settings, and the monitor request headers and bodies with that key. With another key, Owlee cannot read them. It logs a warning, and someone must enter them again.

To restore a SQLite backup:

1. Stop Owlee.
2. Replace `owlee.sqlite` with the backup file.
3. Delete `owlee.sqlite-wal` and `owlee.sqlite-shm` if they exist. They belong to the replaced file.
4. Start Owlee with the same `OWLEE_APP_KEY`.

In Docker, a short-lived container with the same volume does steps 2 and 3. It runs as the image's `owlee` user, so the restored file keeps the right owner. Run it from the directory that holds `backup.sqlite`:

```bash
docker stop owlee
docker run --rm -v owlee-data:/var/lib/owlee -v "$PWD:/restore:ro" --entrypoint sh \
  ghcr.io/coollabsio/owlee:0.1.0 -c \
  'cp /restore/backup.sqlite /var/lib/owlee/owlee.sqlite && rm -f /var/lib/owlee/owlee.sqlite-wal /var/lib/owlee/owlee.sqlite-shm'
docker start owlee
```

On Postgres, `owlee backup` exits with an error that names `pg_dump`. Use the Postgres tools. Stop Owlee before a restore:

```bash
pg_dump --format=custom --file=owlee.dump "$OWLEE_DATABASE_URL"
pg_restore --clean --if-exists --no-owner --dbname="$OWLEE_DATABASE_URL" owlee.dump
```

### SQL shell

`owlee db` opens the configured database in an interactive shell: `sqlite3` for SQLite, `psql` for Postgres. The Docker image has both clients. The Postgres password goes to `psql` in `PGPASSWORD`, not on the command line. Changes that you make in the shell skip all of Owlee's checks. Make a backup first.

```bash
docker exec -it owlee /owlee db
docker compose -f deploy/compose.yaml exec owlee /owlee db
```

### Disk space

Every hour, Owlee deletes monitor check results older than `OWLEE_CHECK_RETENTION_DAYS` (14, at least 1), and error and analytics rows older than their own retention settings. After that pass, SQLite returns the freed pages to the file system with `PRAGMA incremental_vacuum`, so the file shrinks. This works on a database file that Owlee 0.1.0 or later created. An older file keeps its size and reuses the free pages for new rows. On Postgres, autovacuum makes the space of deleted rows reusable.

## Upgrading

1. Read [CHANGELOG.md](CHANGELOG.md) for every release between your version and the new one.
2. Back up the database with `owlee backup` or `pg_dump`, and keep `OWLEE_APP_KEY` with the backup.
3. Change the pinned tag, for example from `ghcr.io/coollabsio/owlee:0.1.0` to the new version, and start the new container. Migrations run when Owlee starts.

Each migration runs once, inside a transaction. If a migration fails, Owlee does not start, and the database stays at the last migration that worked. To go back to an older release, stop Owlee, restore the backup from step 2, and start the older tag. An older binary refuses a database that a newer one migrated, with `database schema version ... is newer than this binary`. Data written after the upgrade is lost when you restore the backup.

## Agents

An agent checks targets that the Owlee server cannot reach, for example a database on a private Docker network or a host in a VPC. The agent is the same binary. You start it on the target server or in the same network. It makes only outbound HTTPS requests to the server, so you do not need to open a port into the private network.

The checks are in environment variables or a config file on the agent host. The server cannot add or change agent checks, so a compromised dashboard cannot make an agent connect to other hosts. Passwords stay on the agent host. The agent judges each check and sends only the status, the status code, the latency, and the error. It never sends a response body.

1. In the web app, open **Agents** and create an agent. Copy the token. The token is shown once.
2. Start the agent. Set each check as `OWLEE_AGENT_CHECK_<n>_<KEY>` environment variables, where `<n>` is a number and `<KEY>` is a key from the table in upper case:

   ```bash
   docker run -d --name owlee-agent --restart unless-stopped --no-healthcheck \
     -e OWLEE_AGENT_SERVER=https://status.example.com \
     -e OWLEE_AGENT_TOKEN=<token> \
     -e OWLEE_AGENT_CHECK_1_NAME="Main DB" \
     -e OWLEE_AGENT_CHECK_1_TYPE=postgres \
     -e OWLEE_AGENT_CHECK_1_TARGET=localhost:5432 \
     -e OWLEE_AGENT_CHECK_1_USER=status \
     -e OWLEE_AGENT_CHECK_1_PASSWORD=... \
     -e OWLEE_AGENT_CHECK_1_DATABASE=app \
     -e OWLEE_AGENT_CHECK_1_INTERVAL=30 \
     ghcr.io/coollabsio/owlee:0.1.0 agent
   ```

   Attach the container to the network of the targets. For more checks, use `OWLEE_AGENT_CHECK_2_...`, `OWLEE_AGENT_CHECK_3_...`, and so on. Without Docker, download the static Linux binary with `curl -fsSL -o owlee https://github.com/coollabsio/owlee/releases/latest/download/owlee-$(uname -m)-linux && chmod +x owlee`, set the same variables, and run `./owlee agent --server https://status.example.com --token <token>`. Pushing a `v*` tag publishes these binaries (`.github/workflows/release.yml`).

   | Key | Types | Default |
   |---|---|---|
   | `name`, `type` (`http`, `tcp`, `postgres`, `mysql`, `redis`), `target` (a URL for http) | all | required |
   | `interval` (seconds, 15 or more), `timeout` (seconds), `failure_threshold` | all | 60, 10, 3 |
   | `degraded_after_ms` | all | none |
   | `user`, `password` (env only), `password_env`, `database` | postgres, mysql (`user` required); redis (`user` and password optional) | none |
   | `expected_status`, `body_contains` | http | 2xx, none |

   A `postgres` or `mysql` check logs in and runs `SELECT 1`. A `redis` check sends `AUTH` when a password is set, then `PING`. Redis over TLS is not supported.
3. Optional: put checks in a TOML file instead, and pass it with `--config` or `OWLEE_AGENT_CONFIG`. With Docker, mount the file. The agent uses the checks from the file and from the environment variables together.

   ```toml
   [[check]]
   name = "Main DB"
   type = "postgres"
   target = "localhost:5432"
   user = "status"
   password_env = "PG_PASSWORD" # the name of an environment variable, not the password
   database = "app"
   interval = 30
   ```

When the agent starts, it sends its list of checks to the server, without secrets. The server creates a monitor for each new check name, updates the monitors that already exist, and pauses the monitors of checks that are no longer in the config. History stays. To change a check, change the config and restart the agent. The agent stops at once when the server rejects the token or the config, and it shows the reason.

The server records each result with the agent name as the region, and opens or resolves incidents as it does for its own checks. In the dashboard you can pause, resume, and delete agent monitors, and add them to status pages, but you cannot edit them or run them. The agent sends a heartbeat every 15 seconds. It shows as offline when the server has not heard from it for 30 seconds. A restart of the agent or one missed heartbeat sends no alert. When the server has not heard from the agent for 2 minutes, each active monitor of the agent that is not in a maintenance window goes down, and Owlee opens an incident that names the agent and notifies your channels. Nothing checks these monitors during that time, so their real state is unknown. A monitor that was already down keeps its incident. When the agent reports again, the incident is resolved and the monitor is pending until its next result. The 2 minutes start again when the server restarts, because agents cannot report while the server is down. You can rename an agent in its details. Old checks keep the old name as their region. The agent uses `OWLEE_CONNECTIVITY_TARGETS` in the same way as the server: when its own host has no network, it does not send failed checks.

You cannot delete an agent while it has monitors.

## Errors

**Issues** is the feed (unresolved, resolved, ignored, search, environment, sparkline). Open an issue for the histogram, tags, event navigation, stack trace, breadcrumbs, contexts, and raw JSON. A new event reopens a resolved issue. **Projects** shows the DSN:

```text
http://<public_key>@127.0.0.1:43123/<project_id>
```

SDKs post to `/api/<project_id>/envelope/` and `/api/<project_id>/store/` (with or without the trailing slash). Auth is `X-Sentry-Auth`, `Authorization: Sentry sentry_key=...`, or `?sentry_key=`. Gzip and deflate bodies are accepted. Those routes answer CORS preflight and do not use the dashboard cookie. `/api/<project_id>/security/` is accepted and ignored. Performance items, sessions, and client reports in an envelope are ignored so the SDK still gets HTTP 200.

An envelope is rejected with `413` above `OWLEE_ERROR_MAX_BODY_BYTES` (1 MiB). Stack frames, breadcrumbs, tags, and messages inside an accepted event are truncated before they are stored. Values under secret-looking keys are replaced with `[Filtered]`. Bearer and basic credentials, JWTs, AWS access key ids, PEM private keys, and card numbers that pass the Luhn check are filtered in every string, the issue title included. The same project key and the same client IP each have a token bucket (`OWLEE_ERROR_RATE_PROJECT_*`, `OWLEE_ERROR_RATE_IP_*`). A full bucket or a full daily quota answers `429` with `Retry-After` and `X-Sentry-Rate-Limits`, which is what Sentry SDKs back off on. The IP bucket is checked before the body is read, so a wrong key or a large compressed body also uses it. A compressed body is rejected when it inflates past the same 1 MiB. One envelope stores at most one event, as in the Sentry envelope format. An event timestamp that is not a date, or that is more than one day ahead, is set to the receive time. The DSN key is public in browser code, so a person who copies it can use up the project's bucket and daily quota. Rotate the key if that happens. The client IP is the TCP peer. When that peer is in `OWLEE_TRUSTED_PROXIES`, it is the `X-Forwarded-For` entry selected by `OWLEE_PROXY_HOPS`. Every rate limit in Owlee groups an IPv6 client by its /64.

Issues still group on a fingerprint. `event_count` increases for every accepted event. The full payload is kept for the first `OWLEE_ERROR_KEEP_FIRST` events (5), then one more per `OWLEE_ERROR_SAMPLE_WINDOW_SECS` (1 hour). The issue row is the aggregate the feed shows. A background pass deletes raw `events` older than `OWLEE_ERROR_RETENTION_DAYS` (30) in batches of 500. The issue, including `event_count`, stays until it has no events left and its `last_seen` is older than the same cutoff. Then the same pass deletes it. `events(timestamp)` is indexed for that delete and for the 24-hour trend query.

Each project may accept `OWLEE_ERROR_DAILY_QUOTA` events per UTC day (100,000) until the root user sets another number under Admin → Ingest quotas, or the project sets its own cap. One error a second all day is about 86,400, so normal use does not reach 100,000. The projects list and the dashboard show when today's counter has hit that cap, and when it resets (00:00 UTC).

The daily cap panel of a project also shows how many events were stored and dropped since the server started, and the reason for the last drop (rate limit, daily cap, origin not allowed, inbound filter, spike protection, invalid event, or a storage error). Owlee counts only requests with a valid project key. The counts are kept in memory.

The **Ingest protection** panel of an app has three settings. Editors can change them.

- **Allowed domains.** One host per line. A host also allows its subdomains, and `*` allows every origin. Empty allows every origin. The host comes from `Origin`, or from `Referer` when `Origin` is missing. A request with neither header passes, because server SDKs send neither. Any other host gets `403` before the app bucket is used. `localhost`, `127.0.0.1`, and `::1` pass when `OWLEE_INGEST_ALLOW_LOCALHOST` is on. The check stops other web pages, not scripts, because a script can send any `Origin`.
- **Inbound filters.** A matching event answers `200` and is not stored. It does not count against the daily cap. **Browser extension errors** and **Web crawlers** are on for a new app. **Localhost** and **Legacy browsers** are off. The crawler and legacy browser filters read the `User-Agent` in the event's `request.headers`, which browser SDKs send.
- **Spike protection.** On for a new app. It limits the events an app stores in one UTC hour to the larger of the daily cap / 24 and `3 * mean + 3 * sd` of the stored counts of the 168 hours before. An event over the limit answers `429` with `Retry-After` until the next hour and `X-Sentry-Rate-Limits: <secs>:error:project:spike_protection`. It does not count against the daily cap. Hourly counts older than 8 days are deleted.

`examples/` has PHP, Node, browser, and Rust SDK programs. Set `SENTRY_DSN` to the project DSN.

Short-link hostnames only take over a request when the `Host` is a configured link domain and not Owlee's own origin, and only after `/api` and `/go` have already been routed. Sentry ingest stays on `/api/...`. In production a link hostname redirects only after its TXT record is verified, the same way as a site domain. A link slug cannot start with a path that Owlee uses, such as `api`. Each address may record 30 clicks a minute. Past that, the redirect still works and the click is not counted.

## Analytics

Add a site in **Sites**, then paste the snippet from **Tracker**:

```html
<script defer src="https://owlee.example.com/data.js" data-site="YOUR_PUBLIC_ID"></script>
```

The script posts JSON to `POST /api/receive` on the same origin as `/data.js`. It sends no cookies (`credentials: 'omit'`). CORS still allows the browser to read the response from another origin. The body is rejected with `413` above `OWLEE_ANALYTICS_MAX_BODY_BYTES` (8 KiB). Known bot user agents are answered `202` and not stored.

Events are accepted only when `Origin` (or `Referer`, if `Origin` is missing) matches the site domain or a subdomain of it, or an enabled link domain attached to that site. In development, `localhost` and `127.0.0.1` are also accepted (`OWLEE_INGEST_ALLOW_LOCALHOST`, on in development and off in production). A new site is unverified. Development still collects, and the site page says so. Production returns `403` until you add the TXT record shown on the site (`_owlee-verify.example.com` TXT `owlee-verify=<token>`) and press Verify. **Download zone file** saves the records as a BIND zone file, which most DNS hosts can import. For a link hostname the file also has a DNS-only CNAME (or A/AAAA) record to the host of `OWLEE_PUBLIC_ORIGIN`. Owlee re-checks verified domains every `OWLEE_DOMAIN_RECHECK_SECS` (1 hour) and clears the flag when DNS answers without the token. A lookup error does not clear it. Status pages have no custom hostname, so there is nothing to verify there.

The same site key and the same client IP each have a token bucket. The site bucket is used only after the origin and verified checks pass, so traffic from another origin cannot block a site. The `Origin` check stops other web pages, not scripts. A script can send any `Origin`, so the counts of a site can be inflated. Per site (or link) and day, each dimension such as path or referrer keeps at most 1,000 distinct values. Other values are counted as `(other)`. A full bucket or a full daily quota answers `429` with `Retry-After`. The default is `OWLEE_ANALYTICS_DAILY_QUOTA` (1,000,000 per site). A million pageviews a day is a large site for this install; a site can set a lower or higher cap of its own. The counter resets at 00:00 UTC. `window.owleeEvent(name, data)` records a custom event.

The quota panel of a site also shows how many events were stored and dropped since the server started, and the reason for the last drop (rate limit, daily cap, origin not allowed, unverified domain, or a storage error). Bots are not counted as drops. A request for an unknown site or one blocked by the per-IP bucket is not counted, because no site is known for it yet. The counts are kept in memory.

Raw `insight_events` rows, including link clicks, older than `OWLEE_ANALYTICS_RETENTION_DAYS` (90) are deleted in batches, with their visits and per-day visitor ids. Daily rollups (`insight_daily`, `insight_link_daily`, and the dimension tables) stay, and those are what the overview reads. `insight_events(timestamp)` is indexed for the delete.

Visitor ids rotate monthly. A visit ends after 30 minutes of idle time. Set `OWLEE_PROXY_HOPS` when Owlee sits behind a reverse proxy (`1` for Traefik or Coolify, which is the default in `deploy/compose.yaml`; `2` when Cloudflare is also in front, which enables `cf-ipcountry` / `cf-region` / `cf-ipcity`). `0` ignores `X-Forwarded-For` and treats the proxy as the client, so every visitor shares one rate-limit bucket. `X-Forwarded-For` and the Cloudflare headers are read only when the TCP peer is in `OWLEE_TRUSTED_PROXIES` (comma-separated CIDRs; the default is loopback and private networks, `none` trusts no proxy, `0.0.0.0/0` is refused). A client that reaches the port directly cannot fake its address or location. Add the Cloudflare ranges only if Cloudflare connects to Owlee with no proxy between them. Point `OWLEE_GEOIP_PATH` at a MaxMind or DB-IP city `.mmdb` if you want country, region, and city without Cloudflare. DB-IP Lite data is published by [DB-IP](https://db-ip.com).

Login, registration, password reset, and setup share one limit per client IP (`OWLEE_LOGIN_RATE_IP_*`). Login also has a limit per account and client address (`OWLEE_LOGIN_RATE_ACCOUNT_*`, 5 at once and then 1 a minute), so wrong guesses from one address do not lock the owner out at another. Each attempt takes a token before the hash, and a successful login or a password reset gives them back. A second limit per account counts every address, at 5 times the rate and 20 times the burst, and caps guesses that come from many addresses. A wrong current password on the change-password form uses the limit per account and address. Both limits are checked before the password hash. A login with an unknown email still checks a hash, so the response time does not show which emails have an account. A full bucket answers `429` with `Retry-After`.

A request that runs longer than 60 seconds gets `408`. The server stops cleanly on Ctrl+C or SIGTERM (`docker stop`).

Short links redirect with `307` or `308` and `Cache-Control: no-store`. On Owlee's own hostname use `/go/{slug}`. On any other hostname that you added under **Links**, the path is the slug. `HEAD` requests and known bots follow the redirect and are not counted.

When public sharing is on, these routes need no session. Counts below the site's threshold are folded into **Other**:

```text
GET /api/public/sites/{slug}/overview?from=<unix>&to=<unix>
GET /api/public/sites/{slug}/{path|referrer|country|browser|os|device}
GET /api/public/links/{slug}/overview
GET /api/public/links/{slug}/{referrer|country|browser|os|device}
```

The matching pages are `/share/sites/{slug}` and `/share/links/{slug}`.

## How this maps to Orbit

Owlee follows [Orbit](https://github.com/coollabsio/orbit)'s layout and leaves out its realtime stack (websockets, live sync, collaborative editing).

| Orbit | Owlee |
| --- | --- |
| `apps/server` binary embeds the web app | `apps/server`, binary name `owlee` |
| `crates/platform` (config, db, auth, migrations) | `crates/platform` |
| `crates/orbit` domain rules | `crates/status` |
| `apps/web` Vite, React, Tailwind | `apps/web` |
| SQLite file, or Postgres via `ORBIT_DATABASE_URL` | SQLite file, or Postgres via `OWLEE_DATABASE_URL` |
| `MigrationRunner`, checksums in `schema_migrations` | same |
| Argon2id passwords, random session tokens | same, sessions stored in the database |
| Initial setup URL, `setup-token rotate`, closed registration | same: one-time `/setup?token=`, then the root user opens registration in Admin after SMTP |
| `platform::jobs` worker | `scheduler`: a lease row plus the check loop |
| `justfile`, multi-stage Dockerfile, `deploy/compose.yaml` | same roles |

The domain follows [OpenStatus](https://github.com/openstatusHQ/openstatus): monitors, check history, status pages and components, incident timelines, maintenance, and a notification provider list. Hosted regions and billing are not in this MVP. The Owlee process runs its own checks with `region = local`. [Agents](#agents) are the private locations: their results use the agent name as the region.

The interface takes Coolify's dark shell with a blue accent in place of Coolify's purple. The public page takes the OpenStatus layout: one banner, a component list with uptime bars, maintenance, and incidents.

## Layout

```
apps/server     API, scheduler, checker, agent mode, error ingest, analytics, short links, repositories, migrations
apps/web        React dashboard, public status page, and public share pages
crates/platform Config, SQLite/Postgres pool, migrations, passwords, ids
crates/status   Assertions, state machine, uptime, slugs, notification signing
crates/insights Cookieless visitor ids, URL parsing, user-agent classification, short-link rules
crates/errors   Sentry envelope parsing, event normalization, issue fingerprinting
deploy          Docker Compose
docs            README screenshots
examples        PHP, JavaScript, and Rust programs that send errors
```

## Privacy

[PRIVACY.md](PRIVACY.md) lists what Owlee stores, how long it keeps it, and how to answer access and erasure requests.

## Attribution

Cookieless analytics and the link shortener reimplement [Beacon](https://github.com/ShadowArcanist/beacons) by ShadowArcanist, which is MIT licensed. Copyright (c) 2026 ShadowArcanist. The tracker in `apps/server/src/insights/tracker.js` is wire-compatible with Beacon's tracker and also defines `window.owleeEvent`. See [NOTICE](NOTICE).

Point Coolify at `ghcr.io/coollabsio/owlee:0.1.0`. `OWLEE_IMAGE` is the compose override. The source repository is `https://github.com/coollabsio/owlee`. API problem URLs use `https://owlee.io/problems/...`.

## License

Apache-2.0. See [LICENSE](LICENSE). Third-party MIT terms for the tracker are in [NOTICE](NOTICE).
