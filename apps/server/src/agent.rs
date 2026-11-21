//! `owlee agent` runs the checks in its own config (a TOML file, `OWLEE_AGENT_CHECK_<n>_<KEY>`
//! environment variables, or both) from inside a private network. It tells the server which checks exist, judges each result itself, and sends only the outcome.
//! The server never tells an agent what to connect to, and secrets never leave the agent host.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use owlee_domain::{Assertion, CheckType, Rules, StrCompare};
use owlee_platform::Id;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::json;

use crate::checker::{self, Database, Job, Login};
use crate::connectivity::Connectivity;
use crate::engine::{self, Outcome};
use crate::state::HttpClients;

const HEARTBEAT_EVERY: Duration = Duration::from_secs(15);
const RETRY_EVERY: Duration = Duration::from_secs(10);
const ENV_PREFIX: &str = "OWLEE_AGENT_CHECK_";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(rename = "check", default)]
    checks: Vec<CheckConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckConfig {
    name: String,
    #[serde(rename = "type")]
    check_type: CheckType,
    target: String,
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(default = "default_timeout")]
    timeout: u64,
    #[serde(default = "default_threshold")]
    failure_threshold: i32,
    degraded_after_ms: Option<i64>,
    user: Option<String>,
    password_env: Option<String>,
    database: Option<String>,
    expected_status: Option<i64>,
    body_contains: Option<String>,
}

const fn default_interval() -> u64 {
    60
}

const fn default_timeout() -> u64 {
    10
}

const fn default_threshold() -> i32 {
    3
}

enum Probe {
    Job(Job),
    Database(Database, Login),
}

/// A check from the config with its secrets resolved from the environment.
struct Check {
    name: String,
    check_type: CheckType,
    target: String,
    interval: Duration,
    timeout: Duration,
    failure_threshold: i32,
    probe: Probe,
    rules: Rules,
}

fn load(path: Option<&Path>) -> Result<Vec<Check>, String> {
    let text = path
        .map(|path| {
            std::fs::read_to_string(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))
        })
        .transpose()?;
    parse(text.as_deref(), std::env::vars(), |name| std::env::var(name).ok())
}

