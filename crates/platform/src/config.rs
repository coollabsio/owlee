use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;

use ipnet::IpNet;
use thiserror::Error;

const ENV: &str = "OWLEE_ENV";
const BIND: &str = "OWLEE_BIND";
const PORT: &str = "OWLEE_PORT";
const ORIGIN: &str = "OWLEE_PUBLIC_ORIGIN";
const DATABASE_PATH: &str = "OWLEE_DATABASE_PATH";
const DATABASE_URL: &str = "OWLEE_DATABASE_URL";
const OPEN_SIGNUP: &str = "OWLEE_OPEN_SIGNUP";
const SMTP_HOST: &str = "OWLEE_SMTP_HOST";
const SMTP_PORT: &str = "OWLEE_SMTP_PORT";
const SMTP_USERNAME: &str = "OWLEE_SMTP_USERNAME";
const SMTP_PASSWORD: &str = "OWLEE_SMTP_PASSWORD";
const SMTP_FROM: &str = "OWLEE_SMTP_FROM";
const ARGON_MEMORY: &str = "OWLEE_ARGON_MEMORY_KIB";
const CONNECTIVITY_TARGETS: &str = "OWLEE_CONNECTIVITY_TARGETS";
const ALLOW_PRIVATE_WEBHOOKS: &str = "OWLEE_ALLOW_PRIVATE_WEBHOOKS";
const PROXY_HOPS: &str = "OWLEE_PROXY_HOPS";
const TRUSTED_PROXIES: &str = "OWLEE_TRUSTED_PROXIES";
const GEOIP_PATH: &str = "OWLEE_GEOIP_PATH";
const ERROR_MAX_BODY: &str = "OWLEE_ERROR_MAX_BODY_BYTES";
const ANALYTICS_MAX_BODY: &str = "OWLEE_ANALYTICS_MAX_BODY_BYTES";
const ERROR_RATE_PROJECT: &str = "OWLEE_ERROR_RATE_PROJECT_PER_MIN";
const ERROR_BURST_PROJECT: &str = "OWLEE_ERROR_RATE_PROJECT_BURST";
const ERROR_RATE_IP: &str = "OWLEE_ERROR_RATE_IP_PER_MIN";
const ERROR_BURST_IP: &str = "OWLEE_ERROR_RATE_IP_BURST";
const ANALYTICS_RATE_SITE: &str = "OWLEE_ANALYTICS_RATE_SITE_PER_MIN";
const ANALYTICS_BURST_SITE: &str = "OWLEE_ANALYTICS_RATE_SITE_BURST";
const ANALYTICS_RATE_IP: &str = "OWLEE_ANALYTICS_RATE_IP_PER_MIN";
const ANALYTICS_BURST_IP: &str = "OWLEE_ANALYTICS_RATE_IP_BURST";
const CHECK_RETENTION: &str = "OWLEE_CHECK_RETENTION_DAYS";
const ERROR_RETENTION: &str = "OWLEE_ERROR_RETENTION_DAYS";
const ANALYTICS_RETENTION: &str = "OWLEE_ANALYTICS_RETENTION_DAYS";
const REPLAY_MAX_BODY: &str = "OWLEE_REPLAY_MAX_BODY_BYTES";
const REPLAY_MAX_SEGMENT: &str = "OWLEE_REPLAY_MAX_SEGMENT_BYTES";
const REPLAY_RATE_APP: &str = "OWLEE_REPLAY_RATE_APP_PER_MIN";
const REPLAY_RATE_IP: &str = "OWLEE_REPLAY_RATE_IP_PER_MIN";
const REPLAY_RETENTION: &str = "OWLEE_REPLAY_RETENTION_DAYS";
const ERROR_KEEP_FIRST: &str = "OWLEE_ERROR_KEEP_FIRST";
const ERROR_SAMPLE_WINDOW: &str = "OWLEE_ERROR_SAMPLE_WINDOW_SECS";
const ALLOW_LOCALHOST: &str = "OWLEE_INGEST_ALLOW_LOCALHOST";
const DOMAIN_RECHECK: &str = "OWLEE_DOMAIN_RECHECK_SECS";
const ERROR_DAILY_QUOTA: &str = "OWLEE_ERROR_DAILY_QUOTA";
const ANALYTICS_DAILY_QUOTA: &str = "OWLEE_ANALYTICS_DAILY_QUOTA";
const LOGIN_RATE_IP: &str = "OWLEE_LOGIN_RATE_IP_PER_MIN";
const LOGIN_BURST_IP: &str = "OWLEE_LOGIN_RATE_IP_BURST";
const LOGIN_RATE_ACCOUNT: &str = "OWLEE_LOGIN_RATE_ACCOUNT_PER_MIN";
const LOGIN_BURST_ACCOUNT: &str = "OWLEE_LOGIN_RATE_ACCOUNT_BURST";
const APP_KEY: &str = "OWLEE_APP_KEY";
const ANALYTICS_GEO: &str = "OWLEE_ANALYTICS_GEO";

