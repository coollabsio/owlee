//! In-memory token bucket for a single Owlee process.
//!
//! Public ingest is limited per project or site key and per client address.
//! A second node does not share these buckets.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

const MAX_KEYS: usize = 10_000;

#[derive(Debug, Clone, Copy)]
pub struct RateDecision {
    pub allowed: bool,
    pub retry_after_secs: u64,
}

pub struct TokenBucket {
    per_sec: f64,
    burst: f64,
    buckets: Mutex<HashMap<String, (Instant, f64)>>,
}

impl TokenBucket {
    pub fn per_minute(per_minute: u32, burst: u32) -> Self {
        Self::new(f64::from(per_minute.max(1)) / 60.0, burst)
    }

    pub fn per_hour(per_hour: u32, burst: u32) -> Self {
        Self::new(f64::from(per_hour.max(1)) / 3600.0, burst)
    }

    fn new(per_sec: f64, burst: u32) -> Self {
        Self {
            per_sec,
            burst: f64::from(burst.max(1)),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Same answer as [`check`](Self::check), but does not use a token.
    /// Login uses this so only a failed attempt costs the account a token.
    pub fn peek(&self, key: &str) -> RateDecision {
        let now = Instant::now();
        let buckets = self
            .buckets
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let level = buckets
            .get(key)
            .map_or(self.burst, |bucket| self.level(*bucket, now));
        self.decide(level)
    }

    /// Drops the bucket, for example after a successful login.
    pub fn forget(&self, key: &str) {
        self.buckets
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(key);
    }

    fn decide(&self, level: f64) -> RateDecision {
        if level >= 1.0 {
            RateDecision {
                allowed: true,
                retry_after_secs: 0,
            }
        } else {
            RateDecision {
                allowed: false,
                retry_after_secs: ((1.0 - level) / self.per_sec).ceil().max(1.0) as u64,
            }
        }
    }

    pub fn check(&self, key: &str) -> RateDecision {
        let now = Instant::now();
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if buckets.len() >= MAX_KEYS && !buckets.contains_key(key) {
            self.evict(&mut buckets, now);
        }
        let entry = buckets.entry(key.to_owned()).or_insert((now, self.burst));
        let level = self.level(*entry, now);
        *entry = (now, level);
        let decision = self.decide(level);
        if decision.allowed {
            entry.1 -= 1.0;
        }
        decision
    }
}

impl TokenBucket {
    fn level(&self, (updated, tokens): (Instant, f64), now: Instant) -> f64 {
        (tokens + now.duration_since(updated).as_secs_f64() * self.per_sec).min(self.burst)
    }

    /// Frees room without resetting a client that is at its limit, so many new
    /// keys cannot clear the buckets of the keys being limited.
    fn evict(&self, buckets: &mut HashMap<String, (Instant, f64)>, now: Instant) {
        // A bucket that has refilled is the same as a new one.
        buckets.retain(|_, bucket| self.level(*bucket, now) < self.burst);
        if buckets.len() < MAX_KEYS {
            return;
        }
        // Still full: drop the half used least recently. A client that keeps
        // sending while it is limited stays recent, so it stays limited.
        let mut ages: Vec<(Instant, String)> = buckets
            .iter()
            .map(|(key, (updated, _))| (*updated, key.clone()))
            .collect();
        ages.sort_by_key(|(updated, _)| *updated);
        for (_, key) in ages.into_iter().take(MAX_KEYS / 2) {
            buckets.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_retry_after() {
        let bucket = TokenBucket::per_minute(60, 2);
        assert!(bucket.check("project").allowed);
        assert!(bucket.check("project").allowed);
        let denied = bucket.check("project");
        assert!(!denied.allowed);
        assert!(denied.retry_after_secs >= 1);
    }

    #[test]
    fn peek_uses_no_token_and_forget_resets() {
        let bucket = TokenBucket::per_minute(1, 1);
        assert!(bucket.peek("account").allowed);
        assert!(bucket.peek("account").allowed);
        assert!(bucket.check("account").allowed);
        assert!(!bucket.peek("account").allowed);
        bucket.forget("account");
        assert!(bucket.peek("account").allowed);
    }

    #[test]
    fn new_keys_do_not_reset_a_limited_key() {
        let bucket = TokenBucket::per_minute(1, 1);
        assert!(bucket.check("attacker").allowed);
        assert!(!bucket.check("attacker").allowed);
        for index in 0..MAX_KEYS * 2 {
            bucket.check(&format!("other-{index}"));
            if index % 100 == 0 {
                assert!(
                    !bucket.check("attacker").allowed,
                    "reset after {index} keys"
                );
            }
        }
        assert!(!bucket.check("attacker").allowed);
    }
}
