# Web analytics onboarding and UX design

Status: built.

## 1. Goal

A new user must get from "no sites" to "my first page view is here" in one short flow, and must trust that the
tracker works. Now the user creates a site, stays on the list, and must find the snippet under the last tab. The
domain verification card sits above every tab of every site, even after it is verified.

## 2. Problems

| # | Problem | Page |
|---|---|---|
| P1 | After create, the dialog closes on the list. The user must find the new site and then the snippet. | New site dialog |
| P2 | The name must be typed. A domain pasted as `https://coolify.io/blog` is not cleaned in the form. | New site dialog |
| P3 | A new site opens on an empty Overview. The setup steps (TXT record, snippet) are split between a card and the last tab. | Site detail |
| P4 | Nothing tells the user that the first page view arrived. | Site detail |
| P5 | The domain verification and quota card shows above the tabs forever. | Site detail |
| P6 | The snippet, the TXT name, and the TXT value have no copy button. | Site detail |
| P7 | The snippet uses the browser origin. A user who opens the dashboard on another host copies a wrong URL. | Site detail |
| P8 | The snippet is shown for plain HTML only. | Site detail |
| P9 | The list shows no traffic and no sign of a site that has no data yet. | Site list |
| P10 | Tab and range reset on reload and cannot be shared. | Site detail |
| P11 | The UI cannot rename or delete a site, or reset its data. The API can. | Site detail |

## 3. Design

### 3.1 Create flow

- Domain first. The form cleans it (scheme, path, port, and `www.` stay as the server's `clean_host` does) and
  fills the name from it until the user types a name.
- After create, the site page opens on its setup guide.

### 3.2 Setup guide

A site with no events opens on a "Setup" tab with numbered steps:

1. Verify the domain: the TXT name and value with copy buttons, and Verify. In development the step says that
   events are accepted before verification.
2. Add the tracker: the snippet with a copy button, with variants for plain HTML, Next.js, and other frameworks
   that render a `<head>`.
3. Wait for the first page view: a live panel polls the site every 3 seconds while it has no events, and then
   shows "First page view received" with a link to Overview.

When the site has events, it opens on Overview, and Setup is a normal tab.

### 3.3 Settings

A "Settings" tab replaces "Tracker". It holds the name, the domain verification state, the daily cap, the public
dashboard, "Reset data", and "Delete site". Delete and reset ask for confirmation.

### 3.4 List

Each row shows visitors and page views for 24 hours, and "Waiting for data" when the site has no events.

### 3.5 URL

Tab and range live in the URL. Defaults are left out.

## 4. API

The site view gains `tracker_url` (the script URL from `OWLEE_PUBLIC_ORIGIN`), `last_event_at`, `visitors_24h`,
and `pageviews_24h`.

## 5. Decisions

| # | Decision | Date |
|---|---|---|
| D1 | The server builds the tracker URL from `OWLEE_PUBLIC_ORIGIN`, the same origin that builds the Sentry DSN. The browser origin is not used. | 2026-11-20 |
| D2 | The setup guide is a tab, not a wizard, so the user can open Overview at any time. | 2026-11-20 |
| D3 | "First page view received" comes from polling the site view, as for error projects. Owlee has no realtime layer. | 2026-11-20 |
| D4 | Verification and quota move into Settings. Only an unverified site in production shows a warning above the tabs, because it drops events. | 2026-11-20 |
| D5 | The server refuses a domain that contains `/`. Before, `https://coolify.io/blog` was stored as `https`, because the port was cut at the first `:`. The form cleans a pasted URL before it sends it. | 2026-11-20 |
| D6 | The opening tab is chosen once per visit: Setup when the site has no events, else Overview. Only that tab is left out of the URL, so the first page view does not move the user off Setup. | 2026-11-20 |
| D7 | The list drops the public id column to make room for the 24-hour columns. The id stays in the snippet. | 2026-11-20 |
| D8 | `last_event_at` reads stored events. After retention deletes them, an idle site shows "Waiting for data" again. | 2026-11-20 |