/// Limits for the public error and analytics ingest endpoints.
#[derive(Clone, Debug)]
pub struct IngestConfig {
    pub error_max_body_bytes: usize,
    pub analytics_max_body_bytes: usize,
    pub error_rate_project_per_min: u32,
    pub error_rate_project_burst: u32,
    pub error_rate_ip_per_min: u32,
    pub error_rate_ip_burst: u32,
    pub analytics_rate_site_per_min: u32,
    pub analytics_rate_site_burst: u32,
    pub analytics_rate_ip_per_min: u32,
    pub analytics_rate_ip_burst: u32,
    /// Days of monitor check results to keep (`OWLEE_CHECK_RETENTION_DAYS`).
    pub check_retention_days: i64,
    pub error_retention_days: i64,
    pub analytics_retention_days: i64,
    /// The read limit of an envelope. The `event` item keeps `error_max_body_bytes`.
    pub replay_max_body_bytes: usize,
    /// The most a replay recording may inflate to.
    pub replay_max_segment_bytes: usize,
    /// Token buckets of envelopes with replay items and no event. The burst is a quarter of the rate.
    pub replay_rate_app_per_min: u32,
    pub replay_rate_ip_per_min: u32,
    pub replay_retention_days: i64,
    pub error_keep_first: i64,
    pub error_sample_window_secs: i64,
    /// Accept `localhost` / `127.0.0.1` analytics origins in addition to the site domain.
    pub allow_localhost: bool,
    pub domain_recheck_secs: i64,
    /// `None` leaves the Settings value. Set until the root user saves quotas.
    pub error_daily_quota: Option<i64>,
    pub analytics_daily_quota: Option<i64>,
    pub login_rate_ip_per_min: u32,
    pub login_rate_ip_burst: u32,
    /// Failed passwords per account and client address. The limit for the
    /// account across all addresses is 5 times this rate and 20 times the burst.
    pub login_rate_account_per_min: u32,
    pub login_rate_account_burst: u32,
    pub analytics_geo: GeoPrecision,
}

/// The finest location saved for an analytics visit (`OWLEE_ANALYTICS_GEO`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GeoPrecision {
    #[default]
    City,
    Region,
    Country,
    Off,
}

impl FromStr for GeoPrecision {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "city" => Ok(Self::City),
            "region" => Ok(Self::Region),
            "country" => Ok(Self::Country),
            "off" => Ok(Self::Off),
            _ => Err(()),
        }
    }
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            error_max_body_bytes: 1024 * 1024,
            analytics_max_body_bytes: 8 * 1024,
            error_rate_project_per_min: 120,
            error_rate_project_burst: 30,
            error_rate_ip_per_min: 60,
            error_rate_ip_burst: 20,
            analytics_rate_site_per_min: 240,
            analytics_rate_site_burst: 60,
            analytics_rate_ip_per_min: 60,
            analytics_rate_ip_burst: 30,
            check_retention_days: 14,
            error_retention_days: 30,
            analytics_retention_days: 90,
            replay_max_body_bytes: 10 * 1024 * 1024,
            replay_max_segment_bytes: 64 * 1024 * 1024,
            replay_rate_app_per_min: 1200,
            replay_rate_ip_per_min: 120,
            replay_retention_days: 14,
            error_keep_first: 5,
            error_sample_window_secs: 3600,
            allow_localhost: true,
            domain_recheck_secs: 3600,
            error_daily_quota: None,
            analytics_daily_quota: None,
            login_rate_ip_per_min: 30,
            login_rate_ip_burst: 10,
            login_rate_account_per_min: 1,
            login_rate_account_burst: 5,
            analytics_geo: GeoPrecision::City,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub environment: EnvironmentMode,
    pub bind: IpAddr,
    pub port: u16,
    pub public_origin: String,
    pub database_path: PathBuf,
    pub database_url: Option<String>,
    pub open_signup: bool,
    pub smtp: Option<SmtpConfig>,
    pub argon_memory_kib: u32,
    /// `host:port` targets that prove the Owlee host itself has network.
    /// Empty disables the check.
    pub connectivity_targets: Vec<String>,
    /// Lets notification webhooks reach loopback, private, and link-local addresses.
    pub allow_private_webhooks: bool,
    /// Trusted reverse proxies in front of Owlee. `0` means the socket address is the client.
    pub proxy_hops: usize,
    /// Forwarding headers are used only when the TCP peer is in one of these
    /// networks. A client that reaches the port directly cannot fake its address.
    pub trusted_proxies: Vec<IpNet>,
    /// Optional MaxMind or DB-IP city database (`.mmdb`) for analytics geolocation.
    pub geoip_path: Option<PathBuf>,
    pub ingest: IngestConfig,
    /// Encrypts secrets saved in the database (`OWLEE_APP_KEY`).
    pub app_key: AppKey,
}

