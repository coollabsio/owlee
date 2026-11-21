use owlee_platform::{
    Config, Database, DatabaseConfig, EnvironmentMode, PasswordError, PasswordService,
};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use thiserror::Error;
use tokio::sync::Semaphore;

use crate::connectivity::Connectivity;
use crate::dns::{SystemResolver, TxtResolver};
use crate::limit::TokenBucket;
use crate::migrations;

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub config: Arc<Config>,
    pub passwords: PasswordService,
    pub http: HttpClients,
    pub scheduler_id: String,
    pub inflight: Inflight,
    pub connectivity: Connectivity,
    pub insights_salt: Arc<crate::insights::Salt>,
    pub insights_limits: Arc<crate::insights::Limits>,
    pub error_project_limit: Arc<TokenBucket>,
    pub error_ip_limit: Arc<TokenBucket>,
    pub analytics_site_limit: Arc<TokenBucket>,
    pub analytics_ip_limit: Arc<TokenBucket>,
    pub error_intake: Arc<crate::intake::IntakeStats>,
    pub analytics_intake: Arc<crate::intake::IntakeStats>,
    pub login_ip_limit: Arc<TokenBucket>,
    /// Failed passwords per account and client address.
    pub login_account_limit: Arc<TokenBucket>,
    /// Failed passwords per account from every address. Five times the rate
    /// of `login_account_limit`, so one address cannot empty it.
    pub login_account_wide_limit: Arc<TokenBucket>,
    pub public_page_limit: Arc<TokenBucket>,
    pub agent_result_limit: Arc<TokenBucket>,
    pub agent_checks_limit: Arc<TokenBucket>,
    /// One permit per CPU for argon2, so password requests cannot take every
    /// blocking thread and all the memory at once.
    pub hash_permits: Arc<Semaphore>,
    /// Account mails per address, so the auth routes cannot flood an inbox.
    pub mail_address_limit: Arc<TokenBucket>,
    /// Checks an editor starts by hand, per user: "Run check" and the unsaved test.
    pub check_limit: Arc<TokenBucket>,
    /// New and regressed issue alerts per project, so an ingest flood of
    /// distinct errors cannot flood the channels.
    pub issue_alert_limit: Arc<TokenBucket>,
    /// Tests set this to read account mails instead of sending them.
    pub outbox: Option<crate::notify::Outbox>,
    pub dns: Arc<dyn TxtResolver>,
    pub geo: Option<Arc<maxminddb::Reader<Vec<u8>>>>,
}

#[derive(Clone)]
pub struct HttpClients {
    pub follow: reqwest::Client,
    pub direct: reqwest::Client,
    /// No redirects, and host names resolve to public addresses only.
    pub webhook: reqwest::Client,
}

impl HttpClients {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            follow: client(reqwest::redirect::Policy::limited(5))?,
            direct: client(reqwest::redirect::Policy::none())?,
            webhook: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .dns_resolver(Arc::new(crate::outbound::PublicOnly))
                .user_agent(concat!("Owlee/", env!("CARGO_PKG_VERSION")))
                .build()?,
        })
    }
}

fn client(policy: reqwest::redirect::Policy) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .redirect(policy)
        .user_agent(concat!("Owlee/", env!("CARGO_PKG_VERSION")))
        .build()
}

#[derive(Clone, Default)]
pub struct Inflight {
    inner: Arc<Mutex<HashSet<String>>>,
}

pub struct CheckGuard {
    inner: Arc<Mutex<HashSet<String>>>,
    id: String,
}

impl Inflight {
    pub fn try_acquire(&self, id: &str) -> Option<CheckGuard> {
        let mut guard = lock(&self.inner);
        if !guard.insert(id.to_owned()) {
            return None;
        }
        Some(CheckGuard {
            inner: Arc::clone(&self.inner),
            id: id.to_owned(),
        })
    }
}

impl Drop for CheckGuard {
    fn drop(&mut self) {
        lock(&self.inner).remove(&self.id);
    }
}

fn lock(inner: &Mutex<HashSet<String>>) -> MutexGuard<'_, HashSet<String>> {
    inner.lock().unwrap_or_else(|error| error.into_inner())
}

