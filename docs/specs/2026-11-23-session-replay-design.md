# Session replay design

Status: built on 2026-11-23. Revised after a review by four models on 2026-11-23. §14 lists where the build differs from this text.

## 1. Goal

An error issue shows the stack trace and the breadcrumbs. It does not show what the user did on the page before
the error. Session replay records the page as a stream of DOM changes and plays it back as a video-like view.

Owlee already receives errors from the official Sentry SDKs. The Sentry browser SDK also records replays with
[rrweb](https://github.com/rrweb-io/rrweb) and sends them in the same envelope format to the same DSN. This design
lets an Owlee error app receive, store, and play those replays, and link them to error events.

The rules that Owlee has now stay the same:

- One Rust binary. No new service, queue, object store, or database engine.
- SQLite is the default. Postgres uses the same schema and the same SQL.
- Every row belongs to an error app, and an app belongs to a workspace. A user from another workspace gets 404.
- Ingest is guarded by body limits, token buckets, allowed domains, inbound filters, caps, and retention.

## 2. Scope

### In scope

- R0. Intake of `replay_event` and `replay_recording` envelope items on the existing error ingest route.
- R1. Storage, per-app switch, caps, retention, privacy export and erasure.
- R2. Replay list and replay page with a player, an error timeline, and the breadcrumbs of the recording.
- R3. Links between error events and replays, in both directions.

### Not in scope

- An Owlee recorder script. The Sentry browser SDK is the only sender (D1).
- Mobile replay (`replay_video` items from the Sentry iOS and Android SDKs). They are dropped with a reason.
- Server-side masking of recorded DOM text. Masking is done by the SDK (D7).
- Heatmaps, rage-click and dead-click detection, and replay search over page content.
- Replays for analytics sites. Analytics is cookieless and keeps no per-visitor data. Replay does not fit it (D2).

## 3. Sender protocol

Facts from the Sentry SDK spec and source. Owlee depends on each of them.

| Fact | Source |
|---|---|
| One segment is one envelope with one `replay_event` item and one `replay_recording` item. | [Replays spec](https://develop.sentry.dev/sdk/telemetry/replays/) |
| `replay_recording` is a JSON header line `{"segment_id": N}`, a newline, then the recording. The recording is a JSON array of rrweb events. The browser SDK compresses it with zlib (fflate `Zlib`), or sends plain JSON when compression is off. No header tells which. | [Replays spec](https://develop.sentry.dev/sdk/telemetry/replays/), [`Compressor.ts`](https://raw.githubusercontent.com/getsentry/sentry-javascript/develop/packages/replay-worker/src/Compressor.ts) |
| `replay_event` has `replay_id` (32 hex chars, equal to `event_id`), `replay_type` (`session` or `buffer`), `segment_id` (from 0), `timestamp`, and `replay_start_timestamp` on segment 0. Optional: `urls`, `trace_ids`, `release`, `environment`, `user`, `request.url`, `request.headers.User-Agent`, `sdk`. `error_ids` is deprecated. | [Replays spec](https://develop.sentry.dev/sdk/telemetry/replays/) |
| The SDK sends a segment about every 5 seconds. A replay ends after 60 minutes, or after 15 minutes with no user activity. | [Replays spec](https://develop.sentry.dev/sdk/telemetry/replays/), [Session Replay docs](https://docs.sentry.io/platforms/javascript/session-replay/) |
| The SDK caps one segment at 20,000,000 characters of uncompressed JSON. | [`constants.ts`](https://raw.githubusercontent.com/getsentry/sentry-javascript/develop/packages/replay-internal/src/constants.ts) |
| An error event sent while a replay records carries the tag `replayId`. | [`handleGlobalEvent.ts`](https://raw.githubusercontent.com/getsentry/sentry-javascript/develop/packages/replay-internal/src/coreHandlers/handleGlobalEvent.ts) |
| Both replay item types map to the rate-limit category `replay`. | [`envelope.ts`](https://raw.githubusercontent.com/getsentry/sentry-javascript/develop/packages/core/src/utils/envelope.ts), [`data_category.rs`](https://raw.githubusercontent.com/getsentry/relay/master/relay-base-schema/src/data_category.rs) |
| By default the SDK masks all text and inputs and blocks media. | [Session Replay docs](https://docs.sentry.io/platforms/javascript/session-replay/) |
| `replaysSessionSampleRate` records whole sessions. `replaysOnErrorSampleRate` keeps a short buffer and sends it only when an error occurs (`replay_type: buffer`). | [Session Replay docs](https://docs.sentry.io/platforms/javascript/session-replay/) |

## 4. Intake (R0)

The route stays `POST /api/{app_id}/envelope/`. Replay intake is its own module, `errors/replay.rs`. The envelope
loop calls it once for each envelope that has replay items. The pipeline order of the
[ingest protection design](2026-11-23-error-ingest-protection-design.md) stays the same, with these changes:

| Step | Check | Refusal of the replay items |
|---|---|---|
| 1 | IP token bucket, before the body is read (exists). It names `error;replay` in its refusal, because the category is not known yet. | `429` |
| 2 | Body size. The read limit is `OWLEE_REPLAY_MAX_BODY_BYTES` (default 10 MiB). The `event` item keeps `OWLEE_ERROR_MAX_BODY_BYTES` as its item length limit (D5). | `413` |
| 3 | Key (exists). | `401` / `404` |
| 4 | Allowed domains (exists). | `403` |
| 5 | **Bucket split.** An envelope with replay items and no `event` item refunds its token of the error IP bucket and takes one from the replay IP bucket. It takes from the replay app bucket instead of the error app bucket (D11). | drop, `replay` rate limit |
| 6a | **Replays switch.** The app has `replays_enabled = 0`. | drop, reason `replays_disabled`, `replay` limit for 86400 s |
| 6b | **Parse.** See below. | drop, reason `malformed` |
| 7 | Inbound filters. They run on the `replay_event`: `request.url` and `request.headers.User-Agent`. The browser extension filter does not apply. | drop, reason `filtered` |
| 8a | **New replays per hour.** A `replay_id` that the app does not have yet counts against `replay_hourly_limit`. Segments of a known replay do not count. | drop, reason `replay_hourly_limit`, `replay` limit until the next UTC hour |
| 8b | **Daily bytes.** Compressed bytes stored by the app today plus this segment exceed `replay_daily_bytes`. | drop, reason `replay_quota`, `replay` limit until UTC midnight |
| 10 | Store. | none |

Outcome for each item (D12):

- A replay refusal never changes the HTTP status that the `event` item gets. It adds the `replay` category to
  `X-Sentry-Rate-Limits` and counts the drop in the intake counts. An envelope with only replay items answers
  `200`, with the header when an item was refused.
- Steps 1 to 4 refuse the whole envelope, as now, because they run before the items are known.
- Replay items never take from the error daily cap, the error app bucket, or spike protection.

Parse rules (step 6b):

- `replay_event` is JSON. `replay_id` is 32 lowercase hex characters. `segment_id` is an integer from 0 to 999.
  The SDK ends a replay after 60 minutes, which is at most about 720 segments.
- The recording header has the same `segment_id`.
- The recording is zlib (first byte `78`, CMF and FLG checked as RFC 1950 requires) or plain JSON (first byte
  `[`). Other bytes are `malformed`. The zlib stream must end exactly at the end of the item.
- The recording inflates to at most `OWLEE_REPLAY_MAX_SEGMENT_BYTES` (default 64 MiB, above the SDK's 20 M
  characters in UTF-8). The check streams into `serde::de::IgnoredAny`, which checks that the input is a JSON
  array, in `spawn_blocking`. It does not build a JSON value.
- `replay_video` items are dropped with reason `unsupported`.

Owlee stores the recording bytes as the SDK sent them (D4).

## 5. Storage (R1)

```sql
ALTER TABLE error_apps ADD COLUMN replays_enabled BIGINT NOT NULL DEFAULT 0;
ALTER TABLE error_apps ADD COLUMN replay_daily_bytes BIGINT NOT NULL DEFAULT 536870912;  -- 512 MiB
ALTER TABLE error_apps ADD COLUMN replay_hourly_limit BIGINT NOT NULL DEFAULT 1000;      -- new replays

CREATE TABLE replays (
    app_id TEXT NOT NULL REFERENCES error_apps (id) ON DELETE CASCADE,
    replay_id TEXT NOT NULL,              -- 32 lowercase hex chars, chosen by the SDK
    replay_type TEXT NOT NULL,            -- session | buffer
    has_start BIGINT NOT NULL,            -- 1 when segment 0 is stored
    started_ms BIGINT,                    -- from segment 0, clamped
    last_ms BIGINT NOT NULL,              -- max segment time, clamped
    received_ms BIGINT NOT NULL,          -- server clock, first segment
    last_received_ms BIGINT NOT NULL,     -- server clock, last segment
    segment_count BIGINT NOT NULL,
    bytes BIGINT NOT NULL,                -- compressed bytes of all segments
    first_url TEXT,
    url_count BIGINT NOT NULL,
    release TEXT,
    environment TEXT,
    browser TEXT,
    os TEXT,
    user_label TEXT,                      -- user.username, else user.email, else user.id
    PRIMARY KEY (app_id, replay_id)
);
CREATE INDEX replays_app_received ON replays (app_id, received_ms);
CREATE INDEX replays_last_received ON replays (last_received_ms);

CREATE TABLE replay_segments (
    app_id TEXT NOT NULL,
    replay_id TEXT NOT NULL,
    segment_id BIGINT NOT NULL,
    time_ms BIGINT NOT NULL,              -- clamped
    encoding TEXT NOT NULL,               -- deflate | identity
    data BLOB NOT NULL,                   -- BYTEA on Postgres
    PRIMARY KEY (app_id, replay_id, segment_id),
    FOREIGN KEY (app_id, replay_id) REFERENCES replays (app_id, replay_id) ON DELETE CASCADE
);

CREATE TABLE replay_urls (
    app_id TEXT NOT NULL,
    replay_id TEXT NOT NULL,
    url TEXT NOT NULL,
    PRIMARY KEY (app_id, replay_id, url),
    FOREIGN KEY (app_id, replay_id) REFERENCES replays (app_id, replay_id) ON DELETE CASCADE
);

-- Same shape and same keyed hash as event_users.
CREATE TABLE replay_users (
    user_hash TEXT NOT NULL,
    app_id TEXT NOT NULL,
    replay_id TEXT NOT NULL,
    PRIMARY KEY (user_hash, app_id, replay_id),
    FOREIGN KEY (app_id, replay_id) REFERENCES replays (app_id, replay_id) ON DELETE CASCADE
);

CREATE TABLE replay_daily_usage (
    app_id TEXT NOT NULL REFERENCES error_apps (id) ON DELETE CASCADE,
    day BIGINT NOT NULL,                  -- Unix second of the UTC day start
    bytes BIGINT NOT NULL,
    PRIMARY KEY (app_id, day)
);

CREATE TABLE replay_hourly_usage (
    app_id TEXT NOT NULL REFERENCES error_apps (id) ON DELETE CASCADE,
    hour BIGINT NOT NULL,                 -- Unix second of the UTC hour start
    replays BIGINT NOT NULL,
    PRIMARY KEY (app_id, hour)
);

ALTER TABLE events ADD COLUMN replay_id TEXT;
CREATE INDEX events_app_replay ON events (app_id, replay_id);
```

### Store

One segment is one transaction, in this order (D6):

1. Insert the `replays` row with `ON CONFLICT DO NOTHING`. When it added a row, reserve one new replay in
   `replay_hourly_usage` with the conditional upsert of the error counters. A refusal rolls back.
2. Insert the segment with `ON CONFLICT DO NOTHING`. When it added no row, commit and stop. A retry of the same
   segment changes no row and no counter.
3. Reserve the segment bytes in `replay_daily_usage` with the conditional upsert. A refusal rolls back the whole
   transaction.
4. Update the `replays` row with the merge rules below.

Merge rules for the `replays` row:

| Column | Rule |
|---|---|
| `segment_count`, `bytes` | Add the segment. |
| `last_ms`, `last_received_ms` | Maximum. |
| `has_start`, `started_ms` | Set by segment 0 only. |
| `replay_type` | `buffer` can become `session`. `session` stays `session`. |
| `first_url`, `release`, `environment`, `browser`, `os`, `user_label` | The first value that is not null wins. |
| `url_count` | Count of `replay_urls` rows for the replay. |

Rules for the values:

- The key of a replay is `(app_id, replay_id)`. A sender with the key of app A cannot write into a replay of
  app B. A sender who guesses a `replay_id` of the same app can add a segment that the real SDK then cannot
  replace. The ids are 128 random bits, so this is an accepted risk.
- Client times (`replay_start_timestamp`, `timestamp`) are clamped to the range from 2 hours before to 5 minutes
  after the server receive time.
- `replay_urls` keeps at most 100 URLs for each replay. `first_url` and every URL go through `scrub_text`, so
  sensitive query parameters become `[Filtered]`, as in error events.
- `replay_event` is not stored as JSON. Owlee keeps only the columns above.
- `user.ip_address` is never stored. The `localhost` inbound filter reads it in memory, as for events.
- An error event with a `replayId` tag of 32 hex characters gets the value in `events.replay_id`. The tag stays in
  the event. No foreign key, because the error often comes before its replay (buffer mode), and the replay can be
  refused (D9).

### Retention

| Data | Setting | Default |
|---|---|---|
| Replays and segments | `OWLEE_REPLAY_RETENTION_DAYS` | 14 |
| `replay_daily_usage` and `replay_hourly_usage` rows | fixed | 8 days |

Retention selects replays by `last_received_ms`, the server clock. A sender cannot keep a replay with a future
timestamp. It deletes segments in batches of 500 first, then the `replays` rows, as the other retention jobs do.
On SQLite, deleted pages are reused by later inserts. The file does not shrink. The admin page shows the replay
bytes of each app.

## 6. Playback (R2)

### Isolation

A recording is a copy of a DOM that a browser on the internet sent. The DSN key is public, so anyone can send a
recording with any HTML in it. That HTML must never run as script, and the viewer's browser should not load
resources from hosts that the sender chose.

The guarantee comes from the browser, not from rrweb parsing (D8):

- rrweb's `Replayer` rebuilds the recorded DOM in an inner iframe with `sandbox="allow-same-origin"` and no
  `allow-scripts`. The browser runs no script in that frame, whatever the recording has. Forms, popups, and top
  navigation are also blocked by the sandbox. Owlee never sets `UNSAFE_replayCanvas`, which adds `allow-scripts`.
- The player runs in its own same-origin document, `/replay-player.html`, framed by the app page. It has its own
  CSP, set by its handler:
  `default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src data: blob:; font-src data:;
  connect-src 'none'; form-action 'none'; base-uri 'none'; frame-ancestors 'self'`.
  The inner iframe inherits this CSP. Recorded inline styles work. Recorded URLs to other hosts do not load.
- `security_headers.rs` gets a third path class next to `is_embeddable`: the player path. It gets
  `X-Frame-Options: SAMEORIGIN` and keeps the CSP of its handler. All other paths keep their headers. The main
  Owlee CSP does not change.
- The app page fetches the segments and sends the parsed events to the player with `postMessage`, with the Owlee
  origin as the target. Each side accepts a message only when `event.source` is the other window and
  `event.origin` is the Owlee origin. The player sends back only player state (time, play, pause).
- The player is `rrweb-player` (MIT), loaded only by `/replay-player.html`. The main bundle does not grow.

Cost of this choice: stylesheets and fonts that rrweb could not inline (for example from a CDN without CORS) stay
as links to other hosts. The CSP blocks them, so those pages play with less styling. This is accepted.

### Pages

- The error app page gets a "Replays" tab. It lists replays by receive time with duration, first URL, URL count,
  user label, browser, release, and error count. Filters: URL (substring match on `replay_urls.url`), release,
  environment, and "has errors".
- The replay page `/errors/apps/{app_id}/replays/{replay_id}` has the player, a timeline under it with one mark for
  each linked error event, and a side list of breadcrumbs and navigations. The side list comes from the Sentry
  custom rrweb events (`type: 5`, `data.tag: breadcrumb`) in the recording, read in the browser. A click on a
  list item or a mark moves the player to that time.
- A replay with `has_start = 0` shows "Incomplete: the start of this replay was not stored" and has no player.
- A replay with `last_received_ms` in the last 15 minutes shows "Live: new segments may arrive". It reloads its
  segment list every 10 seconds while the tab is visible, and adds new segments with the player's `addEvent`.

## 7. Links (R3)

- An event or issue page shows "Watch replay" when the event has a `replay_id` and that replay exists in the same
  app. The link opens the replay page at the error time minus 5 seconds.
- The replay page lists the events of the same app with the same `replay_id`.
- An issue page shows the count of its events that have a replay.

## 8. API

| Route | Use |
|---|---|
| `GET /api/error-apps/{id}/replays?url=&release=&environment=&hasErrors=&from=&to=&cursor=` | replay list |
| `GET /api/error-apps/{id}/replays/{replay_id}` | replay metadata, URLs, linked events, segment ids |
| `GET /api/error-apps/{id}/replays/{replay_id}/segments/{segment_id}` | one segment |
| `DELETE /api/error-apps/{id}/replays/{replay_id}` | delete one replay (editors) |
| `PUT /api/error-apps/{id}/replays-settings` | `{ enabled, dailyBytes, hourlyLimit }` (editors) |

- Every route loads the app through the same workspace check as the other error app routes, then reads only rows
  with that `app_id`. A replay id from another app is 404.
- `dailyBytes` is from 1 MiB to 100 GiB. `hourlyLimit` is from 1 to 100,000. Other values are a `422` field
  error.
- The segment route answers the stored bytes with `Content-Encoding: deflate` (zlib, as HTTP defines it) or no
  encoding, `Content-Type: application/json`, and `Cache-Control: no-store`. The browser inflates it, so
  `fetch().json()` works with no library (D4). `no-store` makes sure that a deleted or erased segment does not
  stay in a browser cache.
- The routes are in the OpenAPI document. Recordings get a new API token scope, `replays:read`. `errors:read`
  gives the replay list and metadata only. MCP gets one tool, `list_replays`, with the metadata only.

## 9. Privacy

PRIVACY.md gets a "Session replay" part:

- A replay holds what the user saw on the page and what they did, as far as the SDK did not mask it. Owlee cannot
  check the masking. Keep the SDK defaults (`maskAllText`, `maskAllInputs`, `blockAllMedia`).
- Replays are off for each app until an editor turns them on. The switch shows this text.
- Owlee stores no IP address for a replay.
- The replay list shows `user.username`, else `user.email`, else `user.id`, in clear, as error events keep them.
  Owlee also keeps a keyed hash of each of them in `replay_users`, as for error events.
- Owlee filters sensitive query parameters in the URLs it lists. It does not change the recording, so URLs,
  breadcrumbs, and page text inside the recording stay as the SDK sent them.
- **Settings → Privacy requests** also finds replays by those hashes. Delete events also deletes those replays.
  Export JSON lists the replay metadata and a download link for each segment. A replay with no user fields cannot
  be found by a request. Delete it from the replay page.

## 10. Build order

1. R0. Envelope parse of the two items in `crates/errors`, with fixed-input tests for zlib, plain, and each
   malformed case. `errors/replay.rs` with the bucket split, switch, filters, caps, and the idempotent store.
2. R1. Migration, retention, privacy export and erasure, admin bytes count, `replays:read` scope.
3. R2. Player document and its headers, browser isolation test, replay list and replay page.
4. R3. `replayId` tag to `events.replay_id`, "Watch replay" links, issue count.

## 11. Tests

- Envelope parsing: zlib and plain recordings. A zlib header other than `78 9c`. Mismatched `segment_id`.
  `segment_id` 1000. Trailing bytes after the zlib stream. Recording that inflates past the limit. Not a JSON
  array.
- Store: the same segment twice gives one segment row and unchanged `replays` counters, `replay_daily_usage`, and
  `replay_hourly_usage`. Segment 3 before segment 0 is stored with `has_start = 0`. Segment 0 then sets
  `has_start` and `started_ms`. A `buffer` replay becomes `session`.
- Clamping: a segment with a timestamp in the year 2100 is stored with the receive time plus 5 minutes, and
  retention deletes it on time.
- Caps: the 1001st new replay in an hour is dropped. Segments of known replays still pass. A segment that passes
  the daily bytes is dropped. In both cases the error event in the same envelope is still stored, the answer is
  `200`, and `X-Sentry-Rate-Limits` names `replay`.
- Buckets: a replay-only envelope does not take from the error app bucket, and an error event after many replay
  envelopes from one IP is still accepted.
- Authorization: replay of app A read through the route of app B is 404. A user of another workspace gets 404. An
  API token with only `errors:read` gets 403 on the segment route.
- Player isolation (browser test in R2): a recording with `<script>`, an inline event handler,
  `<img src=https://example.com>`, a relative `<img src=/api/...>`, `<base href>`, `<meta http-equiv=refresh>`,
  `<form action>`, and `<iframe srcdoc>` plays with no script run and no request out of the player. The player
  page loads in the app frame, and opened directly it shows nothing.

## 12. Open questions

- Is 512 MiB a good default daily cap? A segment is often 1 to 50 KB compressed, and the first full snapshot of a
  large page can be 1 MB or more. Measure with a real app in R0 before release.
- The read limit before the key check grows from 1 MiB to 10 MiB for every request. Should Owlee resolve the key
  from `X-Sentry-Auth` or the query before reading the body, and keep 1 MiB for requests with no such key? Also
  consider a semaphore for large bodies.
- Multi-megabyte blob writes hold the single SQLite writer that error and analytics ingest use. R0 must measure
  the lock time at the expected write rate. Is a total replay byte limit for the install needed, on top of the
  limit for each app?
- A 60-minute replay is about 720 segment requests, all held in browser memory. Should the player load segments
  on demand as playback reaches them, or should one route return a range of segments?
- Should a buffer replay with no linked error after 24 hours be deleted early? It costs space and has little use.

## 13. Decisions

| # | Decision | Date |
|---|---|---|
| D1 | The sender is the Sentry browser SDK. Owlee writes no recorder. The SDK is maintained, masks by default, and is already in the apps that send errors to Owlee. | 2026-11-23 |
| D2 | Replay belongs to error apps, not to analytics sites. Analytics promises no per-visitor data and no cookies. A replay is per-visitor data by nature. | 2026-11-23 |
| D3 | Segments are stored in the database as blobs, not as files. One database keeps backup, deletion, and Postgres setups as they are now. Caps and retention limit the size. | 2026-11-23 |
| D4 | Owlee stores the bytes the SDK sent and serves them with a matching `Content-Encoding`. No server-side recompression, and no inflate library in the browser. Gzip is not accepted, because the browser SDK never sends it and gzip decoders do not agree on multi-member streams. | 2026-11-23 |
| D5 | The envelope read limit grows to the replay limit for every app, and the `event` item keeps its old limit as an item length limit. The key can come in the envelope header, so the app is not verified before the body is read. §12 has the open question of a lower limit before the key check. | 2026-11-23 |
| D6 | One segment is one transaction: replay row, segment, then counters. Each counter changes only when its insert added a row. SDK retries and segments out of order converge to the same rows and counters. | 2026-11-23 |
| D7 | No server-side masking of recorded DOM. Parsing and rewriting rrweb mutations is large and easy to get wrong. The SDK masks by default, PRIVACY.md says so, and the per-app switch is off by default. | 2026-11-23 |
| D8 | The player runs in a same-origin document with its own strict CSP. Recorded HTML is rebuilt in rrweb's inner iframe with `sandbox="allow-same-origin"` and no `allow-scripts`, so the browser never runs recorded script. This is how Sentry plays replays. An opaque-origin player was rejected: a test in Chrome 157 on 2026-11-23 showed that a frame sandboxed without `allow-same-origin` gets `null` for the `contentDocument` of its child iframes, so rrweb cannot rebuild the DOM. A second origin for the player was rejected, because it needs an extra hostname and certificate for each install. | 2026-11-23 |
| D9 | Errors link to replays through `events.replay_id`, written at ingest from the `replayId` tag and indexed. No foreign key, because errors and replays arrive in any order and a replay can be refused. | 2026-11-23 |
| D10 | Replays have a daily byte cap and an hourly cap on new replays, not spike protection. Bytes are the disk cost. The hourly cap stops a sender with the public key from spending the whole day's bytes in minutes with random replay ids. Segments of a known replay do not count against it, so replays that already started can finish. | 2026-11-23 |
| D11 | Replay-only envelopes use their own app and IP token buckets (`OWLEE_REPLAY_RATE_APP_PER_MIN`, default 1200, and `OWLEE_REPLAY_RATE_IP_PER_MIN`, default 120). One session sends about 12 envelopes a minute, so the error buckets (120 and 60 a minute) would refuse errors after about 10 sessions. The IP check before the body cannot know the category, so a replay-only envelope gives back its error IP token after the parse. | 2026-11-23 |
| D12 | The outcome is decided for each item. A replay refusal drops the replay items, adds the `replay` category to `X-Sentry-Rate-Limits`, and never changes the status of the event item. The SDK reads that header on every response. | 2026-11-23 |

## 14. Build notes

| # | Where | What the build does |
|---|---|---|
| B1 | §8 list route | No `from` and `to` filters. The page is `{ items, nextCursor }`, as the issue events list, with `limit` from 1 to 50. |
| B2 | §8 settings route | A `422` names `daily_bytes` and `hourly_limit` under `fields`, as the other error app forms do. |
| B3 | §5 schema | `data` is declared `BYTEA`. Postgres stores binary, and SQLite keeps the blob unchanged under that type name. An extra index `replay_users (app_id, replay_id)` keeps the cascade delete fast. |
| B4 | §4 rate limits | Reasons in `X-Sentry-Rate-Limits`: `replays_disabled`, `replay_hourly_limit`, and `quota_exceeded` for daily bytes. An envelope with an event and replay items also takes a replay app token. Replay bucket bursts are a quarter of the per-minute rate. |
| B5 | §6 timeline | The Sentry SDK writes its `performanceSpan` custom events with an rrweb `timestamp` in seconds. The web app converts every timestamp below 10^11 to ms before playback, or the player starts in 1970. |
| B6 | §7 links | The issue detail adds `replay` to its `latest` event too, because the issue page shows that event without fetching it again. |
| B7 | §8 tokens | The token form has a "Replay recordings" row with only Read. Token app limits apply to replay routes as to the other error app routes. |
| B8 | §11 tests | Tested in the dev environment with `@sentry/browser` 11.17.0: record, list, play, seek, error link, and masked input. A hostile recording (`<script>`, `onerror`, `<base>`, `<meta refresh>`, `<form>`, `<iframe srcdoc>`, external and relative images) ran no script. Served by Owlee with the player CSP, it made no request to another host. Served by the Vite dev server, which sends no CSP, the images and stylesheet did load. |