impl Config {
    #[must_use]
    pub fn development() -> Self {
        Self {
            environment: EnvironmentMode::Development,
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 43123,
            public_origin: "http://127.0.0.1:43123".to_owned(),
            database_path: PathBuf::from("data/owlee.sqlite"),
            database_url: None,
            open_signup: false,
            smtp: None,
            argon_memory_kib: 19456,
            connectivity_targets: ["1.1.1.1:443", "8.8.8.8:443", "9.9.9.9:443"]
                .map(str::to_owned)
                .to_vec(),
            allow_private_webhooks: true,
            proxy_hops: 0,
            trusted_proxies: default_trusted_proxies(),
            geoip_path: None,
            ingest: IngestConfig::default(),
            app_key: AppKey::DEVELOPMENT,
        }
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        let environment = match optional(ENV).as_deref() {
            // Unset is production, so a bare binary never runs with the public app key.
            None | Some("production") => EnvironmentMode::Production,
            Some("development") => EnvironmentMode::Development,
            Some(other) => {
                return Err(ConfigError::Invalid {
                    name: ENV,
                    value: other.to_owned(),
                });
            }
        };
        let mut config = Self::development();
        config.environment = environment;
        if environment == EnvironmentMode::Production {
            config.open_signup = false;
            config.bind = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
            config.ingest.allow_localhost = false;
            config.allow_private_webhooks = false;
        }
        if let Some(bind) = optional(BIND) {
            config.bind = bind.parse().map_err(|_| ConfigError::Invalid {
                name: BIND,
                value: bind,
            })?;
        }
        if let Some(port) = optional(PORT) {
            config.port = port.parse().map_err(|_| ConfigError::Invalid {
                name: PORT,
                value: port,
            })?;
        }
        config.public_origin = match optional(ORIGIN) {
            Some(value) => parse_origin(&value).ok_or(ConfigError::PublicOrigin(value))?,
            None => default_origin(&config),
        };
        config.database_path = resolve_sqlite_path(optional(DATABASE_PATH));
        if let Some(url) = optional(DATABASE_URL) {
            if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
                return Err(ConfigError::DatabaseUrl);
            }
            config.database_url = Some(url);
        }
        // Parsed so an existing environment still boots. Registration is the
        // instance setting in the database, not this flag. OWLEE_OPEN_SIGNUP
        // is ignored.
        if let Some(signup) = optional(OPEN_SIGNUP) {
            config.open_signup = parse_bool(&signup).ok_or(ConfigError::Invalid {
                name: OPEN_SIGNUP,
                value: signup,
            })?;
        }
        if let Some(host) = optional(SMTP_HOST) {
            let from = optional(SMTP_FROM).ok_or(ConfigError::Missing(SMTP_FROM))?;
            let port = match optional(SMTP_PORT) {
                Some(value) => value.parse().map_err(|_| ConfigError::Invalid {
                    name: SMTP_PORT,
                    value,
                })?,
                None => 587,
            };
            config.smtp = Some(SmtpConfig {
                host,
                port,
                username: optional(SMTP_USERNAME),
                password: optional(SMTP_PASSWORD),
                from,
            });
        }
        if let Some(memory) = optional(ARGON_MEMORY) {
            config.argon_memory_kib = memory.parse().map_err(|_| ConfigError::Invalid {
                name: ARGON_MEMORY,
                value: memory,
            })?;
        }
        if let Some(targets) = connectivity_targets_from_env()? {
            config.connectivity_targets = targets;
        }
        if let Some(value) = optional(ALLOW_PRIVATE_WEBHOOKS) {
            config.allow_private_webhooks = parse_bool(&value).ok_or(ConfigError::Invalid {
                name: ALLOW_PRIVATE_WEBHOOKS,
                value,
            })?;
        }
        if let Some(hops) = optional(PROXY_HOPS) {
            config.proxy_hops = hops.parse().map_err(|_| ConfigError::Invalid {
                name: PROXY_HOPS,
                value: hops,
            })?;
        }
        if let Some(networks) = optional(TRUSTED_PROXIES) {
            config.trusted_proxies = parse_networks(&networks).ok_or(ConfigError::Invalid {
                name: TRUSTED_PROXIES,
                value: networks,
            })?;
        }
        if let Some(path) = optional(GEOIP_PATH) {
            config.geoip_path = Some(PathBuf::from(path));
        }
        match optional(APP_KEY) {
            Some(value) => {
                config.app_key = value.parse().map_err(|()| ConfigError::AppKey)?;
            }
            None if environment == EnvironmentMode::Production => {
                return Err(ConfigError::AppKey);
            }
            None => tracing::warn!(
                "OWLEE_APP_KEY is not set, so saved secrets use the public development key"
            ),
        }
        apply_ingest_env(&mut config)?;
        Ok(config)
    }

    pub fn apply_listen(&mut self, listen: SocketAddr) {
        self.bind = listen.ip();
        self.port = listen.port();
    }

    #[must_use]
    pub const fn listen_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind, self.port)
    }

    /// `serve` in production needs the origin that browsers open: the same-origin check and
    /// Secure cookies follow it. Other commands run without it. Loopback and plain `http://`
    /// are allowed with a warning, for a test box or a LAN install.
    pub fn check_serve_origin(&self) -> Result<(), ConfigError> {
        if self.environment != EnvironmentMode::Production {
            return Ok(());
        }
        if optional(ORIGIN).is_none() {
            return Err(ConfigError::PublicOriginMissing);
        }
        let origin = &self.public_origin;
        if origin_is_loopback(origin) {
            tracing::warn!(
                origin,
                "OWLEE_PUBLIC_ORIGIN is a loopback address, so sign-in fails from any other host"
            );
        }
        if !self.secure_cookies() {
            tracing::warn!(
                origin,
                "OWLEE_PUBLIC_ORIGIN is plain http, so the session cookie is not Secure"
            );
        }
        Ok(())
    }

    #[must_use]
    pub fn secure_cookies(&self) -> bool {
        self.public_origin.starts_with("https://")
    }

    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        if self.database_url.is_some() {
            "postgres"
        } else {
            "sqlite"
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentMode {
    Development,
    Production,
}

impl EnvironmentMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Production => "production",
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{name} is invalid: {value}")]
    Invalid { name: &'static str, value: String },
    #[error("missing {0}")]
    Missing(&'static str),
    #[error(
        "OWLEE_DATABASE_URL must be a postgres:// or postgresql:// URL. SQLite is the default; set OWLEE_DATABASE_PATH to move the file"
    )]
    DatabaseUrl,
    #[error(
        "OWLEE_PUBLIC_ORIGIN must be an http:// or https:// origin with no path, such as https://status.example.com: {0}"
    )]
    PublicOrigin(String),
    #[error(
        "OWLEE_PUBLIC_ORIGIN is required in production. Set it to the URL people open, such as https://status.example.com"
    )]
    PublicOriginMissing,
    // The value is a secret, so the message does not repeat it.
    #[error("OWLEE_APP_KEY must be base64 of 32 bytes. Make one with `openssl rand -base64 32`")]
    AppKey,
}

