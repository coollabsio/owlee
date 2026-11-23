# Click and scroll analytics design (autocapture)

Status: draft. Nothing in this document is built yet. Reviewed with `/interrogate` on 2026-11-23. The fixes of that
review are in this version. The review also cut the on-site toolbar, the canvas heatmap, and rage and dead clicks.

## 1. Goal

Owlee analytics records page views and the custom events that a developer sends with code. A site owner cannot
see which links and buttons visitors use, or how far down a page they read, unless they write code for each
element.

This design adds:

- **Autocapture.** The tracker records clicks on links and buttons, form submits, outbound links, and downloads
  with no code. An HTML attribute makes a named custom event with no JavaScript.
- **Click report.** For each page, the elements that visitors click, with counts.
- **Scroll report.** For each page, the percent of page views that reach each 10 % of the page.

All of it is aggregate data with no visitor id. It works in cookieless mode. The built-in event rules (`$` prefix,
own class, own daily cap) and the feature switch rules come from the
[product analytics design](2026-11-23-product-analytics-design.md) §4 and §5.

The rules that Owlee has now stay the same:

- One Rust binary. SQLite is the default. Postgres uses the same schema and the same SQL.
- Every row belongs to a site, and a site belongs to a workspace. A user from another workspace gets 404.
- Ingest keeps its body limit, token buckets, origin check, domain verification, bot filter, and daily cap.

## 2. Facts that this design depends on

| Fact | Location |
|---|---|
| `/data.js` is `apps/server/src/insights/tracker.js`, embedded with `include_str!`, served with CORS `*`. | `apps/server/src/insights/routes.rs:31,110,136-147` |
| The tracker sends one `fetch` (`keepalive`, `credentials: 'omit'`) to `/api/receive` for each event. The payload reads `currentUrl` at send time. | `tracker.js:12,26,34-38` |
| On `pushState` or `replaceState`, the tracker changes `currentUrl` and sends a page view after 300 ms. | `tracker.js:141-157` |
| `/api/receive` limits: 8 KiB body, 60 requests per minute per IP (burst 30), 240 per minute per site (burst 60), daily cap per site. | `crates/platform/src/config.rs:28,67-70,115,120-123` |
| The server stamps the receive time in seconds. | `apps/server/src/insights/routes.rs:289` |
| Dimension rollups cap values at 1,000 per day with a `COUNT(*)` on each new value. Overflow goes to `(other)`. | `apps/server/src/insights/record.rs:253,283-292` |
| `scrub_text` filters secret shapes and card numbers. It has no email filter. `scrub_query` filters sensitive keys only. | `crates/errors/src/scrub.rs:18-44,108-121` |
| `insight_events.device` is desktop, mobile, or tablet, from the user agent. | `crates/insights/src/ua.rs:39-52` |

## 3. Scope

### In scope

- C0. Batched sends with per-event URL and time.
- C1. Autocapture of clicks on interactive elements, submits, outbound links, and downloads.
  `data-owlee-event` attributes.
- C2. Click report and scroll report in a "Clicks" tab.

### Not in scope

- An on-site toolbar and any overlay on the live page. A later spec can add it on top of the same rollups.
- Canvas heatmaps of click positions. Clicks are counted per element, not per position (D5).
- Rage clicks and dead clicks.
- Mouse movement and session replay. The [session replay design](2026-11-23-session-replay-design.md) puts replay on
  error apps.
- Autocapture on mobile apps.

## 4. Feature switches

These switches join the Features card and the `features` route of the product analytics design §4, with the same
rules: off stops collection and hides the feature, data is kept until retention or "Delete ... data", and a switch
that needs another one is in effect only when both are on.

```sql
ALTER TABLE insight_sites ADD COLUMN autocapture_enabled BIGINT NOT NULL DEFAULT 0;
ALTER TABLE insight_sites ADD COLUMN autocapture_text_enabled BIGINT NOT NULL DEFAULT 0;
ALTER TABLE insight_sites ADD COLUMN scroll_enabled BIGINT NOT NULL DEFAULT 0;
```

