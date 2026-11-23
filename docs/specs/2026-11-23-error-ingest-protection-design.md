# Error ingest protection design

Status: built.

## 1. Goal

The DSN key of an error app is public in browser code. Today Owlee guards the ingest endpoint with body limits, a
token bucket per key and per IP, and a fixed daily cap. Three gaps stay open:

| # | Gap | Effect |
|---|---|---|
| G1 | No spike protection. | One crash loop or one flood spends the whole daily cap in minutes. The app is blind for the rest of the day. |
| G2 | No allowed domains. | Any web page can post events with a copied DSN. |
| G3 | No inbound filters. | Crawlers, old browsers, browser extensions, and local development fill issues with noise and spend the cap. |

Sentry has all three ([spike protection](https://docs.sentry.io/product/accounts/quotas/spike-protection),
[inbound filters](https://docs.sentry.io/concepts/data-management/filtering/),
[allowed domains](https://docs.sentry.io/product/relay/projects)). This design adds them to Owlee error apps.

## 2. Pipeline order

Each check runs at the earliest point where its input exists. A cheaper or more general check runs first.

| Step | Check | Refusal | Counts against |
|---|---|---|---|
| 1 | IP token bucket (exists) | `429` | nothing |
| 2 | Body size and decode (exists) | `413` / `400` | nothing |
| 3 | Key (exists) | `401` / `404` | nothing |
| 4 | **Allowed domains** | `403`, drop reason `origin_not_allowed` | nothing |
| 5 | App token bucket (exists) | `429` | nothing |
| 6 | Parse and normalize the event (exists) | `400` | nothing |
| 7 | **Inbound filters** | `200` with no id, drop reason `filtered` | nothing |
| 8 | Daily cap (exists) | `429` | nothing |
| 9 | **Spike protection** | `429`, drop reason `spike_protection` | nothing |
| 10 | Store | `200` with the event id | hourly and daily counters |

Allowed domains run before the app bucket, as in analytics, so other web pages cannot use up the bucket.

## 3. Allowed domains

- Column `error_apps.allowed_domains TEXT NOT NULL DEFAULT ''`. One host per line. Empty means every origin.
- A host entry matches the same host and every subdomain of it, as in analytics (`example.com` allows
  `app.example.com`). `*` alone means every origin.
- The host comes from `Origin`, else from `Referer`. A request with neither header passes, because server SDKs send
  neither.
- `localhost`, `127.0.0.1`, and `::1` pass when `OWLEE_INGEST_ALLOW_LOCALHOST` (the analytics setting) is on.
- The host parser and matcher move out of `insights/routes.rs` into one shared module that both callers use.
- Input is cleaned with the same host cleaner as analytics domains. A URL such as `https://shop.example.com/path` is
  stored as `shop.example.com`. Invalid entries are a `422` field error (D12).

## 4. Inbound filters

- Column `error_apps.inbound_filters TEXT NOT NULL DEFAULT 'browser_extensions,web_crawlers'`. A comma list of
  filter ids. Unknown ids are ignored when read and refused when written.
- The filters are one registry: an enum with an id, a label, a description, and a predicate over the normalized
  event. Adding a filter needs no migration.

| Id | Label | Matches |
|---|---|---|
| `browser_extensions` | Browser extension errors | The crashing frame (last frame of the last exception) has a path that starts with `chrome-extension://`, `moz-extension://`, `safari-extension://`, `safari-web-extension://`, or `ms-browser-extension://`. Or the exception value or message contains a known extension pattern (Sentry's list: `top.GLOBALS`, `originalCreateNotification`, `canvas.contentDocument`, `MyApp_RemoveAllHighlights`, `atomicFindClose`, `conduitPage`, `Can't find variable: ZiteReader`, `jigsaw is not defined`, `ComboSearch is not defined`, `loading.retry.widdit.com`, `NS_ERROR_NOT_CONNECTED`). |
| `web_crawlers` | Web crawlers | The User-Agent is a bot by the `isbot` crate that analytics uses. |
| `localhost` | Localhost | The event `request.url` host is `localhost`, `127.0.0.1`, or `::1`, or `user.ip_address` is `127.0.0.1` or `::1`. |
| `legacy_browsers` | Legacy browsers | Internet Explorer (`MSIE` or `Trident/`), Opera Presto (`Presto/`), Chrome or Chromium below 80, Firefox below 78, Safari below 14 (by `Version/`), Edge Legacy (`Edge/`). |

- The User-Agent is the `User-Agent` of the event's `request.headers`. Browser SDKs put it there. The HTTP request
  header is not read (D10).
- A filtered event answers `200 {}`. The SDK must not retry or back off. It counts as `dropped` with reason
  `filtered` in the in-memory intake counts, so the app page shows it.

## 5. Spike protection

Spike protection limits the events one app stores in one UTC hour. It keeps a storm from spending the daily cap in
the first hour, so the app still records new issues for the rest of the day.

- Column `error_apps.spike_protection BIGINT NOT NULL DEFAULT 1`. A per-app switch.
- Table `error_hourly_usage (app_id TEXT REFERENCES error_apps ON DELETE CASCADE, hour BIGINT, count BIGINT,
  PRIMARY KEY (app_id, hour))`. `hour` is the Unix second of the UTC hour start. It counts stored events only.

The threshold for the current hour:

```
mean  = sum(count of the 168 hours before this hour) / 168      (missing hours are 0)
sd    = sqrt(sum(count²) / 168 - mean²)
floor = ceil(daily cap / 24)
limit = max(floor, ceil(3 * mean + 3 * sd))
```

- `floor` lets a quiet or new app store a full hour's share of its daily cap. A steady storm then spends the cap
  over the whole day, not in its first minutes. A new app needs no warm-up, because the floor applies at once.
- `3 * mean + 3 * sd` gives a busy app room for its normal peak hours. An app with steady traffic peaks near
  `2-3 * mean`. A bursty app has a larger `sd` and gets more room.
- Only stored events count, so a spike that was cut off adds at most `limit` to one hour. One such hour moves
  `mean + sd` by a small part of `limit`. The limit cannot ratchet up from its own refusals.
- One aggregate query reads `sum(count)` and `sum(count * count)` per event. It scans at most 168 indexed rows. No
  in-memory cache until a measurement asks for one.
- The hourly row is reserved with the same conditional upsert as the daily counter
  (`... DO UPDATE SET count = count + 1 WHERE count < limit RETURNING count`), in the same transaction as the
  daily reserve. The daily reserve runs first, and a spike refusal rolls it back (D11).
- A refusal answers `429` with `Retry-After` set to the seconds until the next UTC hour and
  `X-Sentry-Rate-Limits: <secs>:error:project:spike_protection`, so Sentry SDKs back off until then.
- Retention deletes `error_hourly_usage` rows older than 8 days.

## 6. API and UI

- `PUT /api/error-apps/{id}/protection` with `{ allowedDomains: string[], inboundFilters: string[],
  spikeProtection: boolean }`. Editors only, same authorization as `PUT /api/error-apps/{id}/quota`.
- The app view returns the same three fields, the filter registry (id, label, description), and the current hour's
  spike limit and stored count.
- The app page gets one "Ingest protection" card after the Daily cap card: a textarea for allowed domains (one per
  line), one switch per inbound filter with its description, and a spike protection switch that shows
  "N of LIMIT this hour".
- The intake summary labels the new drop reasons: "origin not allowed", "inbound filter", "spike protection".

## 7. Decisions

| # | Decision | Date |
|---|---|---|
| D1 | Spike limit is `max(ceil(cap / 24), ceil(3 * mean + 3 * sd))` over the 168 hours before the current UTC hour. Sentry uses a weighted hourly average of the past week times a factor of 3 to 6 from the standard deviation, with a floor from the quota. Owlee keeps the same inputs with fixed constants, so the limit can be explained on the app page. | 2026-11-23 |
| D2 | Hourly counts live in the database, not in memory. A restart must not reset the baseline, or busy apps get refused after every deploy. | 2026-11-23 |
| D3 | Only stored events enter the baseline. Sentry weights dropped events and decays them. Owlee does not need that, because its floor and sd term already cover recovery. | 2026-11-23 |
| D4 | Allowed domains default to empty (every origin), as Sentry's `*`. A request without `Origin` and `Referer` passes, so server SDKs work. The check stops other web pages, not scripts, because a script can forge `Origin`. | 2026-11-23 |
| D5 | Inbound filters are a registry stored as a comma list, not one column per filter. A new filter needs no migration. | 2026-11-23 |
| D6 | New apps get `browser_extensions` and `web_crawlers` on. `localhost` and `legacy_browsers` are off, because they can hide real errors a developer wants to see. | 2026-11-23 |
| D7 | A filtered event answers `200`, not `403` or `429`. The SDK must not retry or back off for noise. | 2026-11-23 |
| D8 | Filtered and spike-refused events do not spend the daily cap, as in Sentry. | 2026-11-23 |
| D9 | Order: allowed domains before the app bucket, inbound filters after normalize, spike before the daily cap. Superseded in part by D11. | 2026-11-23 |
| D10 | The crawler and legacy browser filters read only the event's `request.headers` User-Agent, not the HTTP header. Browser SDKs put the end user's agent in the event. The HTTP header of a server SDK or a script is often an HTTP library that `isbot` lists (`curl`, `guzzlehttp`, `okhttp`, `python-requests`, `go-http-client`), so with `web_crawlers` on by default, the app page's own curl test and some server SDKs would be dropped without a word. Sentry's filter also reads the event. | 2026-11-23 |
| D11 | The daily reserve runs before the hourly one. When the cap is spent, the SDK gets the back-off until midnight, not a one-hour back-off it would retry into another refusal. With spike first, an app with a cap of 1 (spike floor 1) answered `spike_protection` for its spent cap. A spike refusal rolls back the daily increment, so a refused event still spends neither counter. | 2026-11-23 |
| D12 | Invalid protection input is `422` with a field error, as every `AppError::field` in Owlee, not `400`. | 2026-11-23 |
