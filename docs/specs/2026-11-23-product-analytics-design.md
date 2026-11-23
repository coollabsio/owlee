# Product analytics design

Status: draft. Nothing in this document is built yet. Reviewed with `/interrogate` on 2026-11-23. The fixes of that
review are in this version.

## 1. Goal

Owlee web analytics counts page views, visits, and custom events. It cannot answer product questions:

- How many visitors who open `/pricing` then sign up? Where do the others leave? (funnels)
- After `/docs`, which page do visitors open next? (paths)
- Of the users who came in week 1, how many come back in week 2, 3, 4? (retention)
- What did user `42` do? (persons)

This design adds these reports to analytics sites. Cookieless stays the default. Funnels and paths work in
cookieless mode. Retention and persons need **identity mode**, which an editor turns on for each site.

The rules that Owlee has now stay the same:

- One Rust binary. SQLite is the default. Postgres uses the same schema and the same SQL.
- Every row belongs to a site, and a site belongs to a workspace. A user from another workspace gets 404.
- Ingest keeps its body limit, token buckets, origin check, domain verification, bot filter, and daily cap.

## 2. Facts that this design depends on

| Fact | Location |
|---|---|
| The visitor id (column `session_id`) is a keyed hash of site, IP address, user agent, and month. The salt changes each UTC month, and the old salt is deleted. | `crates/insights/src/identity.rs:8-19`, `apps/server/src/insights/mod.rs:49-89` |
| A visit (`visit_id`) ends after 30 minutes with no event. | `crates/insights/src/identity.rs:6,21-28` |
| Every event type other than `pageview` and `performance` counts as custom. It adds to `insight_visits.events` and `insight_daily.events`, and a visit with an event is not a bounce. | `apps/server/src/insights/record.rs:12-14,102` |
| A custom event type is 1 to 64 chars of `[A-Za-z0-9_-]`, so `$` is never in a custom name. | `apps/server/src/insights/routes.rs:205-213` |
| The ingest transaction is a plain `begin()`. `begin_immediate()` exists. | `apps/server/src/insights/record.rs:6-10`, `crates/platform/src/db.rs:119,136` |
| The tracker uses no cookie and no local storage. It wraps `history.pushState` and `replaceState`. Its only API is `window.owleeEvent(name, data)`. | `apps/server/src/insights/tracker.js:141-162`, `PRIVACY.md:7` |
| Custom event data is flattened into at most 50 keys in `insight_event_data`. | `crates/insights/src/event.rs:113-158` |
| Raw events are kept for `OWLEE_ANALYTICS_RETENTION_DAYS` (default 90). Daily rollups are kept. | `crates/platform/src/config.rs:126`, `apps/server/src/retention.rs:78-91` |
| `insights/routes.rs` has 2,103 lines. Charts are SVG in `charts.tsx`. There is no chart library. | `apps/server/src/insights/routes.rs`, `apps/web/src/components/charts.tsx` |

Consequence: in cookieless mode, one person is one visitor only while their IP address, user agent, and month
stay the same. That is good enough for a funnel inside a visit or a day. It is not good enough for retention.

## 3. Scope

### In scope

- P0. Identity mode: per-site switch, consent-gated device id, `identify`, signed user ids, persons.
- P1. Funnels: saved, ordered steps, conversion window, drop-off, time to convert, one breakdown.
- P2. Paths: next and previous pages from a start page, inside one visit.
- P3. Retention: cohort by first-seen period, return on any event or one event. Identity mode only.
- P4. Persons: list, timeline, privacy export and erasure.

### Not in scope

- Cohorts (saved person filters). Cut after review to keep the first version small.
- Person properties. `identify` links a device to a user id and nothing more. A site that needs properties sends
  them as custom event data.
- A trends or insight builder. The Overview and Events tabs stay as they are.
- Group analytics, funnels or persons on the public dashboard.
- API token scopes and MCP tools for analytics. The [API token design](2026-11-23-api-tokens-mcp-design.md) adds
  analytics scopes later.