pub async fn boot(config: Config) -> Result<AppState, BootError> {
    let database = Database::connect(&DatabaseConfig {
        path: config.database_path.clone(),
        postgres_url: config.database_url.clone(),
        max_connections: if config.database_url.is_some() { 16 } else { 8 },
    })
    .await?;
    let applied = migrations::runner().run(&database).await?;
    tracing::info!(applied, backend = config.backend_name(), "database ready");
    crate::errors::privatize_events(&database, &config.app_key)
        .await
        .map_err(|error| BootError::Config(error.to_string()))?;
    let passwords = PasswordService::new(config.argon_memory_kib)?;
    let insights = crate::insights::prepare(&config);
    crate::instance_settings::apply_quota_env(&database, &config.ingest, &config.app_key)
        .await
        .map_err(|error| BootError::Config(error.to_string()))?;
    let ingest = &config.ingest;
    Ok(AppState {
        db: database,
        http: HttpClients::new()?,
        scheduler_id: owlee_platform::Id::new().to_string(),
        inflight: Inflight::default(),
        error_intake: Arc::default(),
        analytics_intake: Arc::default(),
        connectivity: Connectivity::new(config.connectivity_targets.clone()),
        passwords,
        insights_salt: Arc::default(),
        insights_limits: insights.limits,
        error_project_limit: Arc::new(TokenBucket::per_minute(
            ingest.error_rate_project_per_min,
            ingest.error_rate_project_burst,
        )),
        error_ip_limit: Arc::new(TokenBucket::per_minute(
            ingest.error_rate_ip_per_min,
            ingest.error_rate_ip_burst,
        )),
        analytics_site_limit: Arc::new(TokenBucket::per_minute(
            ingest.analytics_rate_site_per_min,
            ingest.analytics_rate_site_burst,
        )),
        analytics_ip_limit: Arc::new(TokenBucket::per_minute(
            ingest.analytics_rate_ip_per_min,
            ingest.analytics_rate_ip_burst,
        )),
        login_ip_limit: Arc::new(TokenBucket::per_minute(
            ingest.login_rate_ip_per_min,
            ingest.login_rate_ip_burst,
        )),
        login_account_limit: Arc::new(TokenBucket::per_minute(
            ingest.login_rate_account_per_min,
            ingest.login_rate_account_burst,
        )),
        login_account_wide_limit: Arc::new(TokenBucket::per_minute(
            ingest.login_rate_account_per_min.saturating_mul(5),
            ingest.login_rate_account_burst.saturating_mul(20),
        )),
        public_page_limit: Arc::new(TokenBucket::per_minute(600, 200)),
        agent_result_limit: Arc::new(TokenBucket::per_minute(600, 200)),
        agent_checks_limit: Arc::new(TokenBucket::per_minute(6, 3)),
        hash_permits: Arc::new(Semaphore::new(
            std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        )),
        mail_address_limit: Arc::new(TokenBucket::per_hour(3, 3)),
        check_limit: Arc::new(TokenBucket::per_minute(30, 15)),
        issue_alert_limit: Arc::new(TokenBucket::per_hour(
            crate::errors::ISSUE_ALERTS_PER_HOUR,
            crate::errors::ISSUE_ALERTS_PER_HOUR,
        )),
        outbox: None,
        dns: Arc::new(SystemResolver::from_system()),
        geo: insights.geo,
        config: Arc::new(config),
    })
}

pub async fn hash_password(state: &AppState, password: String) -> Result<String, PasswordError> {
    let passwords = state.passwords.clone();
    argon(state, move || passwords.hash(&password)).await
}

pub async fn verify_password(
    state: &AppState,
    password: String,
    encoded: String,
) -> Result<bool, PasswordError> {
    let passwords = state.passwords.clone();
    argon(state, move || passwords.verify(&password, &encoded)).await
}

