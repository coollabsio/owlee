use std::ops::DerefMut;

use owlee_domain::{
    CheckStatus, IncidentImpact, IncidentStatus, MonitorState, NotificationEvent,
    NotificationPayload, PayloadCheck, PayloadIncident, Probe, Rules, apply_outcome, judge,
};
use owlee_platform::{Id, TimestampMillis};
use serde::{Deserialize, Serialize};
use sqlx::{Any, Row, Transaction};

use crate::checker::{self, Job};
use crate::error::AppError;
use crate::notify;
use crate::repo::{self, Incident, IncidentUpdate, Monitor, NewCheck};
use crate::state::AppState;

pub const LOCAL_REGION: &str = "local";

/// A judged check result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub status: CheckStatus,
    pub status_code: Option<i64>,
    pub latency_ms: i64,
    pub error: Option<String>,
}

impl Outcome {
    fn maintenance() -> Self {
        Self {
            status: CheckStatus::Maintenance,
            status_code: None,
            latency_ms: 0,
            error: Some("Skipped during a maintenance window.".to_owned()),
        }
    }
}

/// Longest check error that is stored. An agent chooses this text.
const MAX_ERROR_BYTES: usize = 1024;

pub fn auto_title(name: &str) -> String {
    format!("{name} is down")
}

pub const AUTO_DOWN_PREFIX: &str = "Owlee recorded ";
pub const AUTO_RECOVERED_SUFFIX: &str =
    " are succeeding again. This incident was resolved automatically.";

pub fn judge_probe(rules: &Rules, probe: &Probe) -> Outcome {
    let judgement = judge(rules, probe);
    Outcome {
        status: judgement.status,
        status_code: probe.status_code,
        latency_ms: probe.latency_ms,
        error: judgement.error,
    }
}

/// Runs a check from this server and records it.
pub async fn execute(state: &AppState, id: Id) -> Result<(), AppError> {
    let monitor = active_monitor(state, id).await?;
    let _guard = state
        .inflight
        .try_acquire(&id.to_string())
        .ok_or(AppError::Busy)?;
    let now = TimestampMillis::now();
    let outcome = if repo::in_maintenance(&state.db, id, now.as_millis()).await? {
        Outcome::maintenance()
    } else {
        trial(state, &monitor).await?
    };
    record(state, &monitor, outcome, LOCAL_REGION, now).await
}

/// Probes and judges a monitor from this server without recording anything.
pub async fn trial(state: &AppState, monitor: &Monitor) -> Result<Outcome, AppError> {
    let probe = checker::probe(&state.http, &Job::from(monitor)).await;
    if probe.transport_error.is_some() && !state.connectivity.online().await {
        return Err(AppError::Offline);
    }
    Ok(judge_probe(
        &Rules {
            job_type: monitor.job_type,
            expected_status: monitor.expected_status,
            degraded_after_ms: monitor.degraded_after_ms,
            assertions: monitor.assertions.clone(),
        },
        &probe,
    ))
}

/// Records an outcome that an agent judged inside its network.
pub async fn submit(
    state: &AppState,
    id: Id,
    outcome: Outcome,
    region: &str,
) -> Result<(), AppError> {
    let mut monitor = active_monitor(state, id).await?;
    let _guard = state
        .inflight
        .try_acquire(&id.to_string())
        .ok_or(AppError::Busy)?;
    let now = TimestampMillis::now();
    if monitor.agent_lost_at.is_some() {
        agent_returned(state, &monitor, region, now).await?;
        monitor = active_monitor(state, id).await?;
    }
    let outcome = if repo::in_maintenance(&state.db, id, now.as_millis()).await? {
        Outcome::maintenance()
    } else {
        outcome
    };
    record(state, &monitor, outcome, region, now).await
}

