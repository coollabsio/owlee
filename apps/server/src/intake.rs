//! Stored and dropped counts per error project, analytics site, and telemetry source, so a
//! sender that is refused is visible in the UI and not only in the log.
//! The counts live in memory and start at zero when the server starts.

use std::collections::HashMap;
use std::sync::Mutex;

use owlee_platform::TimestampMillis;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    ClientRateLimit,
    SourceRateLimit,
    DailyQuota,
    Malformed,
    OriginNotAllowed,
    UnverifiedDomain,
    Filtered,
    SpikeProtection,
    StorageFailed,
    ReplaysDisabled,
    ReplayHourlyLimit,
    ReplayQuota,
    /// An item type Owlee does not store, such as `replay_video`.
    Unsupported,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct IntakeCounts {
    pub stored: u64,
    pub dropped: u64,
    pub last_drop: Option<LastDrop>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LastDrop {
    pub at: TimestampMillis,
    pub reason: DropReason,
}

/// Keyed by error project id or analytics site id. Only ids that exist are
/// recorded, so the map grows with the number of projects and sites.
#[derive(Default)]
pub struct IntakeStats {
    counts: Mutex<HashMap<String, IntakeCounts>>,
}

impl IntakeStats {
    pub fn stored(&self, id: &str) {
        self.stored_many(id, 1);
    }

    pub fn stored_many(&self, id: &str, count: u64) {
        self.update(id, |counts| counts.stored += count);
    }

    pub fn dropped(&self, id: &str, reason: DropReason) {
        self.dropped_many(id, 1, reason);
    }

    pub fn dropped_many(&self, id: &str, count: u64, reason: DropReason) {
        self.update(id, |counts| {
            counts.dropped += count;
            counts.last_drop = Some(LastDrop {
                at: TimestampMillis::now(),
                reason,
            });
        });
    }

    /// For a refusal made before the sender's key is checked, when the id is
    /// only a claim from the URL. Counts it against an id that is already
    /// tracked, so made-up ids cannot grow the map.
    pub fn dropped_if_known(&self, id: &str, reason: DropReason) {
        if let Some(counts) = self.lock().get_mut(id) {
            counts.dropped += 1;
            counts.last_drop = Some(LastDrop {
                at: TimestampMillis::now(),
                reason,
            });
        }
    }

    pub fn get(&self, id: &str) -> IntakeCounts {
        self.lock().get(id).cloned().unwrap_or_default()
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut IntakeCounts)) {
        change(self.lock().entry(id.to_owned()).or_default());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, IntakeCounts>> {
        self.counts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}