/// The blocking task owns the permit, so a request that is dropped while it
/// waits does not free the permit before the hash ends.
async fn argon<T: Send + 'static>(
    state: &AppState,
    work: impl FnOnce() -> Result<T, PasswordError> + Send + 'static,
) -> Result<T, PasswordError> {
    let permit = Arc::clone(&state.hash_permits)
        .acquire_owned()
        .await
        .map_err(|_| PasswordError::HashingFailed)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .unwrap_or(Err(PasswordError::HashingFailed))
}

#[must_use]
pub fn testing_config(path: PathBuf) -> Config {
    Config {
        environment: EnvironmentMode::Development,
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 0,
        public_origin: "http://127.0.0.1".to_owned(),
        database_path: path,
        database_url: None,
        // Ignored by the register route. Public sign-up follows instance settings.
        // Reused Postgres tests call `permit_signup` when setup already ran.
        open_signup: false,
        smtp: None,
        argon_memory_kib: 32,
        connectivity_targets: Vec::new(),
        allow_private_webhooks: true,
        proxy_hops: 0,
        trusted_proxies: owlee_platform::default_trusted_proxies(),
        geoip_path: None,
        ingest: owlee_platform::IngestConfig::default(),
        app_key: owlee_platform::AppKey::DEVELOPMENT,
    }
}

/// A signed-in test user inside one workspace.
pub struct TestSession {
    /// The `name=value` cookie pair.
    pub cookie: String,
    pub user_id: owlee_platform::Id,
    pub workspace_id: owlee_platform::Id,
    /// The workspace's first project.
    pub project_id: owlee_platform::Id,
}

/// Signs in a new user who owns a new workspace with a "Default" project.
/// Needs no mail and no setup, so it works on a reused database.
pub async fn editor_session(state: &AppState) -> Result<TestSession, sqlx::Error> {
    use owlee_platform::TimestampMillis;

    let (user_id, cookie) = test_user(state, "editor").await?;
    let mut tx = state.db.begin_immediate().await?;
    let created = crate::workspaces::create_workspace(
        &mut tx,
        user_id,
        "Test workspace",
        "Default",
        TimestampMillis::now(),
    )
    .await?;
    tx.commit().await?;
    Ok(TestSession {
        cookie,
        user_id,
        workspace_id: created.workspace_id,
        project_id: created.project_id,
    })
}

/// Signs in a new read-only member of an existing workspace, such as the one
/// `editor_session` made.
pub async fn member_session(
    state: &AppState,
    workspace_id: owlee_platform::Id,
) -> Result<TestSession, sqlx::Error> {
    use owlee_platform::TimestampMillis;

    let (user_id, cookie) = test_user(state, "member").await?;
    let mut tx = state.db.begin_immediate().await?;
    crate::workspaces::insert_membership(
        &mut tx,
        workspace_id,
        user_id,
        crate::workspaces::Role::Member,
        TimestampMillis::now(),
    )
    .await?;
    tx.commit().await?;
    let project_id = crate::workspaces::projects_in(&state.db, workspace_id)
        .await?
        .first()
        .map(|project| project.id)
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok(TestSession {
        cookie,
        user_id,
        workspace_id,
        project_id,
    })
}

async fn test_user(
    state: &AppState,
    label: &str,
) -> Result<(owlee_platform::Id, String), sqlx::Error> {
    use owlee_platform::{Id, TimestampMillis, generate_token, hash_token};

    let now = TimestampMillis::now();
    let id = Id::new();
    let email = format!("{label}-{id}@example.com");
    crate::repo::insert_user(&state.db, id, &email, &email, "Editor", "unusable", now).await?;
    let token = generate_token();
    let later = TimestampMillis::from_millis(now.as_millis() + 86_400_000);
    crate::repo::insert_session(
        &state.db,
        Id::new(),
        &hash_token(&token),
        id,
        now,
        later,
        later,
    )
    .await?;
    Ok((id, format!("owlee_session={token}")))
}

#[derive(Debug, Error)]
pub enum BootError {
    #[error(transparent)]
    Database(#[from] owlee_platform::DatabaseError),
    #[error(transparent)]
    Migration(#[from] owlee_platform::MigrationError),
    #[error(transparent)]
    Password(#[from] PasswordError),
    #[error("http client: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Config(String),
}