async fn active_monitor(state: &AppState, id: Id) -> Result<Monitor, AppError> {
    let monitor = repo::get_monitor(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !monitor.active {
        return Err(AppError::Conflict(
            "Resume the monitor before running a check.".into(),
        ));
    }
    Ok(monitor)
}

async fn record(
    state: &AppState,
    monitor: &Monitor,
    outcome: Outcome,
    region: &str,
    now: TimestampMillis,
) -> Result<(), AppError> {
    let id = monitor.id;
    let Outcome {
        status,
        status_code,
        latency_ms,
        mut error,
    } = outcome;
    if let Some(text) = &mut error {
        text.truncate(text.floor_char_boundary(MAX_ERROR_BYTES));
    }
    let transition = apply_outcome(
        monitor.state,
        monitor.consecutive_failures,
        monitor.failure_threshold,
        status,
    );
    let check = NewCheck {
        id: Id::new(),
        monitor_id: id,
        status,
        status_code,
        latency_ms,
        error: error.clone(),
        region: region.to_owned(),
        checked_at: now,
    };
    let next = now.as_millis() + monitor.interval_seconds.saturating_mul(1000);
    let mut tx = state.db.begin().await?;
    repo::insert_check(&mut tx, &check).await?;
    repo::bump_daily(&mut tx, id, now.unix_day(), status, latency_ms).await?;
    repo::save_outcome(
        &mut tx,
        id,
        transition.state,
        transition.consecutive_failures,
        now.as_millis(),
        next,
    )
    .await?;

    let mut notify_event = None;
    if transition.open_incident {
        let detail = error.unwrap_or_else(|| "The check failed.".to_owned());
        let message = format!(
            "{AUTO_DOWN_PREFIX}{} consecutive failed checks for {}. {detail}",
            transition.consecutive_failures, monitor.name
        );
        let title = auto_title(&monitor.name);
        if let Some(incident_id) = open_incident(&mut tx, monitor, &title, &message, now).await? {
            notify_event = Some((NotificationEvent::MonitorDown, incident_id, message));
        }
    } else if transition.resolve_incident {
        let message = format!("Checks for {}{AUTO_RECOVERED_SUFFIX}", monitor.name);
        if let Some(incident_id) = resolve_incident(&mut tx, id, &message, now).await? {
            notify_event = Some((NotificationEvent::MonitorRecovered, incident_id, message));
        }
    }
    tx.commit().await?;

    if transition.state != monitor.state {
        tracing::info!(
            monitor = %monitor.name,
            from = monitor.state.as_str(),
            to = transition.state.as_str(),
            "monitor state changed"
        );
    }
    if let Some((event, incident_id, message)) = notify_event {
        let announcement = Announcement {
            event,
            incident_id,
            title: auto_title(&monitor.name),
            message,
            state: transition.state,
            check: Some(PayloadCheck {
                status,
                status_code,
                latency_ms,
                error: check.error.clone(),
            }),
        };
        announce(state, monitor, announcement, now).await;
    }
    Ok(())
}

/// Opens the automatic incident of a monitor. Returns `None` when one is already open.
async fn open_incident(
    tx: &mut Transaction<'_, Any>,
    monitor: &Monitor,
    title: &str,
    message: &str,
    now: TimestampMillis,
) -> Result<Option<Id>, sqlx::Error> {
    let open = sqlx::query(
        "SELECT id FROM incidents WHERE monitor_id = $1 AND auto = 1 AND status != 'resolved' LIMIT 1",
    )
    .bind(monitor.id.to_string())
    .fetch_optional(tx.deref_mut())
    .await?;
    if open.is_some() {
        return Ok(None);
    }
    let incident_id = Id::new();
    let incident = Incident {
        id: incident_id,
        project_id: monitor.project_id,
        page_id: None,
        monitor_id: Some(monitor.id),
        title: title.to_owned(),
        status: IncidentStatus::Investigating,
        impact: IncidentImpact::Major,
        auto: true,
        started_at: now,
        resolved_at: None,
        created_at: now,
        updated_at: now,
    };
    let update = IncidentUpdate {
        id: Id::new(),
        incident_id,
        status: IncidentStatus::Investigating,
        message: message.to_owned(),
        created_at: now,
    };
    repo::insert_incident(tx, &incident, &update).await?;
    Ok(Some(incident_id))
}

/// Resolves the open automatic incident of a monitor. Returns `None` when none is open.
async fn resolve_incident(
    tx: &mut Transaction<'_, Any>,
    monitor_id: Id,
    message: &str,
    now: TimestampMillis,
) -> Result<Option<Id>, sqlx::Error> {
    let open = sqlx::query(
        "SELECT id FROM incidents WHERE monitor_id = $1 AND auto = 1 AND status != 'resolved' \
         ORDER BY started_at DESC LIMIT 1",
    )
    .bind(monitor_id.to_string())
    .fetch_optional(tx.deref_mut())
    .await?;
    let Some(incident_id) = open
        .and_then(|row| row.try_get::<String, _>("id").ok())
        .and_then(|value| value.parse::<Id>().ok())
    else {
        return Ok(None);
    };
    repo::apply_incident_status(
        tx,
        incident_id,
        IncidentStatus::Resolved,
        Some(now.as_millis()),
        now.as_millis(),
    )
    .await?;
    repo::insert_update(
        tx,
        &IncidentUpdate {
            id: Id::new(),
            incident_id,
            status: IncidentStatus::Resolved,
            message: message.to_owned(),
            created_at: now,
        },
    )
    .await?;
    Ok(Some(incident_id))
}

/// What the notification channels hear about an automatic incident.
struct Announcement {
    event: NotificationEvent,
    incident_id: Id,
    title: String,
    message: String,
    /// The monitor state after the change.
    state: MonitorState,
    /// `None` when no check caused the change.
    check: Option<PayloadCheck>,
}

async fn announce(
    state: &AppState,
    monitor: &Monitor,
    announcement: Announcement,
    now: TimestampMillis,
) {
    let Announcement {
        event,
        incident_id,
        title,
        message,
        state: monitor_state,
        check,
    } = announcement;
    let recovered = event == NotificationEvent::MonitorRecovered;
    let payload = NotificationPayload {
        event: event.as_str().to_owned(),
        sent_at: now.to_string(),
        monitor: Some(notify::payload_monitor(monitor, monitor_state)),
        check,
        incident: Some(PayloadIncident {
            id: incident_id.to_string(),
            title,
            status: if recovered {
                IncidentStatus::Resolved
            } else {
                IncidentStatus::Investigating
            },
            impact: IncidentImpact::Major,
        }),
        message: Some(message),
    };
    notify::fanout(
        state,
        &payload,
        monitor.project_id,
        Some(monitor.id),
        Some(incident_id),
    )
    .await;
}

/// How long an agent may be silent before its monitors stop counting as checked. An agent
/// sends a heartbeat every 15 s, so this is eight missed heartbeats in a row. A restart of
/// the agent or of its host, or a short network fault, ends well inside this time.
pub const AGENT_GRACE_MS: i64 = 120_000;

pub const AGENT_LOST_PREFIX: &str = "Owlee has not heard from agent ";
pub const AGENT_BACK_PREFIX: &str = "Owlee hears from agent ";

pub fn agent_lost_title(monitor: &str, agent: &str) -> String {
    format!("{monitor} is not checked: agent {agent} is offline")
}

/// Makes the monitors of every agent match whether the agent still reports.
///
/// A monitor of an agent that is silent for longer than `AGENT_GRACE_MS` goes down with an
/// incident that names the agent. It comes back when the agent is heard again. Every step
/// is a conditional update, so more ticks or more processes change nothing.
///
/// `watching_since` is when this process started. Agents cannot report while no server
/// runs, so that time does not count as silence.
pub async fn reconcile_agents(
    state: &AppState,
    now: TimestampMillis,
    watching_since: TimestampMillis,
) -> Result<(), AppError> {
    let cutoff = now.as_millis() - AGENT_GRACE_MS;
    for back in repo::monitors_of_returned_agents(&state.db, cutoff).await? {
        agent_returned(state, &back.monitor, &back.agent_name, now).await?;
    }
    if watching_since.as_millis() > cutoff {
        return Ok(());
    }
    for lost in repo::monitors_of_silent_agents(&state.db, cutoff, now.as_millis()).await? {
        agent_lost(state, &lost.monitor, &lost.agent_name, cutoff, now).await?;
    }
    Ok(())
}

async fn agent_lost(
    state: &AppState,
    monitor: &Monitor,
    agent: &str,
    cutoff: i64,
    now: TimestampMillis,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    if !repo::mark_agent_lost(&mut tx, monitor.id, cutoff, now.as_millis()).await? {
        return Ok(());
    }
    let title = agent_lost_title(&monitor.name, agent);
    let message = format!(
        "{AGENT_LOST_PREFIX}{agent} for {} minutes. Nothing checks {} until the agent \
         reports again, so its real state is unknown.",
        AGENT_GRACE_MS / 60_000,
        monitor.name
    );
    let incident_id = open_incident(&mut tx, monitor, &title, &message, now).await?;
    tx.commit().await?;
    tracing::warn!(monitor = %monitor.name, agent, "agent is silent, monitor marked down");
    if let Some(incident_id) = incident_id {
        let announcement = Announcement {
            event: NotificationEvent::MonitorDown,
            incident_id,
            title,
            message,
            state: MonitorState::Down,
            check: None,
        };
        announce(state, monitor, announcement, now).await;
    }
    Ok(())
}

async fn agent_returned(
    state: &AppState,
    monitor: &Monitor,
    agent: &str,
    now: TimestampMillis,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    if !repo::clear_agent_lost(&mut tx, monitor.id, now.as_millis()).await? {
        return Ok(());
    }
    let message = format!(
        "{AGENT_BACK_PREFIX}{agent} again. Checks for {} continue. \
         This incident was resolved automatically.",
        monitor.name
    );
    let incident_id = resolve_incident(&mut tx, monitor.id, &message, now).await?;
    tx.commit().await?;
    tracing::info!(monitor = %monitor.name, agent, "agent is back");
    if let Some(incident_id) = incident_id {
        let announcement = Announcement {
            event: NotificationEvent::MonitorRecovered,
            incident_id,
            title: agent_lost_title(&monitor.name, agent),
            message,
            state: MonitorState::Pending,
            check: None,
        };
        announce(state, monitor, announcement, now).await;
    }
    Ok(())
}