/// Reads the checks from the config file text and from `OWLEE_AGENT_CHECK_<n>_<KEY>` variables.
fn parse(
    text: Option<&str>,
    vars: impl IntoIterator<Item = (String, String)>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Vec<Check>, String> {
    let mut checks = match text {
        Some(text) => {
            let config: Config = toml::from_str(text).map_err(|error| error.to_string())?;
            config.checks
        }
        None => Vec::new(),
    };
    checks.extend(env_checks(vars)?);
    if checks.is_empty() {
        return Err(format!(
            "no checks: set {ENV_PREFIX}1_NAME, {ENV_PREFIX}1_TYPE, and {ENV_PREFIX}1_TARGET, \
             or a config file with [[check]] entries"
        ));
    }
    checks
        .into_iter()
        .map(|check| resolve(check, &env))
        .collect()
}

/// Groups `OWLEE_AGENT_CHECK_<n>_<KEY>` variables into one check for each `<n>`, in number
/// order. `<KEY>` is a config key in upper case. `PASSWORD` holds the password itself.
fn env_checks(
    vars: impl IntoIterator<Item = (String, String)>,
) -> Result<Vec<CheckConfig>, String> {
    let mut tables: BTreeMap<u32, toml::Table> = BTreeMap::new();
    for (key, value) in vars {
        let Some(rest) = key.strip_prefix(ENV_PREFIX) else {
            continue;
        };
        let (index, field) = rest
            .split_once('_')
            .and_then(|(index, field)| Some((index.parse::<u32>().ok()?, field)))
            .ok_or_else(|| format!("{key}: use {ENV_PREFIX}<number>_<KEY>"))?;
        let field = field.to_ascii_lowercase();
        let (field, value) = match field.as_str() {
            "password" => ("password_env".to_owned(), toml::Value::String(key.clone())),
            "interval" | "timeout" | "failure_threshold" | "degraded_after_ms"
            | "expected_status" => {
                let number = value
                    .trim()
                    .parse()
                    .map_err(|_| format!("{key} must be a whole number"))?;
                (field, toml::Value::Integer(number))
            }
            _ => (field, toml::Value::String(value)),
        };
        if tables.entry(index).or_default().insert(field, value).is_some() {
            return Err(format!("{key} sets a key that another variable already sets"));
        }
    }
    tables
        .into_iter()
        .map(|(index, table)| {
            table
                .try_into()
                .map_err(|error| format!("{ENV_PREFIX}{index}_*: {error}"))
        })
        .collect()
}

fn resolve(check: CheckConfig, env: &impl Fn(&str) -> Option<String>) -> Result<Check, String> {
    let name = check.name.trim().to_owned();
    let password = match &check.password_env {
        Some(variable) => Some(env(variable).ok_or_else(|| {
            format!("check \"{name}\": environment variable {variable} is not set")
        })?),
        None => None,
    };
    let database = match check.check_type {
        CheckType::Http | CheckType::Tcp => None,
        CheckType::Postgres => Some(Database::Postgres),
        CheckType::Mysql => Some(Database::Mysql),
        CheckType::Redis => Some(Database::Redis),
    };
    if matches!(database, Some(Database::Postgres | Database::Mysql)) && check.user.is_none() {
        return Err(format!(
            "check \"{name}\": set user for a {} check",
            check.check_type.as_str()
        ));
    }
    let probe = match database {
        Some(kind) => Probe::Database(
            kind,
            Login {
                user: check.user,
                password,
                database: check.database,
            },
        ),
        None => Probe::Job(Job {
            job_type: check.check_type.job_type(),
            url: check.target.clone(),
            method: "GET".to_owned(),
            headers: Vec::new(),
            body: String::new(),
            timeout_ms: i64::try_from(check.timeout.saturating_mul(1000)).unwrap_or(i64::MAX),
            follow_redirects: true,
        }),
    };
    Ok(Check {
        name,
        check_type: check.check_type,
        target: check.target,
        interval: Duration::from_secs(check.interval),
        timeout: Duration::from_secs(check.timeout),
        failure_threshold: check.failure_threshold,
        probe,
        rules: Rules {
            job_type: check.check_type.job_type(),
            expected_status: check.expected_status,
            degraded_after_ms: check.degraded_after_ms,
            assertions: check
                .body_contains
                .map(|target| Assertion::Body {
                    compare: StrCompare::Contains,
                    target,
                })
                .into_iter()
                .collect(),
        },
    })
}

#[derive(Clone)]
struct Server {
    client: reqwest::Client,
    base: String,
    token: String,
}

#[derive(Deserialize)]
struct Registered {
    checks: Vec<RegisteredCheck>,
}

#[derive(Deserialize)]
struct RegisteredCheck {
    name: String,
    monitor_id: Id,
}

pub async fn run(
    server: &str,
    token: String,
    config: Option<&Path>,
    connectivity: Connectivity,
) -> Result<(), Box<dyn std::error::Error>> {
    let checks = load(config)?;
    let server = Server {
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("Owlee-Agent/", env!("CARGO_PKG_VERSION")))
            .build()?,
        base: server.trim_end_matches('/').to_owned(),
        token,
    };
    let http = HttpClients::new()?;
    let ids = loop {
        match server.register(&checks).await {
            Ok(ids) => break ids,
            Err(Rejected::Final(reason)) => return Err(reason.into()),
            Err(Rejected::Retry(error)) => {
                tracing::warn!(%error, "could not register checks; retrying");
                tokio::time::sleep(RETRY_EVERY).await;
            }
        }
    };
    tracing::info!(server = %server.base, checks = checks.len(), "Owlee agent started");
    for check in checks {
        let Some(&monitor_id) = ids.get(&check.name) else {
            continue;
        };
        let (server, http, connectivity) = (server.clone(), http.clone(), connectivity.clone());
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(check.interval);
            loop {
                ticks.tick().await;
                let outcome = run_check(&check, &http).await;
                if outcome.error.is_some() && !connectivity.online().await {
                    continue;
                }
                if let Err(error) = server.report(monitor_id, &outcome).await {
                    tracing::warn!(%error, check = %check.name, "could not report a check");
                }
            }
        });
    }
    let mut heartbeat = tokio::time::interval(HEARTBEAT_EVERY);
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if let Err(error) = server.heartbeat().await {
                    tracing::warn!(%error, "heartbeat failed");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutting down");
                return Ok(());
            }
        }
    }
}