- Cross-subdomain identity through a cookie (D5).

## 4. Feature switches

Each feature has its own switch for each site. An editor turns it on or off in a "Features" card on the Settings
tab. The [click and scroll design](2026-11-23-click-scroll-analytics-design.md) adds its switches to the same card
and the same route.

```sql
ALTER TABLE insight_sites ADD COLUMN funnels_enabled BIGINT NOT NULL DEFAULT 1;
ALTER TABLE insight_sites ADD COLUMN paths_enabled BIGINT NOT NULL DEFAULT 1;
ALTER TABLE insight_sites ADD COLUMN identity_enabled BIGINT NOT NULL DEFAULT 0;
ALTER TABLE insight_sites ADD COLUMN retention_enabled BIGINT NOT NULL DEFAULT 0;
ALTER TABLE insight_sites ADD COLUMN people_enabled BIGINT NOT NULL DEFAULT 0;
```

| Switch | Default | Collects new data | Needs | When off |
|---|---|---|---|---|
| Funnels | on | no | – | Funnels tab hidden. Saved funnels are kept. |
| Paths | on | no | – | Paths tab hidden. |
| Identity mode | off | yes: device keys, persons, device days | – | The server drops `did`, `$identify`, and `$forget`. Stored identity data is kept until retention deletes it or an editor clicks "Delete identity data". |
| Retention | off | no | Identity mode | Retention tab hidden. |
| People | off | no | Identity mode | People view hidden. |

Rules for all switches, in this spec and in the click and scroll design (D13):

- Off stops new collection (for switches that collect data) and hides the tab or view. It never deletes data.
  Data goes by normal retention, or at once with the "Delete ... data" button of that feature (editors, with a
  confirm step). On again shows the kept data again.
- A switch that needs another switch is in effect only when both are on. The stored value is kept, and the UI
  shows the switch greyed out with "Needs Identity mode". No cascade writes.
- A report route of a feature that is not in effect answers `403` with `{ "error": "feature_disabled", "feature":
  "<name>" }`. The web app hides the tab, so a user sees this only through a stale page or a direct call.
- **Settings → Privacy requests** always searches identity data, also when Identity mode or People is off, because
  kept data must still be found and erased.
- Funnels and paths are on by default because they read the page views and events that Owlee already stores.
  Every switch that collects new data is off by default (D14).

## 5. Built-in event types

Owlee-made events use the `$` prefix: `$identify` and `$forget` here, and the autocapture types of the
[click and scroll design](2026-11-23-click-scroll-analytics-design.md). A customer cannot send `$` in a custom
event name, so no built-in type can hide an existing custom event (D1).

- The server accepts a `$` type only from a fixed list. Other `$` types are `400`.
- `record.rs` gets one function `class(event_type) -> Pageview | Performance | Custom | Builtin`. `Builtin` events
  do not add to `insight_visits.events` or `insight_daily.events`, and they do not end a bounce. Only `Pageview`
  and `Custom` change the current metrics.
- The Events tab lists `Custom` types only.
- Built-in events use their own daily cap (kind `builtin` in `ingest_usage`), so they cannot use up the cap of
  page views and custom events.

## 6. Identity mode (P0)

### 6.1 Site settings

```sql
ALTER TABLE insight_sites ADD COLUMN identity_secret TEXT;          -- encrypted with secret_box
ALTER TABLE insight_sites ADD COLUMN identity_require_signature BIGINT NOT NULL DEFAULT 0;
```

- The switch is `identity_enabled` (§4). When it is off, the server drops the `did` field and the `$identify` and
  `$forget` events. The Features card shows the drop count of today by reason, so a snippet that does not match
  the switch is visible (D2).
- `identity_secret` is made the first time identity is turned on. It signs user ids (6.4).
- "Delete identity data" deletes device keys on events, device links, persons, and device days, in batches.

### 6.2 Tracker

One attribute and a call-style API. The setup guide shows both when identity is on:

```html
<script>window.owlee=window.owlee||function(){(owlee.q=owlee.q||[]).push(arguments)}</script>
<script defer src="https://owlee.example.com/data.js" data-site="pub_x" data-identity="true"></script>
```

```js
owlee('consent', true)              // store a device id from now on
owlee('consent', false)             // delete it and ask the server to forget this device
owlee('identify', userId, signature) // link this device to a user id
owlee('reset')                      // on logout: new device id
owlee('event', name, data)          // same as owleeEvent(name, data)
```

- The tracker is cookieless until the page calls `owlee('consent', true)`. A site without a consent banner calls it
  on each load. A site with a banner calls it from its consent tool. Owlee does not store the choice and does not
  show a banner (D3).
- On consent, the tracker makes a random device id (`crypto.randomUUID()`) and keeps it in `localStorage` under
  `owlee_did`. Each later event sends it as `did`.
- `identify` before consent is dropped by the tracker. The tracker never stores the user id.
- `consent false` sends one `$forget` event with the `did`, then deletes `owlee_did`.
- `reset` makes a new device id. The old device stays linked to its user.
- If storage throws, the tracker stays cookieless for the page.
- The tracker reads `owlee.q` on load and replaces `window.owlee` with the real function.

### 6.3 Visitor id stays the same

The visitor id (`session_id`) stays the IP hash in both modes. Visits, visitors, bounce, and all current reports do
not change when identity is on or when consent arrives in the middle of a visit (D4). The device is a separate
column, `device_key`:

```sql
ALTER TABLE insight_events ADD COLUMN device_key TEXT;  -- keyed hash of the did, NULL without consent
CREATE INDEX insight_events_site_device_key ON insight_events (site_id, device_key, timestamp);
CREATE INDEX insight_events_site_type ON insight_events (site_id, event_type, timestamp);
```

`device_key = hex(hmac_sha256(identity_secret, did))[..32]`. The raw `did` is not stored. The existing `device`
column (desktop, mobile, tablet) is a different thing and does not change.

### 6.4 Persons and devices

```sql
CREATE TABLE insight_persons (
    id TEXT PRIMARY KEY,
    site_id TEXT NOT NULL REFERENCES insight_sites (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    UNIQUE (site_id, user_id)
);

CREATE TABLE insight_person_devices (
    site_id TEXT NOT NULL REFERENCES insight_sites (id) ON DELETE CASCADE,
    device_key TEXT NOT NULL,
    person_id TEXT NOT NULL REFERENCES insight_persons (id) ON DELETE CASCADE,
    PRIMARY KEY (site_id, device_key)
);

CREATE TABLE insight_device_days (
    site_id TEXT NOT NULL REFERENCES insight_sites (id) ON DELETE CASCADE,
    device_key TEXT NOT NULL,
    day BIGINT NOT NULL,                  -- start of the UTC day in unix seconds, as day() in crates/insights
    PRIMARY KEY (site_id, device_key, day)
);
```

- An **actor** in reports is the person of a device when the device is linked, else the device. Reports resolve
  this at read time with one join on `insight_person_devices`. No event row is rewritten (D6).
- `$identify` carries `uid` (1 to 200 chars) and `sig`. With `identity_require_signature = 1`, `sig` must be
  `hex(hmac_sha256(identity_secret, uid))`, made by the site's own backend. Else the event is dropped as
  `bad_signature`. Without the setting, `sig` is ignored, and the Settings tab says: "Anyone can send any user id.
  Persons are analytics data, not proof of who did what." (D7)
- On `$identify`: `INSERT INTO insight_persons ... ON CONFLICT DO NOTHING`, select the person id, then
  `INSERT INTO insight_person_devices ... ON CONFLICT DO NOTHING`. The first link of a device wins. An `$identify`
  for a device that is linked to another person is dropped as `device_linked`. The page must call `reset` on
  logout (D8).
- Each event with a device inserts `insight_device_days` with `ON CONFLICT DO NOTHING`. No row is updated on each
  event.
- A new device takes one unit of a per-site daily cap of new devices (kind `devices`, default 50,000). Over the
  cap, the event is stored with `device_key = NULL`.