/// The 32-byte key that encrypts secrets saved in the database. Set as base64 in `OWLEE_APP_KEY`.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct AppKey([u8; 32]);

impl AppKey {
    /// Public, so anything sealed with it is only obscured. Development and tests only.
    pub const DEVELOPMENT: Self = Self([0x0e; 32]);

    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl FromStr for AppKey {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        use base64::Engine as _;

        base64::engine::general_purpose::STANDARD
            .decode(value.trim())
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .map(Self)
            .ok_or(())
    }
}

impl fmt::Debug for AppKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// Loopback and private networks: Docker, Traefik, and Coolify proxies live
/// here, and a visitor from the internet does not.
#[must_use]
pub fn default_trusted_proxies() -> Vec<IpNet> {
    [
        "127.0.0.0/8",
        "::1/128",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "fc00::/7",
    ]
    .iter()
    .filter_map(|network| network.parse().ok())
    .collect()
}

/// Comma-separated CIDRs or plain addresses. `none` trusts no proxy. A network
/// that holds every address (`0.0.0.0/0`, `::/0`) is refused.
fn parse_networks(value: &str) -> Option<Vec<IpNet>> {
    if value.eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let network = part
                .parse::<IpNet>()
                .ok()
                .or_else(|| part.parse::<IpAddr>().ok().map(IpNet::from))?;
            (network.prefix_len() > 0).then_some(network)
        })
        .collect()
}

