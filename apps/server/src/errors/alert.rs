//! Alerts for new and regressed issues. They leave the ingest request at
//! once, and each project gets a capped number per hour.

use owlee_domain::{NotificationEvent, NotificationPayload, PayloadIssue};
use owlee_platform::TimestampMillis;

use super::store::{App, IssueAlert, IssueChange};
use crate::notify;
use crate::repo;
use crate::state::AppState;

pub const ISSUE_ALERTS_PER_HOUR: u32 = 10;

pub(super) fn send(state: &AppState, app: &App, alert: IssueAlert) {
    let project_id = app.project_id;
    if !state
        .issue_alert_limit
        .check(&project_id.to_string())
        .allowed
    {
        tracing::warn!(
            project = %project_id,
            issue = alert.short_id,
            "issue alert dropped: the project reached {ISSUE_ALERTS_PER_HOUR} alerts this hour"
        );
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = fanout(&state, project_id, alert).await {
            tracing::error!(%error, "sending an issue alert");
        }
    });
}

async fn fanout(
    state: &AppState,
    project_id: owlee_platform::Id,
    alert: IssueAlert,
) -> Result<(), sqlx::Error> {
    let channels = repo::channels_for_issues(&state.db, &state.config.app_key, project_id).await?;
    if channels.is_empty() {
        return Ok(());
    }
    let Some((project, workspace_id)) = repo::project_label(&state.db, project_id).await? else {
        return Ok(());
    };
    let event = match alert.change {
        IssueChange::New => NotificationEvent::IssueNew,
        IssueChange::Regressed => NotificationEvent::IssueRegressed,
    };
    let origin = state.config.public_origin.trim_end_matches('/');
    let payload = NotificationPayload {
        event: event.as_str().to_owned(),
        sent_at: TimestampMillis::now().to_string(),
        monitor: None,
        check: None,
        incident: None,
        issue: Some(PayloadIssue {
            url: format!(
                "{origin}/errors/{}?workspace={workspace_id}",
                alert.issue_id
            ),
            id: alert.issue_id,
            short_id: alert.short_id,
            title: alert.title,
            culprit: alert.culprit,
            project,
        }),
        message: None,
    };
    for channel in channels {
        notify::deliver(state, &channel, &payload, None, None).await;
    }
    Ok(())
}
