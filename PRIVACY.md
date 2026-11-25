# Privacy

Owlee is self-hosted. You, the operator, are the data controller. Owlee does not send this data to its authors. This page lists what an install stores and how to answer requests from the people in that data.

## Analytics

The tracker sets no cookies and uses no local storage.

Each event stores the page path and query string (sensitive parameters filtered, as in error events, including in the `#fragment`), page title, hostname, UTM tags, referrer domain, browser, operating system, device type, screen size, language, Web Vitals, and any custom event data your site sends.

The visitor's IP address is never stored. Owlee hashes the site, the IP address, the user agent, and a random salt into a visitor id. The salt changes every calendar month (UTC). Owlee deletes the old salt within an hour of the next month starting. After that, nobody can recompute last month's visitor ids from an IP address, not even someone with database access.

Location comes from Cloudflare headers or a GeoIP database, if you configure one. `OWLEE_ANALYTICS_GEO` sets how much is kept:

| Value | Stored |
| --- | --- |
| `city` (default) | Country, region, and city |
| `region` | Country and region |
| `country` | Country |
| `off` | No location |

Analytics data cannot be tied to a person, so an access or erasure request has nothing to return for it.

## Errors

Error events come from the Sentry SDKs in your apps. Before an event is stored, Owlee removes:

- `user.ip_address`, `request.cookies`, and `request.env.REMOTE_ADDR`.
- The client-address headers `X-Forwarded-For`, `X-Real-IP`, `Forwarded`, `CF-Connecting-IP`, and `True-Client-IP`. The value becomes `[Filtered]`.
- The value of any field whose name contains `password`, `passwd`, `secret`, `token`, `api_key`, `apikey`, `authorization`, `cookie`, `session`, `csrf`, `xsrf`, `credit_card`, `card_number`, `private_key`, or `access_key`, or is exactly `auth`. Case, `-`, and `_` do not matter. This covers headers, query strings, request bodies, and stack-frame variables. The value becomes `[Filtered]`.

`user.id`, `user.email`, and `user.username` stay in the stored event. Your SDK sends them only if you set them. Leave them out if you do not need them.

Owlee keeps a keyed hash of each of `user.id`, `user.email`, `user.username`, and `user.ip_address` that the event has. Values are lowercased before hashing. The key is `OWLEE_APP_KEY`, so the hashes cannot be reversed with a list of emails or addresses. The first hash also counts affected users.

## Session replay

The Sentry browser SDK can record replays of your pages. Owlee stores them only for an app where an editor turned on **Store replays**.

- A replay holds what the user saw on the page and what they did, as far as the SDK did not mask it. Owlee cannot check the masking. Keep the SDK defaults: `maskAllText`, `maskAllInputs`, and `blockAllMedia`.
- Owlee stores no IP address for a replay.
- The replay list shows `user.username`, else `user.email`, else `user.id`, as error events keep them. Owlee also keeps a keyed hash of each of them, as for error events.
- Owlee filters sensitive query parameters in the URLs it lists for a replay. It does not change the recording, so URLs, clicks, and page text inside the recording stay as the SDK sent them.

## Telemetry

Logs come from the OpenTelemetry SDKs and Collectors you point at a telemetry source. Before a log is stored, Owlee:

- removes the attributes `client.address`, `source.address`, and `net.peer.ip`, in the record and in its resource;
- replaces the value of an attribute whose name matches the sensitive names listed for errors with `[Filtered]`, at any depth;
- filters bearer and basic credentials, JWTs, AWS access key ids, PEM private keys, and card numbers in the body and in every attribute value, as for errors.

Everything else stays as the SDK sent it, such as user ids or emails that your code puts in a log line. Telemetry has no user key, so it is not part of the access and erasure requests below. Delete telemetry by deleting its source, or let retention remove it.

## Retention

| Data | Kept for |
| --- | --- |
| Error events | `OWLEE_ERROR_RETENTION_DAYS`, 30 days by default |
| Analytics events, visits, and visitor ids | `OWLEE_ANALYTICS_RETENTION_DAYS`, 90 days by default |
| Session replays | `OWLEE_REPLAY_RETENTION_DAYS`, 14 days after the last part arrived |
| Logs | `OWLEE_LOG_RETENTION_DAYS`, 7 days by default |
| Issue rows and analytics daily rollups | Until you delete the project or site |

Issues and daily rollups hold counts, titles, and top values such as pages and countries. They hold no visitor ids or user keys.

## Access and erasure requests

Sign in as an admin and open **Settings → Privacy requests**. Enter the id, email, username, or IP address the person's events were sent with. Letter case does not matter.

- **Export JSON** downloads every stored error event for that value across all projects. It also lists the session replays with that user and a download link for each part of the recording.
- **Delete events** removes those events and those replays. Issue counts and issue titles stay. An issue title comes from the exception message, so check it if your messages can contain personal data.
- A replay with no user fields cannot be found by a request. Delete it from the replay page.

The same actions are available as `GET` and `DELETE /api/privacy/errors?user=<value>`.