async fn run_check(check: &Check, http: &HttpClients) -> Outcome {
    let probe = match &check.probe {
        Probe::Job(job) => checker::probe(http, job).await,
        Probe::Database(kind, login) => {
            checker::database_probe(*kind, &check.target, login, check.timeout).await
        }
    };
    engine::judge_probe(&check.rules, &probe)
}

enum Rejected {
    /// The server refused the token or the config. Retrying cannot help.
    Final(String),
    Retry(reqwest::Error),
}

impl Server {
    async fn register(&self, checks: &[Check]) -> Result<HashMap<String, Id>, Rejected> {
        let body = json!({
            "checks": checks.iter().map(|check| json!({
                "name": check.name,
                "check_type": check.check_type,
                "target": check.target,
                "interval_seconds": check.interval.as_secs(),
                "timeout_ms": check.timeout.as_millis(),
                "failure_threshold": check.failure_threshold,
            })).collect::<Vec<_>>(),
        });
        let response = self
            .client
            .post(format!("{}/api/agent/checks", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(Rejected::Retry)?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(Rejected::Final(
                "the server rejected the agent token; regenerate it in the dashboard".to_owned(),
            ));
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(Rejected::Retry(
                response
                    .error_for_status()
                    .expect_err("429 is an error status"),
            ));
        }
        if status.is_client_error() {
            let problem: serde_json::Value = response.json().await.unwrap_or_default();
            let reason = problem["fields"]["checks"]
                .as_str()
                .or(problem["detail"].as_str())
                .unwrap_or("unknown reason");
            return Err(Rejected::Final(format!(
                "the server rejected the config: {reason}"
            )));
        }
        let registered: Registered = response
            .error_for_status()
            .map_err(Rejected::Retry)?
            .json()
            .await
            .map_err(Rejected::Retry)?;
        Ok(registered
            .checks
            .into_iter()
            .map(|check| (check.name, check.monitor_id))
            .collect())
    }

    async fn heartbeat(&self) -> Result<(), reqwest::Error> {
        self.client
            .post(format!("{}/api/agent/heartbeat", self.base))
            .bearer_auth(&self.token)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn report(&self, monitor_id: Id, outcome: &Outcome) -> Result<(), reqwest::Error> {
        let mut body = json!(outcome);
        body["monitor_id"] = json!(monitor_id);
        self.client
            .post(format!("{}/api/agent/results", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
[[check]]
name = "Main DB"
type = "postgres"
target = "localhost:5432"
user = "status"
password_env = "PG_PASSWORD"
database = "app"
interval = 30

[[check]]
name = "API"
type = "http"
target = "http://api.internal/health"
body_contains = "ok"
"#;

    #[test]
    fn reads_checks_and_resolves_passwords_from_the_environment() {
        let checks = parse(Some(CONFIG), [], |name| {
            (name == "PG_PASSWORD").then(|| "secret".to_owned())
        })
        .unwrap();
        let Probe::Database(Database::Postgres, login) = &checks[0].probe else {
            panic!("first check is a postgres probe");
        };
        assert_eq!(login.password.as_deref(), Some("secret"));
        assert_eq!(login.database.as_deref(), Some("app"));
        assert_eq!(checks[0].interval, Duration::from_secs(30));
        assert_eq!(checks[1].interval, Duration::from_secs(60));
        assert_eq!(
            checks[1].rules.assertions,
            vec![Assertion::Body {
                compare: StrCompare::Contains,
                target: "ok".to_owned()
            }]
        );
    }

    #[test]
    fn rejects_a_missing_password_variable() {
        let error = parse(Some(CONFIG), [], |_| None).err().unwrap();
        assert_eq!(
            error,
            "check \"Main DB\": environment variable PG_PASSWORD is not set"
        );
    }

    async fn register_against(status: axum::http::StatusCode) -> Rejected {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/api/agent/checks",
            axum::routing::post(move || async move { status }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let server = Server {
            client: reqwest::Client::new(),
            base: format!("http://{addr}"),
            token: "token".to_owned(),
        };
        server.register(&[]).await.err().expect("refused")
    }

    #[tokio::test]
    async fn a_rate_limited_registration_is_retried() {
        let rejected = register_against(axum::http::StatusCode::TOO_MANY_REQUESTS).await;
        assert!(matches!(rejected, Rejected::Retry(_)));
        let rejected = register_against(axum::http::StatusCode::UNPROCESSABLE_ENTITY).await;
        assert!(matches!(rejected, Rejected::Final(_)));
    }

    #[test]
    fn rejects_unknown_keys() {
        let error = parse(
            Some("[[check]]\nname = \"x\"\ntype = \"tcp\"\ntarget = \"a:1\"\npasword_env = \"X\"\n"),
            [],
            |_| None,
        )
        .err()
        .unwrap();
        assert!(error.contains("unknown field `pasword_env`"), "{error}");
    }

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn reads_checks_from_environment_variables_only() {
        let vars = vars(&[
            ("OWLEE_AGENT_CHECK_2_NAME", "API"),
            ("OWLEE_AGENT_CHECK_2_TYPE", "http"),
            ("OWLEE_AGENT_CHECK_2_TARGET", "http://api.internal/health"),
            ("OWLEE_AGENT_CHECK_2_EXPECTED_STATUS", "204"),
            ("OWLEE_AGENT_CHECK_1_NAME", "Main DB"),
            ("OWLEE_AGENT_CHECK_1_TYPE", "postgres"),
            ("OWLEE_AGENT_CHECK_1_TARGET", "localhost:5432"),
            ("OWLEE_AGENT_CHECK_1_USER", "status"),
            ("OWLEE_AGENT_CHECK_1_PASSWORD", "secret"),
            ("OWLEE_AGENT_CHECK_1_DATABASE", "app"),
            ("OWLEE_AGENT_CHECK_1_INTERVAL", "30"),
            ("OWLEE_AGENT_TOKEN", "ignored"),
        ]);
        let lookup = vars.clone();
        let checks = parse(None, vars, |name| {
            lookup.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone())
        })
        .unwrap();
        assert_eq!(checks[0].name, "Main DB");
        let Probe::Database(Database::Postgres, login) = &checks[0].probe else {
            panic!("first check is a postgres probe");
        };
        assert_eq!(login.password.as_deref(), Some("secret"));
        assert_eq!(login.database.as_deref(), Some("app"));
        assert_eq!(checks[0].interval, Duration::from_secs(30));
        assert_eq!(checks[1].name, "API");
        assert_eq!(checks[1].rules.expected_status, Some(204));
    }

    #[test]
    fn rejects_bad_check_variables() {
        let error = |pairs: &[(&str, &str)]| parse(None, vars(pairs), |_| None).err().unwrap();
        assert!(error(&[]).starts_with("no checks"));
        assert_eq!(
            error(&[("OWLEE_AGENT_CHECK_X_NAME", "a")]),
            "OWLEE_AGENT_CHECK_X_NAME: use OWLEE_AGENT_CHECK_<number>_<KEY>"
        );
        assert_eq!(
            error(&[("OWLEE_AGENT_CHECK_1_INTERVAL", "soon")]),
            "OWLEE_AGENT_CHECK_1_INTERVAL must be a whole number"
        );
        let missing = error(&[
            ("OWLEE_AGENT_CHECK_1_NAME", "a"),
            ("OWLEE_AGENT_CHECK_1_TYPE", "tcp"),
        ]);
        assert!(missing.contains("missing field `target`"), "{missing}");
    }
}
