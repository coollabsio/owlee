//! Session replay intake. The envelope route calls [`intake`] once for an
//! envelope with replay items. A refusal drops only the replay items: it adds
//! the `replay` category to `X-Sentry-Rate-Limits` and never changes the
//! status that the `event` item gets.

use owlee_errors::{Envelope, ReplaySegment, normalize_event, parse_replay};

use super::filter::InboundFilter;
use super::replay_store::{self, Caps, Stored};
use super::store::App;
use crate::intake::DropReason;
use crate::state::AppState;

/// One `X-Sentry-Rate-Limits` entry for the `replay` category.
#[derive(Debug, Clone, Copy)]
pub struct ReplayLimit {
    pub retry_after_secs: u64,
    pub scope: &'static str,
    pub reason: &'static str,
}

impl ReplayLimit {
    pub fn header_entry(self) -> String {
        let retry = self.retry_after_secs.max(1);
        if self.reason.is_empty() {
            format!("{retry}:replay:{}", self.scope)
        } else {
            format!("{retry}:replay:{}:{}", self.scope, self.reason)
        }
    }
}

/// The replay items of one envelope. One segment is one `replay_event` and one
/// `replay_recording`.
#[derive(Default)]
pub struct ReplayItems<'a> {
    event: Option<&'a [u8]>,
    recording: Option<&'a [u8]>,
    /// A second pair: one envelope carries one segment.
    extra: usize,
    /// `replay_video`, sent by the mobile SDKs.
    video: bool,
}

impl<'a> ReplayItems<'a> {
    pub fn of(envelope: &'a Envelope) -> Self {
        let mut items = Self::default();
        for item in &envelope.items {
            let slot = match item.item_type.as_str() {
                "replay_event" => &mut items.event,
                "replay_recording" => &mut items.recording,
                "replay_video" => {
                    items.video = true;
                    continue;
                }
                _ => continue,
            };
            if slot.is_some() {
                items.extra += 1;
            } else {
                *slot = Some(item.payload.as_slice());
            }
        }
        items
    }

    pub fn is_empty(&self) -> bool {
        self.event.is_none() && self.recording.is_none() && !self.video
    }
}

/// Runs steps 5 to 10 of the pipeline for the replay items. `ip_key` is the
/// client's rate-limit key. `has_event` is whether the envelope also carries
/// an `event`: then the error IP token already paid for the request.
pub async fn intake(
    state: &AppState,
    app: &App,
    items: ReplayItems<'_>,
    has_event: bool,
    ip_key: &str,
) -> Option<ReplayLimit> {
    for _ in 0..items.extra {
        state.error_intake.dropped(&app.id, DropReason::Malformed);
    }
    if !has_event {
        state.error_ip_limit.refund(&format!("error-ip:{ip_key}"));
        let ip = state.replay_ip_limit.check(&format!("replay-ip:{ip_key}"));
        if !ip.allowed {
            state
                .error_intake
                .dropped(&app.id, DropReason::ClientRateLimit);
            return Some(ReplayLimit {
                retry_after_secs: ip.retry_after_secs,
                scope: "ip",
                reason: "",
            });
        }
    }
    let bucket = state
        .replay_app_limit
        .check(&format!("replay-app:{}", app.id));
    if !bucket.allowed {
        state
            .error_intake
            .dropped(&app.id, DropReason::SourceRateLimit);
        return Some(ReplayLimit {
            retry_after_secs: bucket.retry_after_secs,
            scope: "key",
            reason: "",
        });
    }
    if !app.replays_enabled {
        state
            .error_intake
            .dropped(&app.id, DropReason::ReplaysDisabled);
        return Some(ReplayLimit {
            retry_after_secs: 86_400,
            scope: "project",
            reason: "replays_disabled",
        });
    }
    if items.video {
        state.error_intake.dropped(&app.id, DropReason::Unsupported);
        return None;
    }
    let (Some(event), Some(recording)) = (items.event, items.recording) else {
        state.error_intake.dropped(&app.id, DropReason::Malformed);
        tracing::warn!("dropped a replay segment without its event or recording");
        return None;
    };
    let segment = match parse(state, event, recording).await {
        Ok(segment) => segment,
        Err(error) => {
            state.error_intake.dropped(&app.id, DropReason::Malformed);
            tracing::warn!(error = %error, "dropped malformed replay segment");
            return None;
        }
    };
    if filtered(app, &segment) {
        state.error_intake.dropped(&app.id, DropReason::Filtered);
        return None;
    }
    let caps = Caps {
        hourly_limit: app.replay_hourly_limit,
        daily_bytes: app.replay_daily_bytes,
    };
    let stored = replay_store::store_segment(
        &state.db,
        &state.config.app_key,
        &app.id,
        &caps,
        &segment,
        chrono::Utc::now().timestamp_millis(),
    )
    .await;
    match stored {
        Ok(Stored::Added | Stored::Duplicate) => None,
        Ok(Stored::HourlyLimit) => {
            state
                .error_intake
                .dropped(&app.id, DropReason::ReplayHourlyLimit);
            Some(ReplayLimit {
                retry_after_secs: crate::quota::secs_until_next_hour(),
                scope: "project",
                reason: "replay_hourly_limit",
            })
        }
        Ok(Stored::DailyBytes) => {
            state.error_intake.dropped(&app.id, DropReason::ReplayQuota);
            Some(ReplayLimit {
                retry_after_secs: crate::quota::secs_until_utc_midnight(),
                scope: "organization",
                reason: "quota_exceeded",
            })
        }
        Err(error) => {
            state
                .error_intake
                .dropped(&app.id, DropReason::StorageFailed);
            tracing::error!(error = %error, "failed to store replay segment");
            None
        }
    }
}

/// The recording scan reads up to `OWLEE_REPLAY_MAX_SEGMENT_BYTES`, so it runs off the runtime.
async fn parse(state: &AppState, event: &[u8], recording: &[u8]) -> Result<ReplaySegment, String> {
    let max = state.config.ingest.replay_max_segment_bytes;
    let (event, recording) = (event.to_vec(), recording.to_vec());
    tokio::task::spawn_blocking(move || parse_replay(&event, &recording, max))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// The filters read `request.url`, `request.headers`, and `user.ip_address` of
/// the `replay_event`. A replay has no stack, so the extension filter does not apply.
fn filtered(app: &App, segment: &ReplaySegment) -> bool {
    let event = normalize_event(segment.event.clone());
    app.inbound_filters
        .iter()
        .filter(|filter| **filter != InboundFilter::BrowserExtensions)
        .any(|filter| filter.matches(&event))
}
