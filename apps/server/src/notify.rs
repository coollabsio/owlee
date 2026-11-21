use std::sync::{Arc, Mutex};
use std::time::Duration;

use owlee_domain::{
    MonitorState, NotificationEvent, NotificationPayload, PayloadMonitor, sign_body,
};
use owlee_platform::{Id, TimestampMillis};
use serde_json::Value;

use crate::repo::{self, Channel, Monitor};
use crate::state::AppState;

/// Sends to the channels of the workspace that owns `project_id`.
pub async fn fanout(
    state: &AppState,
    payload: &NotificationPayload,
    project_id: Id,
    monitor_id: Option<Id>,
    incident_id: Option<Id>,
) {
    let channels =
        match repo::channels_for_monitor(&state.db, &state.config.app_key, project_id, monitor_id)
            .await
        {
            Ok(channels) => channels,
            Err(error) => {
                tracing::error!(%error, "loading notification channels");
                return;
            }
        };
    for channel in channels {
        deliver(state, &channel, payload, monitor_id, incident_id).await;
    }
}

/// A started attempt holds its delivery this long. A process that dies
/// mid-send leaves the delivery for a retry after it.
const LEASE_MS: i64 = 120_000;

/// After attempt `n` fails, attempt `n + 1` waits `RETRY_DELAYS_MS[n - 1]`.
/// The attempt after the last delay is final, so a delivery gets four.
const RETRY_DELAYS_MS: [i64; 3] = [60_000, 300_000, 1_800_000];