- Every analytics ingest transaction uses `begin_immediate()`. The current `visit()` already reads and then
  writes, so this also fixes a lost-event case under concurrency on SQLite (D9).
- `$forget`: delete the device's `insight_device_days` rows, set `device_key = NULL` on its events, and delete the device
  link. A person with no devices left is deleted.

### 6.5 Retention of identity data

| Data | Setting | Default |
|---|---|---|
| Raw events | `OWLEE_ANALYTICS_RETENTION_DAYS` | 90 (unchanged) |
| Device days | `OWLEE_ANALYTICS_IDENTITY_RETENTION_DAYS` | 400 |
| Device links and persons | deleted when the device or person has no device day left | – |

The retention job deletes `insight_device_days` rows older than the limit, then device links with no day, then
persons with no device.

## 7. Query engine

- Pure functions in `crates/insights/src/funnel.rs`, `paths.rs`, and `retention.rs` take an iterator of rows and
  return the result. They have unit tests with fixed rows and no database (D10).
- The SQL that feeds them filters as much as it can: site, time range, and the event types of the steps (index
  `insight_events_site_type`). Exact path steps filter `pathname` in SQL. Prefix and glob steps filter in Rust.
  Property conditions read `insight_event_data` by `event_id` for the named keys only.
- A report reads at most `OWLEE_ANALYTICS_QUERY_MAX_ROWS` rows (default 2,000,000), joined rows included. Over the
  limit it stops and answers `422` with "Too many events in this range. Choose a shorter range." A partial result
  is never shown as complete.
- At most 2 reports run at the same time in the process, and each has a 30 s limit. Others wait up to 10 s, then
  get `503`. Ingest uses the same pool, so this keeps writes going.
- The routes live in a new module `apps/server/src/insights/product.rs`, not in `routes.rs`.

## 8. Funnels (P1)

```sql
CREATE TABLE insight_funnels (
    id TEXT PRIMARY KEY,
    site_id TEXT NOT NULL REFERENCES insight_sites (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    definition TEXT NOT NULL,             -- JSON, parsed into a typed struct
    created_by TEXT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
```

```json
{
  "steps": [
    { "kind": "pageview", "path": "/pricing", "match": "exact" },
    { "kind": "event", "name": "signup", "where": [{ "key": "plan", "op": "eq", "value": "pro" }] }
  ],
  "actor": "visit",
  "window_secs": 86400
}
```

| Field | Values |
|---|---|
| `steps` | 2 to 10. `pageview` with `match` = `exact`, `prefix`, or `glob` (`*` is one path segment, `**` is any). `event` with a custom or built-in event name and at most 5 conditions (`eq`, `neq`, `contains`, `gt`, `lt`) on event data keys. |
| `actor` | `visit`, `visitor` (the IP-hash visitor id), or `person` (identity mode only). Default: `visit` without identity, `person` with identity. |
| `window_secs` | From the first step to the last. 60 s to 30 days. Ignored for `visit`. |

- Steps are strictly ordered. A step can have the same timestamp (second) as the step before. Timestamps have a
  one-second resolution, so a stricter rule would lose real conversions.
- Result: for each step, the actors that reached it, the conversion from step 1 and from the step before, and the
  median time from the step before. One optional breakdown on a dimension of the first-step event (path,
  referrer, country, browser, os, device, `utm_*`). At most 10 values, the rest in "Other".
- A click on a drop-off lists at most 100 actors that left at that step, with a link to their timeline.
- With `actor: visitor`, the UI says: "A visitor id changes when the IP address, the browser, or the month
  changes. Long windows undercount."
- The URL holds the definition, so an unsaved funnel can be shared.

## 9. Paths (P2)

- Input: a start path or an end path, a depth from 1 to 5, and `from` / `to`.
- The engine reads the page views of each visit in order and counts the transitions at each level. A page that
  repeats right after itself counts once.
