//! Cookieless page analytics and the short-link redirector.
//!
//! The tracker wire format (`/data.js`, `POST /api/receive`) matches
//! ShadowArcanist's Beacon so an existing snippet keeps working. Dashboard
//! routes use Owlee's session cookie.

mod geo;
mod limit;
mod record;
mod routes;
mod seed;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use owlee_platform::{Config, Database, generate_token};
use maxminddb::Reader;
use sqlx::Row;

pub use limit::RateLimiter;
pub use routes::{api_routes, fallback, ingest_routes, public_routes};
pub use seed::seed_demo;

pub struct Runtime {
    pub public_limit: Arc<RateLimiter>,
    pub geo: Option<Arc<Reader<Vec<u8>>>>,
}

pub fn prepare(config: &Config) -> Runtime {
    Runtime {
        public_limit: Arc::new(RateLimiter::new(60, Duration::from_secs(60))),
        geo: config
            .geoip_path
            .as_ref()
            .and_then(|path| geo::open_reader(path).map(Arc::new)),
    }
}

/// The visitor-hash salt for one UTC month. Older salts are deleted once a
/// newer month is in use, so last month's visitor ids can no longer be
/// recomputed from an address and user agent.
#[derive(Default)]
pub struct Salt {
    cached: Mutex<Option<(String, String)>>,
}

impl Salt {
    pub async fn for_time(&self, db: &Database, unix: i64) -> Result<String, sqlx::Error> {
        let month = chrono::DateTime::from_timestamp(unix, 0)
            .unwrap_or_default()
            .format("salt:%Y-%m")
            .to_string();
        if let Some((cached_month, salt)) = self.lock().as_ref()
            && *cached_month == month
        {
            return Ok(salt.clone());
        }
        sqlx::query(
            "INSERT INTO insight_settings (key, value) VALUES ($1, $2) ON CONFLICT (key) DO NOTHING",
        )
        .bind(&month)
        .bind(generate_token())
        .execute(db.pool())
        .await?;
        let salt: String = sqlx::query("SELECT value FROM insight_settings WHERE key = $1")
            .bind(&month)
            .fetch_one(db.pool())
            .await?
            .try_get("value")?;
        // Only older months, so an instance with a slow clock cannot delete the current salt.
        sqlx::query(
            "DELETE FROM insight_settings WHERE key = 'secret' OR (key LIKE 'salt:%' AND key < $1)",
        )
        .bind(&month)
        .execute(db.pool())
        .await?;
        *self.lock() = Some((month, salt.clone()));
        Ok(salt)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(String, String)>> {
        self.cached.lock().unwrap_or_else(|error| error.into_inner())
    }
}