fn optional(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// `OWLEE_CONNECTIVITY_TARGETS` alone, for `owlee agent`, which runs on hosts without the
/// server's app key or origin.
pub fn connectivity_targets_from_env() -> Result<Option<Vec<String>>, ConfigError> {
    optional(CONNECTIVITY_TARGETS)
        .map(|targets| {
            parse_targets(&targets).ok_or(ConfigError::Invalid {
                name: CONNECTIVITY_TARGETS,
                value: targets,
            })
        })
        .transpose()
}

/// The serialized origin (`https://status.example.com`), as browsers send it in `Origin`.
fn parse_origin(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    let bare = matches!(url.scheme(), "http" | "https")
        && url.has_host()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    bare.then(|| url.origin().ascii_serialization())
}

fn default_origin(config: &Config) -> String {
    let host = match config.bind {
        IpAddr::V4(ip) if ip.is_unspecified() => "127.0.0.1".to_owned(),
        IpAddr::V6(ip) => format!("[{ip}]"),
        other => other.to_string(),
    };
    format!("http://{host}:{}", config.port)
}

fn origin_is_loopback(origin: &str) -> bool {
    match url::Url::parse(origin)
        .ok()
        .and_then(|url| url.host().map(|host| host.to_owned()))
    {
        Some(url::Host::Domain(name)) => name == "localhost" || name.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unspecified(),
        None => false,
    }
}

fn resolve_sqlite_path(explicit: Option<String>) -> PathBuf {
    explicit
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/owlee.sqlite"))
}

fn parse_targets(value: &str) -> Option<Vec<String>> {
    if value == "off" {
        return Some(Vec::new());
    }
    value
        .split(',')
        .map(str::trim)
        .map(|target| {
            let (host, port) = target.rsplit_once(':')?;
            (!host.is_empty() && port.parse::<u16>().is_ok()).then(|| target.to_owned())
        })
        .collect()
}

fn apply_ingest_env(config: &mut Config) -> Result<(), ConfigError> {
    let ingest = &mut config.ingest;
    if let Some(value) = optional(ERROR_MAX_BODY) {
        ingest.error_max_body_bytes = parse_usize(ERROR_MAX_BODY, &value)?;
    }
    if let Some(value) = optional(ANALYTICS_MAX_BODY) {
        ingest.analytics_max_body_bytes = parse_usize(ANALYTICS_MAX_BODY, &value)?;
    }
    if let Some(value) = optional(ERROR_RATE_PROJECT) {
        ingest.error_rate_project_per_min = parse_u32(ERROR_RATE_PROJECT, &value)?;
    }
    if let Some(value) = optional(ERROR_BURST_PROJECT) {
        ingest.error_rate_project_burst = parse_u32(ERROR_BURST_PROJECT, &value)?;
    }
    if let Some(value) = optional(ERROR_RATE_IP) {
        ingest.error_rate_ip_per_min = parse_u32(ERROR_RATE_IP, &value)?;
    }
    if let Some(value) = optional(ERROR_BURST_IP) {
        ingest.error_rate_ip_burst = parse_u32(ERROR_BURST_IP, &value)?;
    }
    if let Some(value) = optional(ANALYTICS_RATE_SITE) {
        ingest.analytics_rate_site_per_min = parse_u32(ANALYTICS_RATE_SITE, &value)?;
    }
    if let Some(value) = optional(ANALYTICS_BURST_SITE) {
        ingest.analytics_rate_site_burst = parse_u32(ANALYTICS_BURST_SITE, &value)?;
    }
    if let Some(value) = optional(ANALYTICS_RATE_IP) {
        ingest.analytics_rate_ip_per_min = parse_u32(ANALYTICS_RATE_IP, &value)?;
    }
    if let Some(value) = optional(ANALYTICS_BURST_IP) {
        ingest.analytics_rate_ip_burst = parse_u32(ANALYTICS_BURST_IP, &value)?;
    }
    if let Some(value) = optional(CHECK_RETENTION) {
        ingest.check_retention_days = parse_i64(CHECK_RETENTION, &value)?;
    }
    if let Some(value) = optional(ERROR_RETENTION) {
        ingest.error_retention_days = parse_i64(ERROR_RETENTION, &value)?;
    }
    if let Some(value) = optional(ANALYTICS_RETENTION) {
        ingest.analytics_retention_days = parse_i64(ANALYTICS_RETENTION, &value)?;
    }
    if let Some(value) = optional(REPLAY_MAX_BODY) {
        ingest.replay_max_body_bytes = parse_usize(REPLAY_MAX_BODY, &value)?;
    }
    if let Some(value) = optional(REPLAY_MAX_SEGMENT) {
        ingest.replay_max_segment_bytes = parse_usize(REPLAY_MAX_SEGMENT, &value)?;
    }
    if let Some(value) = optional(REPLAY_RATE_APP) {
        ingest.replay_rate_app_per_min = parse_u32(REPLAY_RATE_APP, &value)?;
    }
    if let Some(value) = optional(REPLAY_RATE_IP) {
        ingest.replay_rate_ip_per_min = parse_u32(REPLAY_RATE_IP, &value)?;
    }
    if let Some(value) = optional(REPLAY_RETENTION) {
        ingest.replay_retention_days = parse_i64(REPLAY_RETENTION, &value)?;
    }
    if let Some(value) = optional(ERROR_KEEP_FIRST) {
        ingest.error_keep_first = parse_i64(ERROR_KEEP_FIRST, &value)?.max(1);
    }
    if let Some(value) = optional(ERROR_SAMPLE_WINDOW) {
        ingest.error_sample_window_secs = parse_i64(ERROR_SAMPLE_WINDOW, &value)?.max(1);
    }
    if let Some(value) = optional(ALLOW_LOCALHOST) {
        ingest.allow_localhost = parse_bool(&value).ok_or(ConfigError::Invalid {
            name: ALLOW_LOCALHOST,
            value,
        })?;
    }
    if let Some(value) = optional(DOMAIN_RECHECK) {
        ingest.domain_recheck_secs = parse_i64(DOMAIN_RECHECK, &value)?.max(60);
    }
    if let Some(value) = optional(ERROR_DAILY_QUOTA) {
        ingest.error_daily_quota = Some(parse_i64(ERROR_DAILY_QUOTA, &value)?);
    }
    if let Some(value) = optional(ANALYTICS_DAILY_QUOTA) {
        ingest.analytics_daily_quota = Some(parse_i64(ANALYTICS_DAILY_QUOTA, &value)?);
    }
    if let Some(value) = optional(LOGIN_RATE_IP) {
        ingest.login_rate_ip_per_min = parse_u32(LOGIN_RATE_IP, &value)?;
    }
    if let Some(value) = optional(LOGIN_BURST_IP) {
        ingest.login_rate_ip_burst = parse_u32(LOGIN_BURST_IP, &value)?;
    }
    if let Some(value) = optional(LOGIN_RATE_ACCOUNT) {
        ingest.login_rate_account_per_min = parse_u32(LOGIN_RATE_ACCOUNT, &value)?;
    }
    if let Some(value) = optional(LOGIN_BURST_ACCOUNT) {
        ingest.login_rate_account_burst = parse_u32(LOGIN_BURST_ACCOUNT, &value)?;
    }
    if let Some(value) = optional(ANALYTICS_GEO) {
        ingest.analytics_geo = value.parse().map_err(|()| ConfigError::Invalid {
            name: ANALYTICS_GEO,
            value,
        })?;
    }
    Ok(())
}

fn parse_u32(name: &'static str, value: &str) -> Result<u32, ConfigError> {
    value.parse().map_err(|_| ConfigError::Invalid {
        name,
        value: value.to_owned(),
    })
}

fn parse_usize(name: &'static str, value: &str) -> Result<usize, ConfigError> {
    value.parse().map_err(|_| ConfigError::Invalid {
        name,
        value: value.to_owned(),
    })
}

fn parse_i64(name: &'static str, value: &str) -> Result<i64, ConfigError> {
    let parsed = value.parse::<i64>().map_err(|_| ConfigError::Invalid {
        name,
        value: value.to_owned(),
    })?;
    if parsed < 1 {
        return Err(ConfigError::Invalid {
            name,
            value: value.to_owned(),
        });
    }
    Ok(parsed)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// `url` with its password replaced, for logs.
#[must_use]
pub fn redact_database_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            if parsed.password().is_some() {
                let _ = parsed.set_password(Some("REDACTED"));
            }
            parsed.into()
        }
        Err(_) => "[invalid database URL]".to_owned(),
    }
}