- Output: for each level, the top 10 pages and "Other", and the links between them with counts.
- The UI shows the levels as columns of `BarList` rows with the link counts. A click on a page makes it the new
  start. No Sankey diagram (D11).
- Paths always use visits, also in identity mode. A path across two visits days apart is not a navigation path.

## 10. Retention (P3)

Needs the Retention switch and Identity mode (§4).

- Input: period (`day`, `week`, `month`), number of periods (up to 12, or 13 for months), and a return event
  ("any event" or one event name).
- The cohort of an actor is the period of its first device day. A person's first day is the earliest day of all
  its devices.
- "Any event" reads `insight_device_days` only, so it covers 400 days.
- A named return event reads `insight_events`, so the range is capped at the raw retention (90 days).
- Output: a table with one row per cohort period, the cohort size, and the percent that came back in each later
  period.

## 11. Persons (P4)

- The Visitors tab gets a "People" view when the People switch and Identity mode are on (§4). It lists persons with user id, first seen, last seen
  (from device days), and device count. Search on user id (exact or prefix).
- The person page shows the devices and the event timeline of all its devices.
- An editor can delete a person: its events (by device), device days, device links, and the person.

## 12. Privacy

PRIVACY.md changes:

- Analytics has two modes. Cookieless (default) is as now. Identity mode stores a random device id in the
  visitor's browser only after the site calls consent, and keeps a keyed hash of it on the server.
- The monthly visitor id is unchanged in both modes. The device hash does not rotate. It is deleted with its device
  days after 400 days without activity.
- "Analytics data cannot be tied to a person" becomes true for sites without identity mode only.
- The IP address is still never stored.
- The retention table gets the identity rows of 6.5.
- **Settings → Privacy requests** searches `insight_persons.user_id` only. Export returns the person, its devices,
  and the events of its devices. Erasure deletes them. Events sent before consent have no device and cannot be
  found. Rollups are counts and keep no person.
- A visitor who withdraws consent (`owlee('consent', false)`) has their device forgotten on the server (6.4).

Follow-up in another spec: the [session replay design](2026-11-23-session-replay-design.md) D2 says analytics keeps
no per-visitor data. That is true only without identity mode.

## 13. API

| Route | Use |
|---|---|
| `GET/POST /api/insights/sites/{id}/funnels`, `GET/PUT/DELETE .../funnels/{funnel_id}` | saved funnels (write: editors) |
| `POST /api/insights/sites/{id}/funnels/query` | run a funnel definition |
| `GET /api/insights/sites/{id}/paths?start=&end=&depth=&from=&to=` | paths |
| `GET /api/insights/sites/{id}/retention?period=&periods=&event=` | retention |
| `GET /api/insights/sites/{id}/persons?q=&cursor=`, `GET/DELETE .../persons/{person_id}` | persons |
| `GET /api/insights/sites/{id}/features` | all switches of both specs, with the effective value and today's drop counts |
| `PATCH /api/insights/sites/{id}/features` | change one or more switches, for example `{ "funnels": false }` (editors) |
| `DELETE /api/insights/sites/{id}/identity/data` | delete identity data (editors) |
| `PUT /api/insights/sites/{id}/identity` | `{ require_signature }` (editors) |
| `POST /api/insights/sites/{id}/identity/secret` | show or rotate the signing secret (editors) |

- Each route loads the site through the same workspace check as the current analytics routes, then reads only
  rows with that `site_id`. A funnel or person id of another site is 404.
- `funnels/query` is a read-only POST because the definition is long. It passes through the same-origin layer.

## 14. Build order

1. Feature switches, the Features card, and the `feature_disabled` check. Built-in event class in `record.rs`,
   `begin_immediate` for ingest, `builtin` daily cap.
2. P0. Tracker consent and call API, device column, `$identify` with signature, persons, device days, `$forget`,
   new-device cap, retention.
3. P1. Query engine limits, funnel function, routes in `product.rs`, funnel page.
4. P2. Paths.
5. P4. People view, person page, privacy export and erasure, PRIVACY.md.
6. P3. Retention.

## 15. Tests