/// Records the delivery, then makes the first attempt. A failure that may
/// pass, such as a timeout or HTTP 503, is retried from the scheduler.
pub async fn deliver(
    state: &AppState,
    channel: &Channel,
    payload: &NotificationPayload,
    monitor_id: Option<Id>,
    incident_id: Option<Id>,
) {
    let id = Id::new();
    let now = TimestampMillis::now();
    let recorded = match serde_json::to_string(payload) {
        Ok(body) => repo::insert_pending_delivery(
            &state.db,
            id,
            channel.id,
            monitor_id,
            incident_id,
            &payload.event,
            &body,
            now,
            TimestampMillis::from_millis(now.as_millis() + LEASE_MS),
        )
        .await
        .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = recorded {
        tracing::error!(%error, "recording notification delivery");
        return;
    }
    attempt(state, channel, payload, id, 1, now).await;
}

/// Sends the pending deliveries that are due at `now`. Each one is claimed
/// first, so two schedulers never send the same attempt.
pub async fn retry_due(state: &AppState, now: TimestampMillis) -> Result<usize, sqlx::Error> {
    let due = repo::due_deliveries(&state.db, now.as_millis(), 20).await?;
    let mut sent = 0;
    for delivery in due {
        let lease_until = now.as_millis() + LEASE_MS;
        if !repo::claim_delivery(
            &state.db,
            delivery.id,
            delivery.next_attempt_at,
            lease_until,
        )
        .await?
        {
            continue;
        }
        let Some(channel) =
            repo::channel(&state.db, &state.config.app_key, delivery.notification_id).await?
        else {
            continue;
        };
        if !channel.enabled {
            repo::settle_delivery(
                &state.db,
                delivery.id,
                "skipped",
                Some("the channel was turned off before the retry"),
                None,
            )
            .await?;
            continue;
        }
        let payload = match serde_json::from_str::<NotificationPayload>(&delivery.payload_json) {
            Ok(payload) => payload,
            Err(error) => {
                let reason = format!("the stored payload does not parse: {error}");
                repo::settle_delivery(&state.db, delivery.id, "failed", Some(&reason), None)
                    .await?;
                continue;
            }
        };
        attempt(
            state,
            &channel,
            &payload,
            delivery.id,
            delivery.attempts + 1,
            now,
        )
        .await;
        sent += 1;
    }
    Ok(sent)
}

/// `number` counts from 1.
async fn attempt(
    state: &AppState,
    channel: &Channel,
    payload: &NotificationPayload,
    id: Id,
    number: i64,
    now: TimestampMillis,
) {
    let retry_after = |failure: &Failure| {
        let delay = usize::try_from(number - 1)
            .ok()
            .and_then(|index| RETRY_DELAYS_MS.get(index))?;
        let test = payload.event == NotificationEvent::Test.as_str();
        (failure.transient && !test).then(|| TimestampMillis::from_millis(now.as_millis() + delay))
    };
    let (status, error, next) = match send(state, channel, payload).await {
        Ok(SendOutcome::Sent) => ("sent", None, None),
        Ok(SendOutcome::Skipped(reason)) => ("skipped", Some(reason), None),
        Err(failure) => match retry_after(&failure) {
            Some(next) => ("pending", Some(failure.reason), Some(next)),
            None => ("failed", Some(failure.reason), None),
        },
    };
    if let Err(error) = repo::settle_delivery(&state.db, id, status, error.as_deref(), next).await {
        tracing::error!(%error, "recording notification delivery");
    }
}

enum SendOutcome {
    Sent,
    Skipped(String),
}

/// `transient` failures may pass on a later attempt. The rest, such as a
/// bad URL or HTTP 404, fail the same way every time.
struct Failure {
    reason: String,
    transient: bool,
}

impl Failure {
    fn transient(reason: String) -> Self {
        Self {
            reason,
            transient: true,
        }
    }

    fn http(status: reqwest::StatusCode, what: &str) -> Self {
        let transient = status.is_server_error()
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status == reqwest::StatusCode::REQUEST_TIMEOUT;
        Self {
            reason: format!("{what} returned HTTP {status}"),
            transient,
        }
    }
}

impl From<String> for Failure {
    fn from(reason: String) -> Self {
        Self {
            reason,
            transient: false,
        }
    }
}

async fn send(
    state: &AppState,
    channel: &Channel,
    payload: &NotificationPayload,
) -> Result<SendOutcome, Failure> {
    let Some(config) = &channel.config else {
        return Err(Failure::from(
            "the channel settings do not open with OWLEE_APP_KEY. Edit the channel and enter its URL again."
                .to_owned(),
        ));
    };
    let (subject, text) = plain(payload);
    match channel.provider.as_str() {
        "email" => {
            let Some(smtp) = effective_smtp(state).await else {
                return Ok(SendOutcome::Skipped(
                    "SMTP is not configured. Save a mail server in Admin, or set OWLEE_SMTP_HOST and OWLEE_SMTP_FROM.".to_owned(),
                ));
            };
            let to = config
                .get("to")
                .and_then(Value::as_str)
                .ok_or_else(|| "email channel is missing a recipient".to_owned())?;
            send_email(&smtp, to, &subject, &text).await?;
            Ok(SendOutcome::Sent)
        }
        "webhook" => {
            let url = webhook_url(state, config.get("url"))?;
            let body = serde_json::to_vec(payload).map_err(|error| error.to_string())?;
            let mut request = webhook_client(state)
                .post(url)
                .timeout(Duration::from_secs(10))
                .header("content-type", "application/json")
                .body(body.clone());
            if let Some(secret) = config.get("secret").and_then(Value::as_str) {
                if !secret.is_empty() {
                    let signature = sign_body(secret, &body);
                    let header = format!("sha256={signature}");
                    request = request.header("x-owlee-signature", header);
                }
            }
            let response = request.send().await.map_err(request_error)?;
            if !response.status().is_success() {
                return Err(Failure::http(response.status(), "webhook"));
            }
            Ok(SendOutcome::Sent)
        }
        "slack" => {
            let url = webhook_url(state, config.get("url"))?;
            post_json(state, url, slack_body(&text)).await?;
            Ok(SendOutcome::Sent)
        }
        "discord" => {
            let url = webhook_url(state, config.get("url"))?;
            post_json(state, url, discord_body(&text)).await?;
            Ok(SendOutcome::Sent)
        }
        other => Err(Failure::from(format!("unknown provider {other}"))),
    }
}

/// Names, titles, and culprits come from users and from public error ingest.
/// Slack reads `<!channel>` and `<@U1>` as mentions, so escape what it parses.
fn slack_body(text: &str) -> Value {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    serde_json::json!({ "text": escaped })
}

/// An empty `parse` list turns off `@everyone`, `@here`, and role and user pings.
fn discord_body(text: &str) -> Value {
    let content = text.chars().take(1900).collect::<String>();
    serde_json::json!({ "content": content, "allowed_mentions": { "parse": [] } })
}

async fn post_json(state: &AppState, url: &str, body: serde_json::Value) -> Result<(), Failure> {
    let response = webhook_client(state)
        .post(url)
        .timeout(Duration::from_secs(10))
        .json(&body)
        .send()
        .await
        .map_err(request_error)?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(Failure::http(response.status(), "endpoint"))
    }
}

pub(crate) async fn effective_smtp(state: &AppState) -> Option<owlee_platform::SmtpConfig> {
    match crate::instance_settings::load(&state.db, &state.config.app_key).await {
        Ok(settings) => settings
            .smtp
            .map(owlee_platform::SmtpConfig::from)
            .or_else(|| state.config.smtp.clone()),
        Err(error) => {
            tracing::error!(%error, "loading instance mail server");
            state.config.smtp.clone()
        }
    }
}

#[derive(Clone, Debug)]
pub struct SentMail {
    pub to: String,
    pub subject: String,
    pub text: String,
}

pub type Outbox = Arc<Mutex<Vec<SentMail>>>;

/// Registration and recovery mail. Sent in the background, so the response
/// time does not tell whether the address has an account.
pub(crate) async fn send_account_mail(state: &AppState, to: &str, subject: &str, text: String) {
    let Some(smtp) = effective_smtp(state).await else {
        tracing::warn!(
            subject,
            "no mail server is configured, so an account mail was not sent"
        );
        return;
    };
    let mail = SentMail {
        to: to.to_owned(),
        subject: subject.to_owned(),
        text,
    };
    if let Some(outbox) = &state.outbox {
        outbox
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(mail);
        return;
    }
    tokio::spawn(async move {
        if let Err(failure) = send_email(&smtp, &mail.to, &mail.subject, &mail.text).await {
            let error = failure.reason;
            tracing::warn!(%error, subject = mail.subject, "sending an account mail failed");
        }
    });
}

