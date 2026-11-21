use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use owlee_platform::{Config, TimestampMillis};

use crate::accounts;
use crate::agent;
use crate::connectivity::Connectivity;
use crate::error::AppError;
use crate::instance_settings;
use crate::ownership;
use crate::repo;
use crate::scheduler;
use crate::seed;
use crate::setup;
use crate::state::{self, BootError};

#[derive(Parser)]
#[command(
    name = "owlee",
    version,
    about = "Owlee status pages, incidents, and uptime monitoring"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the HTTP server and the check scheduler.
    Serve {
        /// Listen address, for example 127.0.0.1:43123.
        #[arg(long)]
        listen: Option<SocketAddr>,
        /// SQLite database file. Ignored when a Postgres URL is configured.
        #[arg(long)]
        database: Option<PathBuf>,
    },
    /// Apply migrations and exit.
    Migrate {
        #[arg(long)]
        database: Option<PathBuf>,
    },
    /// Create the demo account, monitors, status page, and incident history.
    Seed {
        #[arg(long)]
        database: Option<PathBuf>,
    },
    /// Exit successfully when /api/health/ready answers on the configured port.
    Healthcheck,
    /// One-time setup URL for the first owner account.
    SetupToken {
        /// SQLite database file. Ignored when a Postgres URL is configured.
        #[arg(long)]
        database: Option<PathBuf>,
        #[command(subcommand)]
        command: SetupTokenCommand,
    },
    /// Create an account, disable or enable it, or set its password.
    /// On SQLite, stop Owlee first. On Postgres, this can run while Owlee is up.
    User {
        /// SQLite database file. Ignored when a Postgres URL is configured.
        #[arg(long)]
        database: Option<PathBuf>,
        #[command(subcommand)]
        command: UserCommand,
    },
    /// Run checks from this network and report them to a Owlee server. Checks come from a
    /// config file, OWLEE_AGENT_CHECK_<n>_<KEY> environment variables, or both.
    Agent {
        /// Owlee server URL, for example https://status.example.com.
        #[arg(long, env = "OWLEE_AGENT_SERVER")]
        server: String,
        /// Token shown once when the agent was created.
        #[arg(long, env = "OWLEE_AGENT_TOKEN", hide_env_values = true)]
        token: String,
        /// TOML file with [[check]] entries. Optional when the checks are in environment variables.
        #[arg(long, env = "OWLEE_AGENT_CONFIG")]
        config: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum UserCommand {
    /// Create a member account. Asks for the password unless --password-stdin is set.
    Create {
        #[arg(long)]
        email: String,
        /// Display name. Defaults to the part of the email before the @.
        #[arg(long)]
        name: Option<String>,
        /// Read the password from the first line of standard input.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Block sign-in and end every session. The account's data stays.
    Disable {
        #[arg(long)]
        email: String,
    },
    /// Allow a disabled account to sign in again.
    Enable {
        #[arg(long)]
        email: String,
    },
    /// Replace the password and end every session. Works for the root account too.
    SetPassword {
        #[arg(long)]
        email: String,
        /// Read the password from the first line of standard input.
        #[arg(long)]
        password_stdin: bool,
    },
}

#[derive(Subcommand)]
enum SetupTokenCommand {
    /// Print a setup URL when setup is unfinished and no unused token is stored.
    Init {
        /// Public origin to put in the URL. Defaults to OWLEE_PUBLIC_ORIGIN.
        #[arg(long)]
        origin: Option<String>,
    },
    /// Replace the unused setup token. Refuses after setup is complete.
    /// On SQLite, stop Owlee first. On Postgres, this can run while Owlee is up.
    Rotate {
        /// Public origin to put in the URL. Defaults to OWLEE_PUBLIC_ORIGIN.
        #[arg(long)]
        origin: Option<String>,
    },
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve {
        listen: None,
        database: None,
    }) {
        Command::Serve { listen, database } => {
            let config = load(listen, database)?;
            config.check_serve_origin()?;
            let _sqlite_owner = sqlite_owner(&config)?;
            let addr = config.listen_addr();
            let state = state::boot(config).await?;
            if let Some(issued) =
                setup::initialize_setup_token(&state.db, TimestampMillis::now()).await?
            {
                let url = setup::setup_url(&state.config.public_origin, &issued.token);
                eprintln!("Initial setup URL: {url}");
            }
            scheduler::spawn(state.clone());
            let listener = tokio::net::TcpListener::bind(addr).await?;
            let bound = listener.local_addr()?;
            tracing::info!("Owlee listening on http://{bound}");
            axum::serve(
                listener,
                crate::routes::router(state)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        }
        Command::Migrate { database } => {
            let config = load(None, database)?;
            state::boot(config).await?;
            println!("migrations applied");
        }
        Command::Seed { database } => {
            let config = load(None, database)?;
            seed::refuse_in_production(&config)?;
            let state = state::boot(config).await?;
            if seed::seed(&state).await? {
                println!("seeded {DEMO}", DEMO = seed::DEMO_EMAIL);
                println!("password: {}", seed::DEMO_PASSWORD);
            } else {
                println!("database already has an account; left it unchanged");
            }
        }
        Command::Agent {
            server,
            token,
            config,
        } => {
            let targets = owlee_platform::connectivity_targets_from_env()?
                .unwrap_or_else(|| Config::development().connectivity_targets);
            agent::run(
                &server,
                token,
                config.as_deref(),
                Connectivity::new(targets),
            )
            .await?;
        }
        Command::SetupToken { database, command } => {
            let config = load(None, database)?;
            let _sqlite_owner = sqlite_owner(&config)?;
            let origin = match &command {
                SetupTokenCommand::Init { origin } | SetupTokenCommand::Rotate { origin } => {
                    origin.clone()
                }
            };
            let state = state::boot(config).await?;
            let origin = origin.unwrap_or_else(|| state.config.public_origin.clone());
            let now = TimestampMillis::now();
            match command {
                SetupTokenCommand::Init { .. } => {
                    match setup::initialize_setup_token(&state.db, now).await? {
                        Some(issued) => println!("{}", setup::setup_url(&origin, &issued.token)),
                        None => {
                            println!("setup token already exists or setup is complete");
                        }
                    }
                }
                SetupTokenCommand::Rotate { .. } => {
                    let issued = setup::rotate_setup_token(&state.db, now).await?;
                    println!("{}", setup::setup_url(&origin, &issued.token));
                }
            }
        }
        Command::User { database, command } => {
            let config = load(None, database)?;
            let _sqlite_owner = sqlite_owner(&config)?;
            let state = state::boot(config).await?;
            println!("{}", user_command(&state, command).await?);
        }
        Command::Healthcheck => {
            let config = Config::from_env()?;
            let url = format!("http://127.0.0.1:{}/api/health/ready", config.port);
            let response = reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()?
                .get(url)
                .send()
                .await?;
            if !response.status().is_success() {
                return Err(format!("health returned {}", response.status()).into());
            }
        }
    }
    Ok(())
}

async fn user_command(
    state: &state::AppState,
    command: UserCommand,
) -> Result<String, Box<dyn std::error::Error>> {
    let now = TimestampMillis::now();
    let (action, done, user) = match command {
        UserCommand::Create {
            email,
            name,
            password_stdin,
        } => {
            let password = read_password(password_stdin)?;
            let user = accounts::create_account(state, &email, name, password, now)
                .await
                .map_err(cli_error)?;
            ("user.created", "created", user)
        }
        UserCommand::Disable { email } => {
            let user = existing(state, &email).await?;
            if instance_settings::root_user_id(&state.db).await? == Some(user.id) {
                return Err("the root account cannot be disabled".into());
            }
            accounts::disable_account(state, user.id, true, now)
                .await
                .map_err(cli_error)?;
            ("user.disabled", "disabled", user)
        }
        UserCommand::Enable { email } => {
            let user = existing(state, &email).await?;
            accounts::disable_account(state, user.id, false, now)
                .await
                .map_err(cli_error)?;
            ("user.enabled", "enabled", user)
        }
        UserCommand::SetPassword {
            email,
            password_stdin,
        } => {
            let user = existing(state, &email).await?;
            let password = read_password(password_stdin)?;
            accounts::reset_password(state, user.id, password, now)
                .await
                .map_err(cli_error)?;
            ("user.password_set", "set the password of", user)
        }
    };
    let _ = repo::insert_audit(
        &state.db,
        None,
        action,
        "user",
        Some(&user.id.to_string()),
        now,
    )
    .await;
    Ok(format!("{done} {}", user.email))
}

async fn existing(
    state: &state::AppState,
    email: &str,
) -> Result<repo::User, Box<dyn std::error::Error>> {
    Ok(
        repo::user_by_email(&state.db, &owlee_platform::normalize_email(email))
            .await?
            .ok_or_else(|| format!("no account for {email}"))?,
    )
}

fn cli_error(error: AppError) -> Box<dyn std::error::Error> {
    match error {
        AppError::Validation(fields) => fields.into_values().collect::<Vec<_>>().join(" ").into(),
        AppError::Conflict(message) => message.into(),
        other => other.to_string().into(),
    }
}

/// The first line of stdin, or two prompts on a terminal with echo off.
fn read_password(from_stdin: bool) -> Result<String, Box<dyn std::error::Error>> {
    use std::io::{BufRead, IsTerminal, Write};

    let stdin = std::io::stdin();
    if from_stdin {
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        return Ok(line.trim_end_matches(['\n', '\r']).to_owned());
    }
    if !stdin.is_terminal() {
        return Err("no terminal to ask for the password; pass --password-stdin".into());
    }
    let ask = |prompt: &str| -> Result<String, Box<dyn std::error::Error>> {
        eprint!("{prompt}");
        std::io::stderr().flush()?;
        set_echo(false);
        let mut line = String::new();
        let read = stdin.lock().read_line(&mut line);
        set_echo(true);
        eprintln!();
        read?;
        Ok(line.trim_end_matches(['\n', '\r']).to_owned())
    };
    let password = ask("Password: ")?;
    if ask("Repeat password: ")? != password {
        return Err("the passwords do not match".into());
    }
    Ok(password)
}

fn set_echo(on: bool) {
    #[cfg(unix)]
    let _ = std::process::Command::new("stty")
        .arg(if on { "echo" } else { "-echo" })
        .stdin(std::process::Stdio::inherit())
        .status();
    #[cfg(not(unix))]
    let _ = on;
}

/// SQLite is owned by one Owlee process. Postgres stays unlocked so `setup-token`
/// can run while the server is up, matching Orbit.
fn sqlite_owner(config: &Config) -> Result<Option<ownership::Guard>, BootError> {
    if config.database_url.is_some() {
        return Ok(None);
    }
    ownership::lock_sqlite(&config.database_path)
        .map(Some)
        .map_err(BootError::Config)
}

fn load(listen: Option<SocketAddr>, database: Option<PathBuf>) -> Result<Config, BootError> {
    let mut config = Config::from_env().map_err(|error| BootError::Config(error.to_string()))?;
    if let Some(listen) = listen {
        config.apply_listen(listen);
    }
    if let Some(database) = database {
        config.database_path = database;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_setup_token_rotate() {
        let cli = Cli::try_parse_from([
            "owlee",
            "setup-token",
            "rotate",
            "--origin",
            "https://status.example.com",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::SetupToken {
                command: SetupTokenCommand::Rotate { .. },
                ..
            })
        ));
    }

    #[test]
    fn parses_user_set_password() {
        let cli = Cli::try_parse_from([
            "owlee",
            "user",
            "set-password",
            "--email",
            "a@example.com",
            "--password-stdin",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::User {
                command: UserCommand::SetPassword {
                    password_stdin: true,
                    ..
                },
                ..
            })
        ));
    }
}

/// Ctrl+C, or SIGTERM from `docker stop` and Coolify.
async fn shutdown_signal() {
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
}