- Each switch off: its report route answers `403 feature_disabled`, and a switch that collects data stores nothing
  new. On again: the kept data shows again.
- Retention and People on with Identity mode off: not in effect, `403`. No stored value changes.
- Identity mode off: privacy requests still find and erase identity data.
- A `$` type not in the list is `400`. A built-in event does not change `insight_daily.events` or bounce.
- With identity off, `did`, `$identify`, and `$forget` make no device, person, or day row.
- Two concurrent `$identify` for the same new device and user give one person and one link, and both events are
  stored (SQLite and Postgres).
- `$identify` for a device linked to another person is dropped and changes nothing.
- With `require_signature`, a wrong `sig` is dropped. A right one links.
- `$forget` removes the device's days and link, and `device_key` on its events.
- Retention job: an old device day is deleted, then the link, then the person.
- Funnel: steps out of order do not count. A step at the same second as the step before counts. A step after the
  window does not count. `visit` does not join two visits. Median time for odd and even counts.
- Paths: a reload counts once. "Other" collects the values after the top 10.
- Retention: a person with two devices counts once per period, from the earliest device day.
- Query limit: a range over the limit answers `422`.
- Authorization: a funnel or person of site A read through site B is 404. Another workspace is 404.

## 16. Open questions

- Is 2,000,000 rows a good scan limit after the SQL filters? Measure a funnel on SQLite with 2,000,000 matching rows
  before release.
- Is 50,000 new devices per site per day a good default?

## 17. Decisions

| # | Decision | Date |
|---|---|---|
| D1 | Built-in events use a `$` prefix, which custom names cannot contain. They have their own class, do not change visit and bounce metrics, and have their own daily cap. | 2026-11-23 |
| D2 | The server enforces the identity switch and drops identity data when it is off. Settings shows the drop counts, so a snippet that does not match is visible. | 2026-11-23 |
| D3 | Identity is always consent-gated in the tracker: no device id before `owlee('consent', true)`. A site without a banner calls it at once. One snippet, no unsafe default. | 2026-11-23 |
| D4 | The visitor id stays the monthly IP hash in both modes. The device is a separate column, `device_key`. Current reports do not change, and consent in the middle of a visit does not split it. | 2026-11-23 |
| D5 | The device id lives in `localStorage`, not in a cookie, so it never goes to the site's own server. Each subdomain is a separate device until `identify` links them. | 2026-11-23 |
| D6 | Persons are resolved at read time through `insight_person_devices`. Events keep only `device_key`. Linking a device writes one row and rewrites no event, so there is no merge. | 2026-11-23 |
| D7 | User ids can be signed by the site's backend with an HMAC per site. Without signing, the UI states that persons can be forged. Privacy requests search `user_id` only, never data that a visitor can set. | 2026-11-23 |
| D8 | The first person linked to a device keeps it. A later `identify` for another user on that device is dropped. This stops a sender from moving a device into another user's timeline and keeps writes to inserts only. | 2026-11-23 |
| D9 | Analytics ingest uses `begin_immediate()`, and identity rows use `INSERT ... ON CONFLICT DO NOTHING` then a select. Concurrent first events cannot lose an event or make two persons. | 2026-11-23 |
| D10 | Reports filter in SQL, then compute in pure Rust functions in `crates/insights`, with a row limit, 2 concurrent reports, and a 30 s limit. Same code on SQLite and Postgres. | 2026-11-23 |
| D11 | Cohorts, person properties, and the Sankey diagram are cut from the first version. Paths use the existing `BarList`. | 2026-11-23 |
| D12 | Identity data (device days, links, persons) is kept 400 days after the last activity, so retention can cover 13 months while raw events stay at 90 days. | 2026-11-23 |
| D13 | Each feature has its own switch for each site. Off stops collection and hides the feature but keeps data until retention or an explicit delete. A switch that needs another one is in effect only when both are on, with no cascade writes. | 2026-11-23 |
| D14 | Funnels and paths are on by default because they collect nothing new. Every switch that collects new data is off by default. | 2026-11-23 |