| Switch | Default | Needs | When off |
|---|---|---|---|
| Autocapture | off | – | The server drops `$click`, `$submit`, `$outbound`, and `$download`. The Elements part of the Clicks tab is hidden. "Delete click data" deletes those events and `insight_element_daily`. |
| Element text | off | Autocapture | The server drops the `text` field. "Delete element text" sets `text` to NULL in the event data and the rollup. |
| Scroll depth | off | – | The server drops `$pageleave`. The Scroll part of the Clicks tab is hidden. "Delete scroll data" deletes `insight_scroll_daily`. |

- The Clicks tab is shown when Autocapture or Scroll depth is on.
- `data-owlee-event` (5.2) makes normal custom events, so it works with every switch off.
- The tracker learns what to collect from the snippet: `data-autocapture="true"`, `data-scroll="true"`. The setup
  guide adds them when the switches are on. Element text is server-only. The server enforces every switch, and the
  Features card shows today's drop counts, so a snippet that does not match is visible (D2).

## 5. Autocapture (C1)

### 5.1 What the tracker captures

| Event type | When | Fields |
|---|---|---|
| `$click` | A click on an interactive element or inside one: `a`, `button`, `input[type=button\|submit\|reset\|checkbox\|radio]`, `select`, `summary`, `[role=button\|link\|tab\|menuitem]` | `el` |
| `$submit` | A `submit` event on a form | `el` of the form. Never field values. |
| `$outbound` | A click on an `http(s)` link to another host | `el`, `href` |
| `$download` | A click on a link with `download`, or a path that ends in a common file type (`pdf`, `zip`, `dmg`, `exe`, `csv`, `xlsx`, `docx`, `mp4`, ...) | `el`, `href` |
| `$pageleave` | Once for each page load: the first of `pagehide`, `visibilitychange` to hidden, or an SPA navigation. A page restored from the back-forward cache is a new page load. | `scroll` (max depth %, 0 to 100) |

`el` is:

```json
{ "tag": "button", "sel": "main > form#signup > button.btn-primary:nth-of-type(1)", "text": "Start free trial" }
```

- `sel` is a CSS selector from the element up to 5 ancestors: tag, `id`, at most 3 classes, and `:nth-of-type`
  when siblings have the same tag. At most 400 chars, below the 500-char limit of event data values.
- An `id` or class that looks generated is left out: 3 or more digits in a row, a UUID, a hex run of 8 or more,
  or a hash suffix such as `css-1x2y3z`. This keeps selectors stable across builds and keeps record ids such as
  `#user-12345` out (D4).
- `text` is the `innerText` of the interactive element, trimmed and collapsed, at most 100 chars. It is never read
  from `input`, `textarea`, `select`, or `[contenteditable]`. The tracker sends it only when the element is not
  inside `[data-owlee-no-capture]`. The server keeps it only when the Element text switch is in effect, and passes it through
  `scrub_text` and a new email filter in `crates/errors/src/scrub.rs` (D4).
- `href` is host and path only. Path segments that look like ids or tokens (the same rule as ids above, or 16 or
  more chars of `[A-Za-z0-9_-]` with a digit) become `:id`. `mailto:`, `tel:`, and `javascript:` links send no
  `href` and are not `$outbound`.
- Clicks inside a `[data-owlee-no-capture]` subtree send no `text` and no `href`.

### 5.2 Events without code

```html
<button data-owlee-event="signup" data-owlee-prop-plan="pro">Start</button>
```

- A click on an element with `data-owlee-event` sends a **custom** event with that name and the
  `data-owlee-prop-*` values as data. It works with autocapture off. It is the same as `owleeEvent(name, data)`.

### 5.3 Storage

- `$click`, `$submit`, `$outbound`, and `$download` are stored in `insight_events` as built-in events, with `el` and
  `href` in `insight_event_data`. A funnel step can match them, for example
  `{ "kind": "event", "name": "$click", "where": [{ "key": "sel", "op": "contains", "value": "#signup" }] }`.
- They do not change visit counts, event counts, or bounce, and the Events tab does not list them (product
  analytics D1).