async fn send_email(
    smtp: &owlee_platform::SmtpConfig,
    to: &str,
    subject: &str,
    text: &str,
) -> Result<(), Failure> {
    use lettre::message::header::ContentType;
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

    let email = Message::builder()
        .from(
            smtp.from
                .parse()
                .map_err(|error: lettre::address::AddressError| error.to_string())?,
        )
        .to(to
            .parse()
            .map_err(|error: lettre::address::AddressError| error.to_string())?)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(text.to_owned())
        .map_err(|error| error.to_string())?;
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host)
        .map_err(|error| error.to_string())?
        .port(smtp.port);
    if let (Some(username), Some(password)) = (&smtp.username, &smtp.password) {
        builder = builder.credentials(Credentials::new(username.clone(), password.clone()));
    }
    builder.build().send(email).await.map_err(|error| Failure {
        reason: trim(&error.to_string()),
        transient: !error.is_permanent(),
    })?;
    Ok(())
}

fn webhook_url<'a>(state: &AppState, value: Option<&'a Value>) -> Result<&'a str, String> {
    let url = value.and_then(Value::as_str).unwrap_or("");
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("notification URL must start with http:// or https://".to_owned());
    }
    if !state.config.allow_private_webhooks {
        crate::outbound::check_url(url).map_err(str::to_owned)?;
    }
    Ok(url)
}

fn webhook_client(state: &AppState) -> &reqwest::Client {
    if state.config.allow_private_webhooks {
        &state.http.direct
    } else {
        &state.http.webhook
    }
}

pub fn plain(payload: &NotificationPayload) -> (String, String) {
    let name = payload
        .monitor
        .as_ref()
        .map(|monitor| monitor.name.clone())
        .or_else(|| {
            payload
                .issue
                .as_ref()
                .map(|issue| format!("{} {}", issue.short_id, issue.title))
        })
        .unwrap_or_else(|| "Owlee".to_owned());
    let subject = match payload.event.as_str() {
        "monitor.down" => format!("[Owlee] {name} is down"),
        "monitor.recovered" => format!("[Owlee] {name} recovered"),
        "incident.updated" => format!("[Owlee] Incident update: {name}"),
        "issue.new" => format!("[Owlee] New error: {name}"),
        "issue.regressed" => format!("[Owlee] Regressed error: {name}"),
        "notification.test" => format!("[Owlee] Test notification for {name}"),
        _ => format!("[Owlee] {name}"),
    };
    let mut lines = vec![subject.clone(), String::new()];
    if let Some(monitor) = &payload.monitor {
        lines.push(format!("Monitor: {} ({})", monitor.name, monitor.url));
        lines.push(format!("State: {}", monitor.state.as_str()));
    }
    if let Some(check) = &payload.check {
        lines.push(format!(
            "Check: {} in {}ms",
            check.status.as_str(),
            check.latency_ms
        ));
        if let Some(error) = &check.error {
            lines.push(format!("Error: {error}"));
        }
    }
    if let Some(incident) = &payload.incident {
        lines.push(format!(
            "Incident: {} ({}, {})",
            incident.title,
            incident.status.as_str(),
            incident.impact.as_str()
        ));
    }
    if let Some(issue) = &payload.issue {
        lines.push(format!("Project: {}", issue.project));
        lines.push(format!("Issue: {} {}", issue.short_id, issue.title));
        if let Some(culprit) = issue
            .culprit
            .as_deref()
            .filter(|culprit| !culprit.is_empty())
        {
            lines.push(format!("Culprit: {culprit}"));
        }
        lines.push(issue.url.clone());
    }
    if let Some(message) = &payload.message {
        lines.push(String::new());
        lines.push(message.clone());
    }
    (subject, lines.join("\n"))
}

/// Without the URL: a Slack or Discord path is the credential.
fn request_error(error: reqwest::Error) -> Failure {
    Failure::transient(trim(&error_chain(&error.without_url())))
}

/// reqwest's message omits the cause, such as a refused address or a TLS failure.
pub(crate) fn error_chain(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

/// Alerts go to third parties, so they carry the monitor URL without its
/// credentials and query string.
pub(crate) fn payload_monitor(monitor: &Monitor, state: MonitorState) -> PayloadMonitor {
    PayloadMonitor {
        id: monitor.id.to_string(),
        name: monitor.name.clone(),
        url: crate::present::without_secrets(&monitor.url),
        state,
    }
}

fn trim(value: &str) -> String {
    value.chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_text_cannot_mention() {
        assert_eq!(
            slack_body("<!channel> & <@U1> a>b"),
            serde_json::json!({ "text": "&lt;!channel&gt; &amp; &lt;@U1&gt; a&gt;b" })
        );
    }

    #[test]
    fn discord_content_cannot_mention() {
        assert_eq!(
            discord_body("@everyone hi"),
            serde_json::json!({ "content": "@everyone hi", "allowed_mentions": { "parse": [] } })
        );
        assert_eq!(
            discord_body(&"x".repeat(2000))["content"]
                .as_str()
                .unwrap()
                .len(),
            1900
        );
    }
}
