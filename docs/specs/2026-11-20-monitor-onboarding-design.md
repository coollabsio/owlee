# Monitor onboarding and UX design

Status: built.

## 1. Goal

A new user must get from "no monitors" to "my first check is green, and I get an alert when it fails" in one
short flow. Now the user fills one long form with every field and saves without knowing if the check works.
The dialog closes on the list, and the first result comes on the next scheduler tick. Nothing tells the user that
alerts, status pages, and incidents connect to the monitor.

## 2. Problems

| # | Problem | Page |
|---|---|---|
| P1 | The create form shows all 12 fields at once. Most users need only the URL. | New monitor dialog |
| P2 | The user cannot test a check before saving. A wrong URL or status shows only after the first tick. | New monitor dialog |
| P3 | After save, the dialog closes on the list. The first check waits for the scheduler. | New monitor dialog |
| P4 | `example.com` without a scheme fails. The name must be typed even when the URL says it. | New monitor dialog |
| P5 | Switching the type to TCP keeps `https://` in the target field. | New monitor dialog |
| P6 | The detail page does not show the open incident, the alert channels, or the status pages of the monitor. | Monitor detail |
| P7 | A paused monitor shows its last state in the list, not "Paused". | Monitor list |
| P8 | Check times are absolute dates. | Monitor detail |

## 3. Design

### 3.1 Create flow

- One field first: the URL or `host:port`. The type comes from the input: a value with `://` or a bare domain is
  HTTP (`https://` is added), `host:port` with a non-HTTP port is TCP. The user can still change the type.
- The name fills in from the host while the user has not typed a name.
- "Advanced" holds method, expected status, timeout, degraded after, failures before down, and body contains.
  Interval and project stay visible.
- "Test" runs the check once on the server without saving and shows the result: state, status code, latency, and
  error. Save does not require a test.
- After save, Owlee runs the first check at once and opens the monitor detail page.

### 3.2 Monitor detail

- A "Setup" card shows what is connected: alert channels, status pages, and the open incident. Each missing part
  has a link to add it. Editors see the links; members see the state only.
- Check times are relative, with the full time on hover.

### 3.3 Monitor list

- A paused monitor shows "Paused" as its state and sorts after active monitors.

## 4. API

| Method | Path | Body | Result |
|---|---|---|---|
| POST | `/api/monitors/test` | the monitor create body | one check result, not stored |

The monitor detail view gains `open_incident_id`, `alert_channels` (`id`, `name`, `provider`),
`alert_channel_count`, and `status_pages` (`id`, `title`, `slug`). The field names match the existing Channel and
StatusPage types. Each list is limited to the caller's workspace. The list endpoint does not carry these fields.

The test endpoint returns `status` (success, degraded, or failure), `status_code`, `latency_ms`, and `error`, the
same shape as a stored check.

## 5. Security

`/api/monitors/test` is for editors only and uses the same validation and checker as a saved monitor. It gives no
new reach: an editor can already save a monitor for the same target and read its result. "Run check" had no
rate limit. Both now share one limit per user (D7).

## 6. Decisions

| # | Decision | Date |
|---|---|---|
| D1 | The URL is the only required input. The name and the type come from it, and the user can change both. | 2026-11-20 |
| D2 | Test before save is optional. Save never waits for a test, so an endpoint that is down now can still be monitored. | 2026-11-20 |
| D3 | The test endpoint takes the same body as create and does not write rows, so one validator covers both. | 2026-11-20 |
| D4 | After save, the server runs the first check at once and waits up to 5 seconds for it. A slower target keeps checking in the background, and the detail page shows Pending until the result is stored. Waiting the full 30-second timeout would block the save. | 2026-11-20 |
| D5 | The detail page shows connections (alerts, status pages, incident) as a checklist, not a wizard. A wizard hides the monitor until every step is done. | 2026-11-20 |
| D6 | Agent-managed monitors keep their current read-only detail page. Their config lives in the agent file. | 2026-11-20 |
| D7 | "Run check" and "Test" share one rate limit per user: 30 a minute, with a burst of 15. | 2026-11-20 |
| D8 | Members get no alert channel names (channels hold webhook URLs), only `alert_channel_count`. The open incident link shows for members, because they can read incidents. | 2026-11-20 |
| D9 | The description field moves into Advanced. | 2026-11-20 |