- `$pageleave` is not stored as an event row. It feeds only the scroll rollup.
- Built-in events use the `builtin` daily cap, so they cannot use up the cap of page views and custom events.

## 6. Batched sends (C0)

- The tracker keeps `$` events in a queue. It flushes the queue every 5 s, at 20 events, on `pagehide`, and
  **before** it changes `currentUrl` on an SPA navigation. Page views, `performance`, and custom events are sent at
  once, as now.
- The body is `{ "type": "$batch", "site": ..., "events": [...] }` with the page-level fields once (`hostname`,
  `screen`, `language`). Each event has its own `type`, `url` (as it was when the event happened), and `age` (ms
  between the event and the send).
- The server sets the event time to `receive time - age`. An `age` below 0 or above 60,000 is clamped.
- At most 20 events. `OWLEE_ANALYTICS_MAX_BODY_BYTES` default grows from 8 KiB to 16 KiB.
- **Each event takes one token** from the IP and site buckets, and one unit of the `builtin` cap. When the buckets
  or the cap run out in the middle of a batch, the rest is dropped and the answer is `429` (D3).
- The defaults of the analytics buckets grow, because one page view can now bring several events:
  per site 1,200 per minute (burst 300), per IP 120 per minute (burst 60).
- One batch is one transaction (`begin_immediate`). Each event is checked alone. A bad event is dropped and
  counted, and the others are stored.
- The tracker never retries a send, so a batch is not counted twice.

## 7. Rollups (C2)

```sql
CREATE TABLE insight_element_daily (
    site_id TEXT NOT NULL REFERENCES insight_sites (id) ON DELETE CASCADE,
    day BIGINT NOT NULL,
    pathname TEXT NOT NULL,
    device TEXT NOT NULL,                 -- desktop | mobile | tablet
    selector TEXT NOT NULL,
    tag TEXT NOT NULL,
    text TEXT,                            -- the first text seen for this selector this day
    clicks BIGINT NOT NULL,
    PRIMARY KEY (site_id, day, pathname, device, selector)
);

CREATE TABLE insight_scroll_daily (
    site_id TEXT NOT NULL REFERENCES insight_sites (id) ON DELETE CASCADE,
    day BIGINT NOT NULL,
    pathname TEXT NOT NULL,
    device TEXT NOT NULL,
    depth_bucket BIGINT NOT NULL,         -- max scroll depth / 10, so 0 to 10
    views BIGINT NOT NULL,
    PRIMARY KEY (site_id, day, pathname, device, depth_bucket)
);
```

- `$click` updates `insight_element_daily` with an upsert. `$pageleave` updates `insight_scroll_daily`.
- Pathname: a path that went to `(other)` in today's `path` dimension rollup goes to `(other)` here too. No new
  count query is needed for paths.
- Selectors: at most 200 per path, device, and day. The count query runs only when an insert makes a new row. Over
  the cap, the click goes to selector `(other)`.
- The scroll table has at most 11 rows per path, device, and day, so it needs no cap.
- Both tables are deleted after `OWLEE_ANALYTICS_RETENTION_DAYS`, not kept like the other rollups. They describe
  the current layout of a page, and `insight_element_daily` holds element text (D6).

## 8. Clicks tab (C2)

- A path picker with the top paths by page views, a path match (`exact`, `prefix`, or `glob`, the same rules as
  funnel steps), a device filter, and the date range.
- **Elements**: a table of tag, text (when kept), selector, clicks, and click share. Sort on each column (shared
  `DataTable`).
- **Scroll**: one bar for each 10 % of depth with the percent of page views that reached it. Under the chart:
  "Based on N page leaves out of M page views." M comes from the `path` dimension rollup. The percent uses N,
  because a page view whose leave event was lost has no depth.
- The routes live in a new module `apps/server/src/insights/clicks.rs`, not in `routes.rs`.

## 9. Privacy

PRIVACY.md changes:

- Autocapture stores the tag and a selector of the clicked link or button. Ids and classes that look generated are
  left out. It stores the visible text of the element only when the site turns on text capture. Emails, card
  numbers, and secrets in that text become `[Filtered]`. It never stores input values.