impl fmt::Display for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let database = self
            .database_url
            .as_deref()
            .map(redact_database_url)
            .unwrap_or_else(|| self.database_path.display().to_string());
        write!(
            formatter,
            "env={} listen={} database={database} signup={}",
            self.environment.as_str(),
            self.listen_addr(),
            if self.open_signup { "open" } else { "closed" }
        )
    }
}

impl FromStr for EnvironmentMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "development" => Ok(Self::Development),
            "production" => Ok(Self::Production),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn trusted_proxies_parse_and_refuse_everything() {
        let networks = super::parse_networks("10.0.0.0/8, 203.0.113.7, ::1").unwrap();
        assert_eq!(networks.len(), 3);
        assert!(networks[1].contains(&"203.0.113.7".parse::<std::net::IpAddr>().unwrap()));
        assert_eq!(super::parse_networks("none").unwrap().len(), 0);
        assert!(super::parse_networks("0.0.0.0/0").is_none());
        assert!(super::parse_networks("::/0").is_none());
        assert!(super::parse_networks("proxy.local").is_none());
    }

    use std::path::PathBuf;

    use super::*;

    #[test]
    fn parses_connectivity_targets() {
        assert_eq!(
            parse_targets("1.1.1.1:443, [::1]:53"),
            Some(vec!["1.1.1.1:443".to_owned(), "[::1]:53".to_owned()])
        );
        assert_eq!(parse_targets("off"), Some(Vec::new()));
        assert_eq!(parse_targets("1.1.1.1"), None);
        assert_eq!(parse_targets("1.1.1.1:443,"), None);
    }

    #[test]
    fn redacts_passwords() {
        assert_eq!(
            redact_database_url("postgres://status:secret@localhost/status"),
            "postgres://status:REDACTED@localhost/status"
        );
    }

    /// `Config::from_env` reads process globals.
    fn lock_process_env() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn rejects_sqlite_database_urls() {
        let _env = lock_process_env();
        // SAFETY: the lock above is the only other writer of this variable.
        unsafe {
            std::env::set_var("OWLEE_DATABASE_URL", "sqlite://data/ignored.sqlite");
        }
        let error = Config::from_env().unwrap_err();
        assert!(matches!(error, ConfigError::DatabaseUrl));
        unsafe {
            std::env::remove_var("OWLEE_DATABASE_URL");
        }
    }

    #[test]
    fn app_key_is_required_in_production_and_parsed_as_base64() {
        let _env = lock_process_env();
        // SAFETY: the lock above is the only other writer of these variables.
        unsafe {
            std::env::set_var("OWLEE_ENV", "production");
            std::env::remove_var("OWLEE_APP_KEY");
        }
        let missing = Config::from_env().unwrap_err();
        assert_eq!(
            missing.to_string(),
            "OWLEE_APP_KEY must be base64 of 32 bytes. Make one with `openssl rand -base64 32`"
        );
        unsafe {
            std::env::set_var(
                "OWLEE_APP_KEY",
                "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
            );
        }
        let config = Config::from_env().unwrap();
        assert_eq!(config.app_key, AppKey::new([1; 32]));
        assert_eq!(format!("{:?}", config.app_key), "[REDACTED]");
        unsafe {
            std::env::set_var("OWLEE_APP_KEY", "c2hvcnQ=");
        }
        assert!(matches!(
            Config::from_env().unwrap_err(),
            ConfigError::AppKey
        ));
        unsafe {
            std::env::remove_var("OWLEE_APP_KEY");
            std::env::remove_var("OWLEE_ENV");
        }
    }

    const KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    #[test]
    fn unset_environment_is_production() {
        let _env = lock_process_env();
        // SAFETY: the lock above is the only other writer of these variables.
        unsafe {
            std::env::remove_var("OWLEE_ENV");
            std::env::remove_var("OWLEE_APP_KEY");
        }
        assert!(matches!(
            Config::from_env().unwrap_err(),
            ConfigError::AppKey
        ));
        unsafe {
            std::env::set_var("OWLEE_APP_KEY", KEY);
        }
        let config = Config::from_env().unwrap();
        assert_eq!(config.environment, EnvironmentMode::Production);
        assert!(!config.allow_private_webhooks);
        unsafe {
            std::env::remove_var("OWLEE_APP_KEY");
        }
    }

    #[test]
    fn serve_in_production_refuses_a_missing_or_malformed_public_origin() {
        let _env = lock_process_env();
        // SAFETY: the lock above is the only other writer of these variables.
        unsafe {
            std::env::set_var("OWLEE_ENV", "production");
            std::env::set_var("OWLEE_APP_KEY", KEY);
            std::env::remove_var("OWLEE_PUBLIC_ORIGIN");
        }
        let unset = Config::from_env().unwrap();
        assert_eq!(
            unset.check_serve_origin().unwrap_err().to_string(),
            "OWLEE_PUBLIC_ORIGIN is required in production. Set it to the URL people open, such as https://status.example.com"
        );
        for bad in [
            "status.example.com",
            "ftp://status.example.com",
            "https://status.example.com/owlee",
            "https://status.example.com/?a=1",
            "https://user@status.example.com",
            "https://",
        ] {
            unsafe {
                std::env::set_var("OWLEE_PUBLIC_ORIGIN", bad);
            }
            assert_eq!(
                Config::from_env().unwrap_err().to_string(),
                format!(
                    "OWLEE_PUBLIC_ORIGIN must be an http:// or https:// origin with no path, such as https://status.example.com: {bad}"
                ),
            );
        }
        for (value, origin, secure) in [
            (
                "https://Status.Example.com/",
                "https://status.example.com",
                true,
            ),
            (
                "https://status.example.com:443",
                "https://status.example.com",
                true,
            ),
            ("http://10.0.0.5:8080", "http://10.0.0.5:8080", false),
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080", false),
        ] {
            unsafe {
                std::env::set_var("OWLEE_PUBLIC_ORIGIN", value);
            }
            let config = Config::from_env().unwrap();
            assert_eq!(config.public_origin, origin);
            assert_eq!(config.secure_cookies(), secure);
            config.check_serve_origin().unwrap();
        }
        unsafe {
            std::env::set_var("OWLEE_ENV", "development");
            std::env::remove_var("OWLEE_PUBLIC_ORIGIN");
        }
        let development = Config::from_env().unwrap();
        assert_eq!(development.public_origin, "http://127.0.0.1:43123");
        development.check_serve_origin().unwrap();
        unsafe {
            std::env::remove_var("OWLEE_ENV");
            std::env::remove_var("OWLEE_APP_KEY");
        }
    }

    #[test]
    fn loopback_origins_are_recognized() {
        for origin in [
            "http://127.0.0.1:8080",
            "http://localhost",
            "http://[::1]:80",
            "http://0.0.0.0",
        ] {
            assert!(origin_is_loopback(origin), "{origin}");
        }
        assert!(!origin_is_loopback("https://status.example.com"));
        assert!(!origin_is_loopback("http://192.168.1.10"));
    }

    #[test]
    fn analytics_geo_defaults_to_city_and_rejects_unknown_values() {
        let _env = lock_process_env();
        // SAFETY: the lock above is the only other writer of this variable.
        unsafe {
            std::env::set_var("OWLEE_ENV", "development");
        }
        assert_eq!(
            Config::from_env().unwrap().ingest.analytics_geo,
            GeoPrecision::City
        );
        // SAFETY: the lock above is the only other writer of this variable.
        unsafe {
            std::env::set_var("OWLEE_ANALYTICS_GEO", "country");
        }
        assert_eq!(
            Config::from_env().unwrap().ingest.analytics_geo,
            GeoPrecision::Country
        );
        unsafe {
            std::env::set_var("OWLEE_ANALYTICS_GEO", "street");
        }
        assert_eq!(
            Config::from_env().unwrap_err().to_string(),
            "OWLEE_ANALYTICS_GEO is invalid: street"
        );
        unsafe {
            std::env::remove_var("OWLEE_ANALYTICS_GEO");
            std::env::remove_var("OWLEE_ENV");
        }
    }

    #[test]
    fn database_path_uses_the_given_file_or_the_default() {
        assert_eq!(
            resolve_sqlite_path(None),
            PathBuf::from("data/owlee.sqlite")
        );
        assert_eq!(
            resolve_sqlite_path(Some("/tmp/custom.sqlite".to_owned())),
            PathBuf::from("/tmp/custom.sqlite")
        );
    }
}