- Outbound and download links are stored as host and path. Path parts that look like ids or tokens become `:id`.
- The click and scroll rollups hold no visitor id, and they are kept for the raw retention (90 days by default).
  The retention table gets these rows.
- `data-owlee-no-capture` stops text and link capture inside an element.

## 10. API

| Route | Use |
|---|---|
| `PATCH /api/insights/sites/{id}/features` | the switches of §4, for example `{ "autocapture": true }` (editors). Defined in the product analytics design. |
| `DELETE /api/insights/sites/{id}/clicks/data?part=clicks\|text\|scroll` | the delete buttons of §4 (editors) |
| `GET /api/insights/sites/{id}/clicks/elements?path=&match=&device=&from=&to=` | element table |
| `GET /api/insights/sites/{id}/clicks/scroll?path=&match=&device=&from=&to=` | scroll buckets with N and M |

Each route loads the site through the same workspace check as the current analytics routes.

## 11. Build order

1. C0. Switches of §4 on the shared Features card. `$batch` in the tracker and on `/api/receive`, per-event URL and age, per-event tokens, body limit, new
   bucket defaults. Needs the built-in event class of the product analytics design.
2. C1. Site switches, autocapture in the tracker, selector builder, `href` cleaning, `data-owlee-event`, email
   filter, server checks and storage.
3. C2. Rollups, retention, Clicks tab, PRIVACY.md.

## 12. Tests

- With Autocapture off, `$click` is dropped. With Scroll depth off, `$pageleave` is dropped. Off keeps the stored
  rows, and the delete route removes them. A custom event named
  `click` or `download` is stored as custom in every case.
- With Element text off, or on while Autocapture is off, no `text` is stored.
- Batch: 21 events is `400`. A bad event in a batch is dropped, and the others are stored. A bucket with 3 tokens
  left stores 3 of 5 events and answers `429`.
- Batch time: an event with `age = 4000` gets a time 4 s before the receive time. A click queued on `/a` and sent
  after a navigation to `/b` is stored on `/a`.
- Selector builder (Bun test with a DOM): generated ids and classes are left out, `nth-of-type` only with same-tag
  siblings, at most 400 chars, no text from `input` or from a `[data-owlee-no-capture]` subtree.
- `href` cleaning: `/reset/8f3a9c...` becomes `/reset/:id`. `mailto:` sends no `href`.
- Text with an email and a card number is stored as `[Filtered]` for both.
- Rollup: two clicks on the same selector give one row with `clicks = 2`. The 201st selector goes to `(other)`.
- Scroll: one page load sends one `$pageleave`, also when it is hidden and shown again.
- Authorization: a site of another workspace is 404.

## 13. Open questions

- Are 1,200 per minute per site and 120 per minute per IP good bucket defaults with autocapture on? Measure on a
  real site in C0.

## 14. Decisions

| # | Decision | Date |
|---|---|---|
| D1 | Autocapture and scroll data are aggregate and need no visitor id. They work in cookieless mode. | 2026-11-23 |
| D2 | The server enforces the site switches and drops what is off. The snippet attributes only save tracker work. Settings shows the drop counts, so a mismatch is visible. | 2026-11-23 |
| D3 | A batch carries each event's URL and age, and each event takes one bucket token and one unit of the `builtin` cap. Batching saves requests but does not weaken the limits, and a queued click stays on the page where it happened. | 2026-11-23 |
| D4 | Text capture is off by default and only from links and buttons. Generated ids and classes and id-like path parts are removed. Names in element text are not filtered, so the owner must turn text on. | 2026-11-23 |
| D5 | Clicks are counted per element, not per position. Position maps smear on centered layouts and need an overlay to read. A selector table answers "what do visitors click" without one. | 2026-11-23 |
| D6 | Click and scroll rollups follow raw retention (90 days), not the keep-forever rule of the other rollups. Old layouts make old data wrong, and the rows can hold element text. | 2026-11-23 |
| D7 | The on-site toolbar, canvas heatmaps, and rage and dead clicks are cut from the first version. The Clicks tab shows the same element and scroll data without a token flow on customer pages. | 2026-11-23 |
