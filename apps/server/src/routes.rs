use std::collections::BTreeMap;

use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::header::{AUTHORIZATION, ORIGIN};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use owlee_domain::{
    Assertion, CheckType, HeaderPair, IncidentImpact, IncidentStatus, JobType, MonitorInput,
    MonitorState, NotificationEvent, NotificationPayload, OpenIncident, page_banner,
    resolved_at_after_update, validate_monitor, validate_slug,
};
use owlee_platform::{
    Id, PasswordError, TimestampMillis, generate_token, hash_token, normalize_email,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::engine;
use crate::error::AppError;
use crate::instance_settings::{self, SmtpInput};
use crate::notify;
use crate::peer::Client;
use crate::present::{self, Stats};
use crate::repo::{
    self, Agent, Channel, Incident, IncidentUpdate, Maintenance, Monitor, Page, User,
};
use crate::seed::{DEMO_EMAIL, DEMO_PASSWORD};
use crate::setup::{self, NewOwner};
use crate::state::{self, AppState};
use crate::workspaces::{self, Member, ProjectFilter, audit, require_project};

const DAY_MS: i64 = 86_400_000;
// Above the 64 KiB monitor body that validation allows, after JSON escaping.
const MAX_JSON_BODY_BYTES: usize = 512 * 1024;
/// 100 checks with the longest names and targets still fit in the body limit.
const MAX_AGENT_CHECKS: usize = 100;

pub fn router(state: AppState) -> Router {
    let cookie_api = Router::new()
        .route("/health", get(health))
        .route("/health/ready", get(ready))
        .route("/meta", get(meta))
        .route("/auth/register", post(register))
        .route("/auth/register/complete", post(register_complete))
        .route("/auth/recovery", post(recovery))
        .route("/auth/recovery/complete", post(recovery_complete))
        .route("/auth/login", post(login))
        .route("/setup/status", get(setup_status))
        .route("/setup/complete", post(setup_complete))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
        .route("/auth/password", post(change_password))
        .route("/auth/sessions", get(list_sessions))
        .route("/auth/sessions/{id}", delete(revoke_session))
        .route("/admin/settings", get(admin_settings))
        .route("/admin/settings/registration", put(set_registration))
        .route("/admin/settings/quotas", put(set_quotas))
        .route("/admin/settings/smtp", put(save_smtp).delete(delete_smtp))
        .merge(crate::accounts::routes())
        .merge(workspaces::routes())
        .merge(crate::invitations::routes())
        .route("/overview", get(overview))
        .route("/dashboard", get(crate::dashboard::show))
        .route("/monitors", get(list_monitors).post(create_monitor))
        .route(
            "/monitors/{id}",
            get(get_monitor).put(update_monitor).delete(delete_monitor),
        )
        .route("/monitors/test", post(test_monitor))
        .route("/monitors/{id}/checks", post(run_check))
        .route("/monitors/{id}/active", post(set_active))
        .route("/agents", get(list_agents).post(create_agent))
        .route("/agents/{id}", put(rename_agent).delete(delete_agent))
        .route("/agents/{id}/token", post(rotate_agent_token))
        .route("/pages", get(list_pages).post(create_page))
        .route(
            "/pages/{id}",
            get(get_page).put(update_page).delete(delete_page),
        )
        .route("/incidents", get(list_incidents).post(create_incident))
        .route(
            "/incidents/{id}",
            get(get_incident).put(edit_incident).delete(delete_incident),
        )
        .route("/incidents/{id}/updates", post(add_update))
        .route("/incidents/{id}/updates/{update_id}", put(edit_update))
        .route(
            "/maintenances",
            get(list_maintenances).post(create_maintenance),
        )
        .route(
            "/maintenances/{id}",
            put(update_maintenance).delete(delete_maintenance),
        )
        .route("/notifications", get(list_channels).post(create_channel))
        .route(
            "/notifications/{id}",
            put(update_channel).delete(delete_channel),
        )
        .route("/notifications/{id}/test", post(test_channel))
        .route("/public/{slug}", get(public_page))
        .merge(crate::insights::api_routes())
        .merge(crate::errors::api_routes())
        .route_layer(middleware::from_fn_with_state(state.clone(), same_origin));
    // Bearer-token agents and public ingest are called from other origins on purpose.
    // The ingest routes below read raw bodies with their own caps.
    let api = cookie_api
        .route("/agent/checks", post(agent_checks))
        .route("/agent/heartbeat", post(agent_heartbeat))
        .route("/agent/results", post(agent_result))
        .layer(DefaultBodyLimit::max(MAX_JSON_BODY_BYTES))
        .merge(crate::insights::ingest_routes())
        .merge(crate::errors::ingest_routes())
        .fallback(api_missing);
    Router::new()
        .nest("/api", api)
        .merge(crate::insights::public_routes())
        .fallback(crate::insights::fallback)
        // No route streams, so a request that runs longer than this is stuck.
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            std::time::Duration::from_secs(60),
        ))
        .layer(middleware::from_fn(crate::security_headers::add))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Cross-site request forgery guard for the cookie routes. `SameSite=Lax`
/// alone still lets a sibling subdomain post with the cookie.
async fn same_origin(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if !matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        return next.run(request).await;
    }
    let headers = request.headers();
    let expected = state.config.public_origin.trim_end_matches('/');
    let allowed = match headers.get(ORIGIN) {
        Some(origin) => origin.as_bytes() == expected.as_bytes(),
        None => headers
            .get("sec-fetch-site")
            .is_none_or(|site| site.as_bytes() != b"cross-site"),
    };
    if !allowed {
        return AppError::CrossOrigin {
            expected: expected.to_owned(),
            received: headers
                .get(ORIGIN)
                .map(|origin| String::from_utf8_lossy(origin.as_bytes()).into_owned()),
        }
        .into_response();
    }
    next.run(request).await
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn ready(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    sqlx::query("SELECT 1").execute(state.db.pool()).await?;
    Ok(Json(
        json!({ "status": "ok", "database": state.config.backend_name() }),
    ))
}

async fn meta(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let setup_complete = setup::is_complete(&state.db).await?;
    let settings = instance_settings::load(&state.db, &state.config.app_key).await?;
    let demo = if state.config.environment.as_str() == "development" && setup_complete {
        json!({ "email": DEMO_EMAIL, "password": DEMO_PASSWORD })
    } else {
        Value::Null
    };
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "signup_open": settings.signup_open(state.config.smtp.is_some(), setup_complete),
        "setup_complete": setup_complete,
        "database": state.config.backend_name(),
        "public_origin": state.config.public_origin,
        "demo": demo,
    })))
}

async fn api_missing() -> AppError {
    AppError::NotFound
}

#[derive(Deserialize)]
struct LoginBody {
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct EmailBody {
    email: String,
}

const REGISTER_LINK_MS: i64 = DAY_MS;
const RECOVERY_LINK_MS: i64 = 3_600_000;

/// Always 202 while registration is open, so the answer does not tell whether
/// the address has an account. Both cases send one mail.
async fn register(
    State(state): State<AppState>,
    client: Client,
    Json(body): Json<EmailBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    limit_auth_ip(&state, &client.limit_key)?;
    let normalized = request_email(&body.email)?;
    ensure_signup_open(&state).await?;
    if mail_allowed(&state, &normalized) {
        let origin = &state.config.public_origin;
        let (subject, text) = if repo::user_by_email(&state.db, &normalized).await?.is_some() {
            (
                "You already have an Owlee account",
                format!(
                    "Someone asked to create an Owlee account with this email address, but you already have one.\n\n\
                     Sign in: {origin}/login\n\
                     Forgot your password? Reset it: {origin}/forgot-password\n\n\
                     If this was not you, ignore this email."
                ),
            )
        } else {
            let token = generate_token();
            let now = TimestampMillis::now();
            repo::insert_email_token(
                &state.db,
                &hash_token(&token),
                repo::Purpose::Register,
                &normalized,
                None,
                now,
                TimestampMillis::from_millis(now.as_millis() + REGISTER_LINK_MS),
            )
            .await?;
            (
                "Finish creating your Owlee account",
                format!(
                    "Open this link to finish creating your Owlee account:\n\n\
                     {origin}/register/complete?token={token}\n\n\
                     The link works once, for 24 hours. If you did not ask for it, ignore this email."
                ),
            )
        };
        notify::send_account_mail(&state, &normalized, subject, text).await;
    }
    Ok((StatusCode::ACCEPTED, Json(json!({}))))
}

#[derive(Deserialize)]
struct RegisterCompleteBody {
    token: String,
    password: String,
    display_name: Option<String>,
}

async fn register_complete(
    State(state): State<AppState>,
    client: Client,
    jar: CookieJar,
    Json(body): Json<RegisterCompleteBody>,
) -> Result<(StatusCode, CookieJar, Json<present::UserResponse>), AppError> {
    limit_auth_ip(&state, &client.limit_key)?;
    state
        .passwords
        .validate(&body.password)
        .map_err(password_field)?;
    let display_name = display_name_field(body.display_name)?;
    ensure_signup_open(&state).await?;
    let token_hash = hash_token(&body.token);
    let now = TimestampMillis::now();
    // A wrong token must not buy a hash. The token is used up only after the
    // hash, so a hash that fails leaves the link working.
    if !repo::email_token_live(&state.db, &token_hash, repo::Purpose::Register, now).await? {
        return Err(invalid_link());
    }
    let hash = state::hash_password(&state, body.password)
        .await
        .map_err(password_field)?;
    let Some(token) =
        repo::consume_email_token(&state.db, &token_hash, repo::Purpose::Register, now).await?
    else {
        return Err(invalid_link());
    };
    let account = account_fields(&token.email, display_name)?;
    if repo::user_by_email(&state.db, &account.normalized)
        .await?
        .is_some()
    {
        return Err(invalid_link());
    }
    let id = Id::new();
    repo::insert_user(
        &state.db,
        id,
        &account.email,
        &account.normalized,
        &account.display_name,
        &hash,
        now,
    )
    .await?;
    let user = repo::user_by_id(&state.db, id)
        .await?
        .ok_or(AppError::Internal)?;
    let jar = issue_session(&state, jar, &user, now).await?;
    let _ = repo::insert_audit(
        &state.db,
        Some(user.id),
        "auth.register",
        "user",
        Some(&user.id.to_string()),
        now,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        jar,
        Json(user_response(&state, &user).await?),
    ))
}

/// Always 202, with or without an account for the address.
async fn recovery(
    State(state): State<AppState>,
    client: Client,
    Json(body): Json<EmailBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    limit_auth_ip(&state, &client.limit_key)?;
    let normalized = request_email(&body.email)?;
    if mail_allowed(&state, &normalized)
        && let Some(user) = repo::user_by_email(&state.db, &normalized).await?
        && !user.disabled
    {
        let token = generate_token();
        let now = TimestampMillis::now();
        repo::insert_email_token(
            &state.db,
            &hash_token(&token),
            repo::Purpose::Recovery,
            &normalized,
            Some(user.id),
            now,
            TimestampMillis::from_millis(now.as_millis() + RECOVERY_LINK_MS),
        )
        .await?;
        let origin = &state.config.public_origin;
        let text = format!(
            "Open this link to choose a new Owlee password:\n\n\
             {origin}/reset-password?token={token}\n\n\
             The link works once, for 1 hour. If you did not ask for it, ignore this email; your password stays the same."
        );
        notify::send_account_mail(&state, &normalized, "Reset your Owlee password", text).await;
    }
    Ok((StatusCode::ACCEPTED, Json(json!({}))))
}

#[derive(Deserialize)]
struct RecoveryCompleteBody {
    token: String,
    password: String,
}

async fn recovery_complete(
    State(state): State<AppState>,
    client: Client,
    jar: CookieJar,
    Json(body): Json<RecoveryCompleteBody>,
) -> Result<(CookieJar, Json<present::UserResponse>), AppError> {
    limit_auth_ip(&state, &client.limit_key)?;
    state
        .passwords
        .validate(&body.password)
        .map_err(password_field)?;
    let token_hash = hash_token(&body.token);
    let now = TimestampMillis::now();
    // Same order as `register_complete`: look, hash, then use the token up.
    if !repo::email_token_live(&state.db, &token_hash, repo::Purpose::Recovery, now).await? {
        return Err(invalid_link());
    }
    let hash = state::hash_password(&state, body.password)
        .await
        .map_err(password_field)?;
    let token =
        repo::consume_email_token(&state.db, &token_hash, repo::Purpose::Recovery, now).await?;
    let Some(user_id) = token.and_then(|token| token.user_id) else {
        return Err(invalid_link());
    };
    let Some(user) = repo::user_by_id(&state.db, user_id)
        .await?
        .filter(|user| !user.disabled)
    else {
        return Err(invalid_link());
    };
    repo::revoke_user_sessions(&state.db, user.id, None, now).await?;
    repo::set_password(&state.db, user.id, &hash, now).await?;
    state.login_account_limit.forget(&login_account_key(
        &normalize_email(&user.email),
        &client.limit_key,
    ));
    let jar = issue_session(&state, jar, &user, now).await?;
    let _ = repo::insert_audit(
        &state.db,
        Some(user.id),
        "auth.password_reset",
        "user",
        Some(&user.id.to_string()),
        now,
    )
    .await;
    Ok((jar, Json(user_response(&state, &user).await?)))
}

#[derive(Deserialize)]
struct PasswordBody {
    current_password: String,
    new_password: String,
}

async fn change_password(
    State(state): State<AppState>,
    client: Client,
    jar: CookieJar,
    Json(body): Json<PasswordBody>,
) -> Result<StatusCode, AppError> {
    let user = require_user(&state, &jar).await?;
    let account_key = login_account_key(&normalize_email(&user.email), &client.limit_key);
    let account_limit = state.login_account_limit.check(&account_key);
    if !account_limit.allowed {
        return Err(AppError::limited(account_limit.retry_after_secs));
    }
    let valid = state::verify_password(&state, body.current_password, user.password_hash.clone())
        .await
        .unwrap_or(false);
    if !valid {
        return Err(AppError::field(
            "current_password",
            "The current password is wrong.",
        ));
    }
    state.login_account_limit.forget(&account_key);
    state
        .passwords
        .validate(&body.new_password)
        .map_err(|error| AppError::field("new_password", error.to_string()))?;
    let hash = state::hash_password(&state, body.new_password)
        .await
        .map_err(|error| AppError::field("new_password", error.to_string()))?;
    let now = TimestampMillis::now();
    repo::set_password(&state.db, user.id, &hash, now).await?;
    let current = session_token_hash(&state, &jar);
    repo::revoke_user_sessions(&state.db, user.id, current.as_deref(), now).await?;
    let _ = repo::insert_audit(
        &state.db,
        Some(user.id),
        "auth.password_changed",
        "user",
        Some(&user.id.to_string()),
        now,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_sessions(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Json<Value>, AppError> {
    let user = require_user(&state, &jar).await?;
    let current = session_token_hash(&state, &jar);
    let sessions = repo::list_sessions(&state.db, user.id, TimestampMillis::now()).await?;
    Ok(Json(json!(
        sessions
            .iter()
            .map(|session| json!({
                "id": session.id,
                "created_at": session.created_at,
                "last_activity_at": session.last_activity_at,
                "current": current.as_deref() == Some(session.token_hash.as_str()),
            }))
            .collect::<Vec<_>>()
    )))
}

/// Only the caller's own live sessions are found, so another user's id is a 404.
async fn revoke_session(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<Id>,
) -> Result<(CookieJar, StatusCode), AppError> {
    let user = require_user(&state, &jar).await?;
    let now = TimestampMillis::now();
    let session = repo::list_sessions(&state.db, user.id, now)
        .await?
        .into_iter()
        .find(|session| session.id == id)
        .ok_or(AppError::NotFound)?;
    repo::revoke_token(&state.db, &session.token_hash, now).await?;
    let jar = if session_token_hash(&state, &jar).as_deref() == Some(session.token_hash.as_str()) {
        jar.remove(expired_cookie(&state))
    } else {
        jar
    };
    Ok((jar, StatusCode::NO_CONTENT))
}

pub(crate) fn request_email(email: &str) -> Result<String, AppError> {
    let normalized = normalize_email(email);
    if valid_email(&normalized) {
        Ok(normalized)
    } else {
        Err(AppError::field("email", "Enter a valid email address."))
    }
}

/// The first account is created only with the setup token. Later accounts need
/// the root user to open registration after a mail server is saved.
async fn ensure_signup_open(state: &AppState) -> Result<(), AppError> {
    let settings = instance_settings::load(&state.db, &state.config.app_key).await?;
    let setup_complete = setup::is_complete(&state.db).await?;
    if settings.signup_open(state.config.smtp.is_some(), setup_complete) {
        Ok(())
    } else {
        Err(AppError::RegistrationClosed)
    }
}

/// A few account mails per address an hour. Over that, the route still
/// answers 202 and sends nothing.
pub(crate) fn mail_allowed(state: &AppState, normalized_email: &str) -> bool {
    state
        .mail_address_limit
        .check(&hash_token(normalized_email))
        .allowed
}

fn invalid_link() -> AppError {
    AppError::field(
        "token",
        "This link is invalid or has expired. Ask for a new one.",
    )
}

/// One client address guessing at one account. Another address has its own
/// bucket, so a stranger's wrong guesses do not lock the owner out.
fn login_account_key(normalized_email: &str, client_key: &str) -> String {
    format!(
        "login-account:{}:{client_key}",
        hash_token(normalized_email)
    )
}

async fn setup_status(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    Ok(Json(json!({
        "complete": setup::is_complete(&state.db).await?,
    })))
}

#[derive(Deserialize)]
struct SetupBody {
    token: String,
    email: String,
    password: String,
    display_name: Option<String>,
}

async fn setup_complete(
    State(state): State<AppState>,
    client: Client,
    jar: CookieJar,
    Json(body): Json<SetupBody>,
) -> Result<(StatusCode, CookieJar, Json<present::UserResponse>), AppError> {
    limit_auth_ip(&state, &client.limit_key)?;
    let now = TimestampMillis::now();
    // A finished install refuses setup even when the body still carries the old token.
    // A wrong or expired token, while setup is still open, is unauthorized.
    if setup::is_complete(&state.db).await? {
        return Err(AppError::SetupUnavailable);
    }
    if !setup::setup_token_valid(&state.db, &body.token, now).await? {
        return Err(AppError::InvalidSetupToken);
    }
    let account = account_fields(&body.email, body.display_name)?;
    if let Err(error) = state.passwords.validate(&body.password) {
        return Err(password_field(error));
    }
    let password_hash = state::hash_password(&state, body.password)
        .await
        .map_err(password_field)?;
    let completed = setup::complete_setup(
        &state.db,
        NewOwner {
            token: body.token,
            email: account.email,
            normalized_email: account.normalized,
            display_name: account.display_name,
            password_hash,
        },
        TimestampMillis::now(),
    )
    .await
    .map_err(|error| match error {
        setup::SetupError::AlreadyComplete => AppError::SetupUnavailable,
        setup::SetupError::InvalidToken => AppError::InvalidSetupToken,
        setup::SetupError::EmailTaken => {
            AppError::Conflict("An account with that email already exists.".into())
        }
        setup::SetupError::Database(error) => AppError::from(error),
    })?;
    let jar = session_jar(&state, jar, completed.session_token);
    Ok((
        StatusCode::CREATED,
        jar,
        Json(user_response(&state, &completed.user).await?),
    ))
}

async fn login(
    State(state): State<AppState>,
    client: Client,
    jar: CookieJar,
    Json(body): Json<LoginBody>,
) -> Result<(CookieJar, Json<present::UserResponse>), AppError> {
    let normalized = normalize_email(&body.email);
    limit_auth_ip(&state, &client.limit_key)?;
    // Each attempt takes its tokens before the hash, so attempts that run at
    // the same time cannot all pass. A right password gives them back.
    let account_key = login_account_key(&normalized, &client.limit_key);
    let wide_key = format!("login-account-wide:{}", hash_token(&normalized));
    for (limit, key) in [
        (&state.login_account_limit, &account_key),
        (&state.login_account_wide_limit, &wide_key),
    ] {
        let decision = limit.check(key);
        if !decision.allowed {
            if key == &wide_key {
                state.login_account_limit.refund(&account_key);
            }
            return Err(AppError::limited(decision.retry_after_secs));
        }
    }
    let user = repo::user_by_email(&state.db, &normalized).await?;
    // Unknown emails still pay for one hash, so the response time does not
    // tell which emails have an account.
    let encoded = match &user {
        Some(user) => user.password_hash.clone(),
        None => dummy_hash(&state).await,
    };
    let valid = state::verify_password(&state, body.password, encoded)
        .await
        .unwrap_or(false);
    let Some(user) = user.filter(|_| valid) else {
        return Err(AppError::InvalidLogin);
    };
    if user.disabled {
        return Err(AppError::Forbidden(
            "This account is disabled. Ask an admin to enable it.",
        ));
    }
    state.login_account_limit.forget(&account_key);
    state.login_account_wide_limit.refund(&wide_key);
    let now = TimestampMillis::now();
    let jar = issue_session(&state, jar, &user, now).await?;
    let _ = repo::insert_audit(
        &state.db,
        Some(user.id),
        "auth.login",
        "user",
        Some(&user.id.to_string()),
        now,
    )
    .await;
    Ok((jar, Json(user_response(&state, &user).await?)))
}

/// Login, register, invite sign-up, recovery, and setup share one bucket per client IP.
pub(crate) fn limit_auth_ip(state: &AppState, ip: &str) -> Result<(), AppError> {
    limit(&state.login_ip_limit, &format!("auth-ip:{ip}"))
}

fn limit(bucket: &crate::limit::TokenBucket, key: &str) -> Result<(), AppError> {
    let decision = bucket.check(key);
    if decision.allowed {
        Ok(())
    } else {
        Err(AppError::limited(decision.retry_after_secs))
    }
}

/// A real hash of a random password, made once, for logins with an unknown email.
async fn dummy_hash(state: &AppState) -> String {
    static DUMMY: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
    DUMMY
        .get_or_init(|| async {
            state::hash_password(state, owlee_platform::generate_token())
                .await
                .unwrap_or_default()
        })
        .await
        .clone()
}

async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, StatusCode), AppError> {
    if let Some(token_hash) = session_token_hash(&state, &jar) {
        repo::revoke_token(&state.db, &token_hash, TimestampMillis::now()).await?;
    }
    Ok((jar.remove(expired_cookie(&state)), StatusCode::NO_CONTENT))
}

async fn me(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Json<present::UserResponse>, AppError> {
    let user = require_user(&state, &jar).await?;
    Ok(Json(user_response(&state, &user).await?))
}

async fn admin_settings(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Json<Value>, AppError> {
    require_root(&state, &jar).await?;
    Ok(Json(settings_body(&state).await?))
}

#[derive(Deserialize)]
struct RegistrationBody {
    open: bool,
}

async fn set_registration(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<RegistrationBody>,
) -> Result<Json<Value>, AppError> {
    let actor = require_root(&state, &jar).await?;
    let now = TimestampMillis::now();
    instance_settings::set_registration_open(
        &state.db,
        body.open,
        state.config.smtp.is_some(),
        now,
    )
    .await
    .map_err(|error| match error {
        instance_settings::InstanceError::EmailNotConfigured => AppError::EmailNotConfigured,
        instance_settings::InstanceError::Database(error) => AppError::from(error),
    })?;
    let _ = repo::insert_audit(
        &state.db,
        Some(actor.id),
        if body.open {
            "instance.registration_opened"
        } else {
            "instance.registration_closed"
        },
        "installation",
        None,
        now,
    )
    .await;
    Ok(Json(settings_body(&state).await?))
}

#[derive(Deserialize)]
struct SmtpBody {
    host: String,
    port: Option<i64>,
    username: Option<String>,
    password: Option<String>,
    from: String,
}

async fn save_smtp(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<SmtpBody>,
) -> Result<Json<Value>, AppError> {
    let actor = require_root(&state, &jar).await?;
    let input = smtp_input(&body, &state.config.app_key)?;
    let now = TimestampMillis::now();
    instance_settings::save_smtp(&state.db, input, now).await?;
    let _ = repo::insert_audit(
        &state.db,
        Some(actor.id),
        "instance.smtp_updated",
        "installation",
        None,
        now,
    )
    .await;
    Ok(Json(settings_body(&state).await?))
}

async fn delete_smtp(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Json<Value>, AppError> {
    let actor = require_root(&state, &jar).await?;
    let now = TimestampMillis::now();
    instance_settings::clear_smtp(&state.db, now).await?;
    let _ = repo::insert_audit(
        &state.db,
        Some(actor.id),
        "instance.smtp_removed",
        "installation",
        None,
        now,
    )
    .await;
    Ok(Json(settings_body(&state).await?))
}

async fn settings_body(state: &AppState) -> Result<Value, AppError> {
    let settings = instance_settings::load(&state.db, &state.config.app_key).await?;
    let env_smtp = state.config.smtp.is_some();
    Ok(json!({
        "registration_open": settings.registration_open,
        "smtp_ready": settings.smtp_ready(env_smtp),
        "smtp": smtp_body(&settings, state.config.smtp.as_ref()),
        "root_user": settings.root_user.as_ref().map(|user| json!({
            "id": user.id,
            "email": user.email,
            "display_name": user.display_name,
        })),
        "initialized": settings.initialized,
        "version": env!("CARGO_PKG_VERSION"),
        "database": state.config.backend_name(),
        "error_daily_quota": settings.error_daily_quota,
        "analytics_daily_quota": settings.analytics_daily_quota,
    }))
}

#[derive(Deserialize)]
struct QuotaBody {
    error_daily: i64,
    analytics_daily: i64,
}

async fn set_quotas(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<QuotaBody>,
) -> Result<Json<Value>, AppError> {
    let actor = require_root(&state, &jar).await?;
    if !(1..=100_000_000).contains(&body.error_daily) {
        return Err(AppError::field(
            "error_daily",
            "Use a daily error cap from 1 to 100000000.",
        ));
    }
    if !(1..=100_000_000).contains(&body.analytics_daily) {
        return Err(AppError::field(
            "analytics_daily",
            "Use a daily analytics cap from 1 to 100000000.",
        ));
    }
    let now = TimestampMillis::now();
    instance_settings::set_quotas(&state.db, body.error_daily, body.analytics_daily, now).await?;
    let _ = repo::insert_audit(
        &state.db,
        Some(actor.id),
        "instance.quotas_updated",
        "installation",
        None,
        now,
    )
    .await;
    Ok(Json(settings_body(&state).await?))
}

fn smtp_body(
    settings: &instance_settings::InstanceView,
    env: Option<&owlee_platform::SmtpConfig>,
) -> Value {
    if let Some(smtp) = &settings.smtp {
        return json!({
            "host": smtp.host,
            "port": smtp.port,
            "username": smtp.username,
            "from": smtp.from,
            "has_password": smtp.password.is_some(),
            "source": "saved",
        });
    }
    env.map(|smtp| {
        json!({
            "host": smtp.host,
            "port": smtp.port,
            "username": smtp.username,
            "from": smtp.from,
            "has_password": smtp.password.is_some(),
            "source": "environment",
        })
    })
    .unwrap_or(Value::Null)
}

fn smtp_input(body: &SmtpBody, key: &owlee_platform::AppKey) -> Result<SmtpInput, AppError> {
    let host = body.host.trim();
    if host.is_empty()
        || host.len() > 253
        || host.contains(char::is_whitespace)
        || host.contains("://")
    {
        return Err(AppError::field("host", "Enter the mail server hostname."));
    }
    let port = match body.port {
        None => 587,
        Some(port) if (1..=65535).contains(&port) => u16::try_from(port).unwrap_or(587),
        Some(_) => return Err(AppError::field("port", "Enter a port from 1 to 65535.")),
    };
    let from = body.from.trim();
    if !valid_email(from) {
        return Err(AppError::field("from", "Enter a valid From address."));
    }
    let username = body
        .username
        .as_deref()
        .map(str::trim)
        .filter(|username| !username.is_empty())
        .map(str::to_owned);
    if username
        .as_ref()
        .is_some_and(|username| username.len() > 200)
    {
        return Err(AppError::field("username", "Use at most 200 characters."));
    }
    let password = body
        .password
        .as_deref()
        .map(str::trim)
        .filter(|password| !password.is_empty())
        .map(str::to_owned);
    if password
        .as_ref()
        .is_some_and(|password| password.len() > 500)
    {
        return Err(AppError::field("password", "Use at most 500 characters."));
    }
    Ok(SmtpInput {
        host: host.to_owned(),
        port,
        username,
        sealed_password: password.map(|password| {
            crate::secret_box::seal(key, instance_settings::SMTP_PASSWORD_AAD, &password)
        }),
        from: from.to_owned(),
    })
}

async fn overview(
    State(state): State<AppState>,
    member: Member,
    Query(filter): Query<ProjectFilter>,
) -> Result<Json<Value>, AppError> {
    let project = filter.resolve(&state.db, member.workspace_id).await?;
    let monitors = repo::list_monitors(&state.db, member.workspace_id, project).await?;
    let stats = load_stats(&state, &monitor_ids(&monitors)).await?;
    let monitors = monitors
        .iter()
        .map(|monitor| {
            monitor_view(
                member.is_editor(),
                present::monitor_response(monitor, stats_view(&stats), 30, Vec::new()),
            )
        })
        .collect::<Vec<_>>();
    let incidents = repo::list_incidents(&state.db, member.workspace_id, project)
        .await?
        .into_iter()
        .filter(|incident| incident.status != IncidentStatus::Resolved)
        .take(8)
        .collect::<Vec<_>>();
    let incidents = incident_views(&state, incidents).await?;
    let activity = repo::recent_audit(&state.db, member.workspace_id, 12).await?;
    Ok(Json(json!({
        "monitors": monitors,
        "incidents": incidents,
        "activity": activity.iter().map(present::AuditResponse::from).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct MonitorBody {
    /// Read on create only. A monitor never moves to another project.
    project_id: Option<Id>,
    name: String,
    description: Option<String>,
    job_type: JobType,
    url: String,
    method: Option<String>,
    headers: Option<Vec<HeaderPair>>,
    body: Option<String>,
    timeout_ms: Option<i64>,
    interval_seconds: Option<i64>,
    degraded_after_ms: Option<i64>,
    expected_status: Option<i64>,
    assertions: Option<Vec<Assertion>>,
    follow_redirects: Option<bool>,
    failure_threshold: Option<i32>,
    active: Option<bool>,
}

async fn list_monitors(
    State(state): State<AppState>,
    member: Member,
    Query(filter): Query<ProjectFilter>,
) -> Result<Json<Vec<present::MonitorResponse>>, AppError> {
    let project = filter.resolve(&state.db, member.workspace_id).await?;
    let monitors = repo::list_monitors(&state.db, member.workspace_id, project).await?;
    let stats = load_stats(&state, &monitor_ids(&monitors)).await?;
    Ok(Json(
        monitors
            .iter()
            .map(|monitor| {
                monitor_view(
                    member.is_editor(),
                    present::monitor_response(monitor, stats_view(&stats), 30, Vec::new()),
                )
            })
            .collect(),
    ))
}

async fn create_monitor(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<MonitorBody>,
) -> Result<(StatusCode, Json<present::MonitorDetail>), AppError> {
    member.require_editor()?;
    let project_id = body_project(&state, &member, body.project_id).await?;
    let input = monitor_input(body);
    validate_monitor(&input).map_err(AppError::Validation)?;
    let now = TimestampMillis::now();
    let monitor = materialize(Id::new(), project_id, &input, now, None);
    repo::insert_monitor(&state.db, &monitor).await?;
    audit(
        &state.db,
        &member,
        "monitor.created",
        "monitor",
        Some(monitor.id),
        now,
    )
    .await;
    if monitor.active {
        first_check(&state, monitor.id).await;
    }
    Ok((
        StatusCode::CREATED,
        Json(detail(&state, &member, monitor.id).await?),
    ))
}

/// Long enough for a healthy target to answer. A slow one keeps running in the
/// background and the response shows the monitor as pending.
const FIRST_CHECK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

async fn first_check(state: &AppState, id: Id) {
    let state = state.clone();
    let check = tokio::spawn(async move {
        match engine::execute(&state, id).await {
            Ok(()) | Err(AppError::Busy | AppError::Offline) => {}
            Err(error) => tracing::warn!(%error, monitor = %id, "first check failed"),
        }
    });
    let _ = tokio::time::timeout(FIRST_CHECK_WAIT, check).await;
}

async fn get_monitor(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<Json<present::MonitorDetail>, AppError> {
    Ok(Json(detail(&state, &member, id).await?))
}

async fn update_monitor(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
    Json(body): Json<MonitorBody>,
) -> Result<Json<present::MonitorDetail>, AppError> {
    member.require_editor()?;
    let existing = workspace_monitor(&state, &member, id).await?;
    reject_agent_monitor(&existing, "Edit this check in the agent config.")?;
    let input = monitor_input(body);
    validate_monitor(&input).map_err(AppError::Validation)?;
    let now = TimestampMillis::now();
    let mut monitor = materialize(id, existing.project_id, &input, now, Some(&existing));
    if monitor.active && !existing.active {
        monitor.next_check_at = now;
    }
    repo::update_monitor(&state.db, &monitor).await?;
    audit(
        &state.db,
        &member,
        "monitor.updated",
        "monitor",
        Some(id),
        now,
    )
    .await;
    Ok(Json(detail(&state, &member, id).await?))
}

async fn delete_monitor(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<StatusCode, AppError> {
    member.require_editor()?;
    if !repo::delete_monitor(&state.db, member.workspace_id, id).await? {
        return Err(AppError::NotFound);
    }
    audit(
        &state.db,
        &member,
        "monitor.deleted",
        "monitor",
        Some(id),
        TimestampMillis::now(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn run_check(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<Json<present::MonitorDetail>, AppError> {
    member.require_editor()?;
    let monitor = workspace_monitor(&state, &member, id).await?;
    reject_agent_monitor(&monitor, "The agent runs this check on its own schedule.")?;
    limit_checks(&state, &member)?;
    engine::execute(&state, id).await?;
    Ok(Json(detail(&state, &member, id).await?))
}

/// Runs a monitor that is not saved yet, with the create body and its checks.
async fn test_monitor(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<MonitorBody>,
) -> Result<Json<engine::Outcome>, AppError> {
    member.require_editor()?;
    let project_id = body_project(&state, &member, body.project_id).await?;
    let input = monitor_input(body);
    validate_monitor(&input).map_err(AppError::Validation)?;
    limit_checks(&state, &member)?;
    let monitor = materialize(Id::new(), project_id, &input, TimestampMillis::now(), None);
    Ok(Json(engine::trial(&state, &monitor).await?))
}

fn limit_checks(state: &AppState, member: &Member) -> Result<(), AppError> {
    let decision = state.check_limit.check(&member.user.id.to_string());
    if decision.allowed {
        Ok(())
    } else {
        Err(AppError::limited(decision.retry_after_secs))
    }
}

async fn list_agents(
    State(state): State<AppState>,
    member: Member,
    Query(filter): Query<ProjectFilter>,
) -> Result<Json<Vec<present::AgentResponse>>, AppError> {
    let project = filter.resolve(&state.db, member.workspace_id).await?;
    let agents = repo::list_agents(&state.db, member.workspace_id, project).await?;
    Ok(Json(
        agents.iter().map(present::AgentResponse::from).collect(),
    ))
}

#[derive(Deserialize)]
struct AgentBody {
    project_id: Option<Id>,
    name: String,
}

async fn create_agent(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<AgentBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    member.require_editor()?;
    let project_id = body_project(&state, &member, body.project_id).await?;
    let name = agent_name(&state, project_id, &body.name, None).await?;
    let id = Id::new();
    let token = generate_token();
    let now = TimestampMillis::now();
    repo::insert_agent(&state.db, id, project_id, &name, &hash_token(&token), now).await?;
    audit(&state.db, &member, "agent.created", "agent", Some(id), now).await;
    let agent = repo::workspace_agent(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::Internal)?;
    let mut response = json!(present::AgentResponse::from(&agent));
    response["token"] = json!(token);
    Ok((StatusCode::CREATED, Json(response)))
}

/// A name that is valid and free in the project. `except` is the agent that is renamed.
async fn agent_name(
    state: &AppState,
    project_id: Id,
    raw: &str,
    except: Option<Id>,
) -> Result<String, AppError> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(AppError::field("name", "Use 1 to 60 characters."));
    }
    if repo::agent_name_taken(&state.db, project_id, name, except).await? {
        return Err(AppError::field(
            "name",
            "An agent with this name already exists in this project.",
        ));
    }
    Ok(name.to_owned())
}

#[derive(Deserialize)]
struct AgentRenameBody {
    name: String,
}

/// Renames the agent. Its monitors move with their project, and a monitor never moves to
/// another project, so the agent stays in its project.
async fn rename_agent(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
    Json(body): Json<AgentRenameBody>,
) -> Result<Json<present::AgentResponse>, AppError> {
    member.require_editor()?;
    let agent = repo::workspace_agent(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let name = agent_name(&state, agent.project_id, &body.name, Some(id)).await?;
    repo::rename_agent(&state.db, id, &name).await?;
    audit(
        &state.db,
        &member,
        "agent.renamed",
        "agent",
        Some(id),
        TimestampMillis::now(),
    )
    .await;
    Ok(Json(present::AgentResponse::from(&repo::Agent {
        name,
        ..agent
    })))
}

/// Replaces the agent's token. The old token stops working at once.
async fn rotate_agent_token(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<Json<Value>, AppError> {
    member.require_editor()?;
    let agent = repo::workspace_agent(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let token = generate_token();
    repo::set_agent_token(&state.db, id, &hash_token(&token)).await?;
    audit(
        &state.db,
        &member,
        "agent.token_rotated",
        "agent",
        Some(id),
        TimestampMillis::now(),
    )
    .await;
    let mut response = json!(present::AgentResponse::from(&agent));
    response["token"] = json!(token);
    Ok(Json(response))
}

async fn delete_agent(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<StatusCode, AppError> {
    member.require_editor()?;
    let agent = repo::workspace_agent(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !repo::delete_agent(&state.db, id).await? {
        return Err(AppError::Conflict(format!(
            "{} still has {} monitor(s). Delete them first.",
            agent.name, agent.monitor_count
        )));
    }
    audit(
        &state.db,
        &member,
        "agent.deleted",
        "agent",
        Some(id),
        TimestampMillis::now(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AgentChecksBody {
    checks: Vec<AgentCheckBody>,
}

#[derive(Deserialize)]
struct AgentCheckBody {
    name: String,
    check_type: CheckType,
    target: String,
    interval_seconds: i64,
    timeout_ms: i64,
    failure_threshold: i32,
}

/// The agent sends its config, without secrets, when it starts.
async fn agent_checks(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AgentChecksBody>,
) -> Result<Json<Value>, AppError> {
    let agent = require_agent(&state, &headers).await?;
    limit(&state.agent_checks_limit, &format!("agent:{}", agent.id))?;
    if body.checks.len() > MAX_AGENT_CHECKS {
        return Err(AppError::field(
            "checks",
            format!("Send at most {MAX_AGENT_CHECKS} checks."),
        ));
    }
    let mut checks: Vec<repo::AgentCheck> = Vec::new();
    for check in body.checks {
        let name = check.name.trim().to_owned();
        let input = MonitorInput {
            name: name.clone(),
            description: String::new(),
            job_type: check.check_type.job_type(),
            url: check.target.trim().to_owned(),
            method: "GET".to_owned(),
            headers: Vec::new(),
            body: String::new(),
            timeout_ms: check.timeout_ms,
            interval_seconds: check.interval_seconds,
            degraded_after_ms: None,
            expected_status: None,
            assertions: Vec::new(),
            follow_redirects: true,
            failure_threshold: check.failure_threshold,
            active: true,
        };
        if let Err(fields) = validate_monitor(&input) {
            let detail = fields
                .iter()
                .map(|(field, message)| format!("{field}: {message}"))
                .collect::<Vec<_>>()
                .join(" ");
            return Err(AppError::field(
                "checks",
                format!("Check \"{name}\": {detail}"),
            ));
        }
        if checks.iter().any(|known| known.name == name) {
            return Err(AppError::field(
                "checks",
                format!("Check \"{name}\" is listed twice."),
            ));
        }
        checks.push(repo::AgentCheck {
            name,
            check_type: check.check_type,
            target: input.url,
            interval_seconds: input.interval_seconds,
            timeout_ms: input.timeout_ms,
            failure_threshold: input.failure_threshold,
        });
    }
    let ids = repo::sync_agent_checks(&state.db, &agent, &checks, TimestampMillis::now()).await?;
    Ok(Json(json!({
        "checks": ids
            .iter()
            .map(|(name, id)| json!({ "name": name, "monitor_id": id }))
            .collect::<Vec<_>>(),
    })))
}

async fn agent_heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    require_agent(&state, &headers).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AgentResultBody {
    monitor_id: Id,
    #[serde(flatten)]
    outcome: engine::Outcome,
}

async fn agent_result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AgentResultBody>,
) -> Result<StatusCode, AppError> {
    let agent = require_agent(&state, &headers).await?;
    limit(&state.agent_result_limit, &format!("agent:{}", agent.id))?;
    let monitor = repo::get_monitor(&state.db, body.monitor_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if monitor.agent_id != Some(agent.id) {
        return Err(AppError::Forbidden(
            "This monitor does not belong to this agent.",
        ));
    }
    engine::submit(&state, monitor.id, body.outcome, &agent.name).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ActiveBody {
    active: bool,
}

async fn set_active(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
    Json(body): Json<ActiveBody>,
) -> Result<Json<present::MonitorDetail>, AppError> {
    member.require_editor()?;
    let monitor = workspace_monitor(&state, &member, id).await?;
    let now = TimestampMillis::now();
    let next = if body.active {
        now.as_millis()
    } else {
        monitor.next_check_at.as_millis()
    };
    repo::set_active(&state.db, id, body.active, next, now.as_millis()).await?;
    Ok(Json(detail(&state, &member, id).await?))
}

#[derive(Deserialize)]
struct PageBody {
    /// Read on create only.
    project_id: Option<Id>,
    title: String,
    description: Option<String>,
    slug: String,
    published: Option<bool>,
    homepage_url: Option<String>,
    monitor_ids: Vec<Id>,
}

async fn list_pages(
    State(state): State<AppState>,
    member: Member,
    Query(filter): Query<ProjectFilter>,
) -> Result<Json<Vec<present::PageResponse>>, AppError> {
    let project = filter.resolve(&state.db, member.workspace_id).await?;
    let pages = repo::list_pages(&state.db, member.workspace_id, project).await?;
    let mut response = Vec::new();
    for page in pages {
        let components = repo::components(&state.db, page.id).await?;
        response.push(present::page_response(&page, &components));
    }
    Ok(Json(response))
}

async fn create_page(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<PageBody>,
) -> Result<(StatusCode, Json<present::PageResponse>), AppError> {
    member.require_editor()?;
    let project_id = body_project(&state, &member, body.project_id).await?;
    let (title, description, slug, homepage) = validate_page(&body)?;
    if repo::page_by_slug(&state.db, &slug).await?.is_some() {
        return Err(AppError::Conflict(
            "That status page slug is already in use.".into(),
        ));
    }
    let names = component_names(&state, &member, &body.monitor_ids).await?;
    let now = TimestampMillis::now();
    let page = Page {
        id: Id::new(),
        project_id,
        title,
        description,
        slug,
        published: body.published.unwrap_or(true),
        homepage_url: homepage,
        created_at: now,
        updated_at: now,
    };
    repo::insert_page(&state.db, &page).await?;
    repo::replace_components(&state.db, page.id, &names, now).await?;
    audit(
        &state.db,
        &member,
        "page.created",
        "page",
        Some(page.id),
        now,
    )
    .await;
    let components = repo::components(&state.db, page.id).await?;
    Ok((
        StatusCode::CREATED,
        Json(present::page_response(&page, &components)),
    ))
}

async fn get_page(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<Json<present::PageResponse>, AppError> {
    let page = repo::workspace_page(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let components = repo::components(&state.db, id).await?;
    Ok(Json(present::page_response(&page, &components)))
}

async fn update_page(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
    Json(body): Json<PageBody>,
) -> Result<Json<present::PageResponse>, AppError> {
    member.require_editor()?;
    let existing = repo::workspace_page(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let (title, description, slug, homepage) = validate_page(&body)?;
    if let Some(other) = repo::page_by_slug(&state.db, &slug).await? {
        if other.id != id {
            return Err(AppError::Conflict(
                "That status page slug is already in use.".into(),
            ));
        }
    }
    let names = component_names(&state, &member, &body.monitor_ids).await?;
    let now = TimestampMillis::now();
    let page = Page {
        id,
        project_id: existing.project_id,
        title,
        description,
        slug,
        published: body.published.unwrap_or(existing.published),
        homepage_url: homepage,
        created_at: existing.created_at,
        updated_at: now,
    };
    repo::update_page(&state.db, &page).await?;
    repo::replace_components(&state.db, id, &names, now).await?;
    audit(&state.db, &member, "page.updated", "page", Some(id), now).await;
    let components = repo::components(&state.db, id).await?;
    Ok(Json(present::page_response(&page, &components)))
}

async fn delete_page(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<StatusCode, AppError> {
    member.require_editor()?;
    if !repo::delete_page(&state.db, member.workspace_id, id).await? {
        return Err(AppError::NotFound);
    }
    audit(
        &state.db,
        &member,
        "page.deleted",
        "page",
        Some(id),
        TimestampMillis::now(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct IncidentBody {
    project_id: Option<Id>,
    title: String,
    status: IncidentStatus,
    impact: IncidentImpact,
    message: String,
    page_id: Option<Id>,
    monitor_id: Option<Id>,
}

#[derive(Deserialize)]
struct UpdateBody {
    status: IncidentStatus,
    message: String,
}

#[derive(Deserialize)]
struct IncidentEditBody {
    title: String,
    impact: IncidentImpact,
}

#[derive(Deserialize)]
struct UpdateEditBody {
    message: String,
}

async fn list_incidents(
    State(state): State<AppState>,
    member: Member,
    Query(filter): Query<ProjectFilter>,
) -> Result<Json<Vec<present::IncidentResponse>>, AppError> {
    let project = filter.resolve(&state.db, member.workspace_id).await?;
    let incidents = repo::list_incidents(&state.db, member.workspace_id, project).await?;
    Ok(Json(incident_views(&state, incidents).await?))
}

async fn create_incident(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<IncidentBody>,
) -> Result<(StatusCode, Json<present::IncidentResponse>), AppError> {
    member.require_editor()?;
    let project_id = body_project(&state, &member, body.project_id).await?;
    let (title, message) = validate_incident(&body.title, &body.message)?;
    ensure_links(&state, &member, body.page_id, body.monitor_id).await?;
    let now = TimestampMillis::now();
    let id = Id::new();
    let incident = Incident {
        id,
        project_id,
        page_id: body.page_id,
        monitor_id: body.monitor_id,
        title,
        status: body.status,
        impact: body.impact,
        auto: false,
        started_at: now,
        resolved_at: resolved_at_after_update(None, body.status, now.as_millis())
            .map(TimestampMillis::from_millis),
        created_at: now,
        updated_at: now,
    };
    let update = IncidentUpdate {
        id: Id::new(),
        incident_id: id,
        status: body.status,
        message: message.clone(),
        created_at: now,
    };
    let mut tx = state.db.begin().await?;
    repo::insert_incident(&mut tx, &incident, &update).await?;
    tx.commit().await?;
    audit(
        &state.db,
        &member,
        "incident.created",
        "incident",
        Some(id),
        now,
    )
    .await;
    notify_incident(&state, &incident, &message).await;
    Ok((
        StatusCode::CREATED,
        Json(present::incident_response(&incident, &[update])),
    ))
}

async fn get_incident(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<Json<present::IncidentResponse>, AppError> {
    let incident = repo::workspace_incident(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let updates = repo::updates_for(&state.db, &[id]).await?;
    Ok(Json(present::incident_response(&incident, &updates)))
}

async fn add_update(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
    Json(body): Json<UpdateBody>,
) -> Result<Json<present::IncidentResponse>, AppError> {
    member.require_editor()?;
    let incident = repo::workspace_incident(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let message = validate_update_message(&body.message)?;
    let now = TimestampMillis::now();
    let resolved = resolved_at_after_update(
        incident.resolved_at.map(TimestampMillis::as_millis),
        body.status,
        now.as_millis(),
    );
    let update = IncidentUpdate {
        id: Id::new(),
        incident_id: id,
        status: body.status,
        message: message.clone(),
        created_at: now,
    };
    let mut tx = state.db.begin().await?;
    repo::apply_incident_status(&mut tx, id, body.status, resolved, now.as_millis()).await?;
    repo::insert_update(&mut tx, &update).await?;
    tx.commit().await?;
    audit(
        &state.db,
        &member,
        "incident.updated",
        "incident",
        Some(id),
        now,
    )
    .await;
    let updated = repo::workspace_incident(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::Internal)?;
    notify_incident(&state, &updated, &message).await;
    let updates = repo::updates_for(&state.db, &[id]).await?;
    Ok(Json(present::incident_response(&updated, &updates)))
}

/// Corrects the wording. It sends no alerts; status changes go through `add_update`.
async fn edit_incident(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
    Json(body): Json<IncidentEditBody>,
) -> Result<Json<present::IncidentResponse>, AppError> {
    member.require_editor()?;
    let title = body.title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(AppError::field("title", "Use 1 to 200 characters."));
    }
    let now = TimestampMillis::now();
    if !repo::edit_incident(&state.db, member.workspace_id, id, title, body.impact, now).await? {
        return Err(AppError::NotFound);
    }
    audit(
        &state.db,
        &member,
        "incident.edited",
        "incident",
        Some(id),
        now,
    )
    .await;
    incident_view(&state, &member, id).await
}

/// Corrects one timeline message. Its status and time stay as posted.
async fn edit_update(
    State(state): State<AppState>,
    member: Member,
    Path((id, update_id)): Path<(Id, Id)>,
    Json(body): Json<UpdateEditBody>,
) -> Result<Json<present::IncidentResponse>, AppError> {
    member.require_editor()?;
    let message = validate_update_message(&body.message)?;
    repo::workspace_incident(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let now = TimestampMillis::now();
    if !repo::edit_update(&state.db, id, update_id, &message, now).await? {
        return Err(AppError::NotFound);
    }
    audit(
        &state.db,
        &member,
        "incident.edited",
        "incident",
        Some(id),
        now,
    )
    .await;
    incident_view(&state, &member, id).await
}

async fn incident_view(
    state: &AppState,
    member: &Member,
    id: Id,
) -> Result<Json<present::IncidentResponse>, AppError> {
    let incident = repo::workspace_incident(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let updates = repo::updates_for(&state.db, &[id]).await?;
    Ok(Json(present::incident_response(&incident, &updates)))
}

fn validate_update_message(message: &str) -> Result<String, AppError> {
    let message = message.trim();
    if message.is_empty() || message.chars().count() > 5_000 {
        return Err(AppError::field(
            "message",
            "Write an update between 1 and 5000 characters.",
        ));
    }
    Ok(message.to_owned())
}

async fn delete_incident(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<StatusCode, AppError> {
    member.require_editor()?;
    if !repo::delete_incident(&state.db, member.workspace_id, id).await? {
        return Err(AppError::NotFound);
    }
    audit(
        &state.db,
        &member,
        "incident.deleted",
        "incident",
        Some(id),
        TimestampMillis::now(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MaintenanceBody {
    project_id: Option<Id>,
    title: String,
    message: String,
    page_id: Id,
    monitor_ids: Vec<Id>,
    starts_at: TimestampMillis,
    ends_at: TimestampMillis,
}

async fn list_maintenances(
    State(state): State<AppState>,
    member: Member,
    Query(filter): Query<ProjectFilter>,
) -> Result<Json<Vec<present::MaintenanceResponse>>, AppError> {
    let project = filter.resolve(&state.db, member.workspace_id).await?;
    let now = TimestampMillis::now();
    let rows = repo::list_maintenances(&state.db, member.workspace_id, project).await?;
    Ok(Json(
        rows.iter()
            .map(|row| present::maintenance_response(row, now))
            .collect(),
    ))
}

async fn create_maintenance(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<MaintenanceBody>,
) -> Result<(StatusCode, Json<present::MaintenanceResponse>), AppError> {
    member.require_editor()?;
    let project_id = body_project(&state, &member, body.project_id).await?;
    let (title, message) = validate_maintenance(&body)?;
    ensure_links(&state, &member, Some(body.page_id), None).await?;
    ensure_monitors(&state, &member, &body.monitor_ids).await?;
    let now = TimestampMillis::now();
    let maintenance = Maintenance {
        id: Id::new(),
        project_id,
        page_id: Some(body.page_id),
        title,
        message,
        starts_at: body.starts_at,
        ends_at: body.ends_at,
        monitor_ids: body.monitor_ids,
        created_at: now,
    };
    repo::insert_maintenance(&state.db, &maintenance).await?;
    audit(
        &state.db,
        &member,
        "maintenance.created",
        "maintenance",
        Some(maintenance.id),
        now,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(present::maintenance_response(&maintenance, now)),
    ))
}

/// The project stays fixed after creation, like a status page's.
async fn update_maintenance(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
    Json(body): Json<MaintenanceBody>,
) -> Result<Json<present::MaintenanceResponse>, AppError> {
    member.require_editor()?;
    let existing = repo::workspace_maintenance(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let (title, message) = validate_maintenance(&body)?;
    ensure_links(&state, &member, Some(body.page_id), None).await?;
    ensure_monitors(&state, &member, &body.monitor_ids).await?;
    let now = TimestampMillis::now();
    let maintenance = Maintenance {
        id,
        project_id: existing.project_id,
        page_id: Some(body.page_id),
        title,
        message,
        starts_at: body.starts_at,
        ends_at: body.ends_at,
        monitor_ids: body.monitor_ids,
        created_at: existing.created_at,
    };
    repo::update_maintenance(&state.db, &maintenance, now).await?;
    audit(
        &state.db,
        &member,
        "maintenance.updated",
        "maintenance",
        Some(id),
        now,
    )
    .await;
    Ok(Json(present::maintenance_response(&maintenance, now)))
}

fn validate_maintenance(body: &MaintenanceBody) -> Result<(String, String), AppError> {
    let title = body.title.trim().to_owned();
    let message = body.message.trim().to_owned();
    let mut fields = BTreeMap::new();
    if title.is_empty() || title.chars().count() > 160 {
        fields.insert("title".to_owned(), "Use 1 to 160 characters.".to_owned());
    }
    if message.is_empty() || message.chars().count() > 5_000 {
        fields.insert(
            "message".to_owned(),
            "Describe the window in 1 to 5000 characters.".to_owned(),
        );
    }
    if body.ends_at.as_millis() <= body.starts_at.as_millis() {
        fields.insert(
            "ends_at".to_owned(),
            "The window must end after it starts.".to_owned(),
        );
    }
    if !fields.is_empty() {
        return Err(AppError::Validation(fields));
    }
    Ok((title, message))
}

async fn delete_maintenance(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<StatusCode, AppError> {
    member.require_editor()?;
    if !repo::delete_maintenance(&state.db, member.workspace_id, id).await? {
        return Err(AppError::NotFound);
    }
    audit(
        &state.db,
        &member,
        "maintenance.deleted",
        "maintenance",
        Some(id),
        TimestampMillis::now(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ChannelBody {
    name: String,
    provider: String,
    config: Value,
    enabled: Option<bool>,
    monitor_ids: Vec<Id>,
}

/// Editors only. Responses carry masked configs, see `present::channel_response`.
async fn list_channels(
    State(state): State<AppState>,
    member: Member,
) -> Result<Json<Vec<present::ChannelResponse>>, AppError> {
    member.require_editor_to_read()?;
    let channels =
        repo::list_channels(&state.db, &state.config.app_key, member.workspace_id).await?;
    let mut response = Vec::new();
    for channel in channels {
        response.push(channel_view(&state, channel).await?);
    }
    Ok(Json(response))
}

async fn create_channel(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<ChannelBody>,
) -> Result<(StatusCode, Json<present::ChannelResponse>), AppError> {
    member.require_editor()?;
    let (name, provider, config) =
        validate_channel(&body, None, state.config.allow_private_webhooks)?;
    ensure_monitors(&state, &member, &body.monitor_ids).await?;
    let now = TimestampMillis::now();
    let channel = Channel {
        id: Id::new(),
        workspace_id: member.workspace_id,
        name,
        provider,
        config: Some(config),
        enabled: body.enabled.unwrap_or(true),
        created_at: now,
    };
    repo::insert_channel(
        &state.db,
        &state.config.app_key,
        &channel,
        &body.monitor_ids,
    )
    .await?;
    audit(
        &state.db,
        &member,
        "notification.created",
        "notification",
        Some(channel.id),
        now,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(channel_view(&state, channel).await?),
    ))
}

async fn update_channel(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
    Json(body): Json<ChannelBody>,
) -> Result<Json<present::ChannelResponse>, AppError> {
    member.require_editor()?;
    let existing =
        repo::workspace_channel(&state.db, &state.config.app_key, member.workspace_id, id)
            .await?
            .ok_or(AppError::NotFound)?;
    let (name, provider, config) =
        validate_channel(&body, Some(&existing), state.config.allow_private_webhooks)?;
    ensure_monitors(&state, &member, &body.monitor_ids).await?;
    let channel = Channel {
        id,
        workspace_id: existing.workspace_id,
        name,
        provider,
        config: Some(config),
        enabled: body.enabled.unwrap_or(existing.enabled),
        created_at: existing.created_at,
    };
    repo::update_channel(
        &state.db,
        &state.config.app_key,
        &channel,
        &body.monitor_ids,
        TimestampMillis::now(),
    )
    .await?;
    Ok(Json(channel_view(&state, channel).await?))
}

async fn delete_channel(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<StatusCode, AppError> {
    member.require_editor()?;
    if !repo::delete_channel(&state.db, member.workspace_id, id).await? {
        return Err(AppError::NotFound);
    }
    audit(
        &state.db,
        &member,
        "notification.deleted",
        "notification",
        Some(id),
        TimestampMillis::now(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn test_channel(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<Json<present::ChannelResponse>, AppError> {
    member.require_editor()?;
    let channel =
        repo::workspace_channel(&state.db, &state.config.app_key, member.workspace_id, id)
            .await?
            .ok_or(AppError::NotFound)?;
    let payload = NotificationPayload {
        event: NotificationEvent::Test.as_str().to_owned(),
        sent_at: TimestampMillis::now().to_string(),
        monitor: None,
        check: None,
        incident: None,
        message: Some("This is a test notification from Owlee.".to_owned()),
    };
    notify::deliver(&state, &channel, &payload, None, None).await;
    Ok(Json(channel_view(&state, channel).await?))
}

async fn public_page(
    State(state): State<AppState>,
    client: Client,
    Path(slug): Path<String>,
) -> Result<Json<present::PublicPage>, AppError> {
    limit(
        &state.public_page_limit,
        &format!("public-page:{}", client.limit_key),
    )?;
    let page = repo::page_by_slug(&state.db, &slug)
        .await?
        .ok_or(AppError::NotFound)?;
    if !page.published {
        return Err(AppError::NotFound);
    }
    let components = repo::components(&state.db, page.id).await?;
    let component_monitors = components
        .iter()
        .map(|component| component.monitor_id)
        .collect::<Vec<_>>();
    let stats = load_stats(&state, &component_monitors).await?;
    let mut public_components = Vec::new();
    let mut states = Vec::new();
    for component in &components {
        let Some(monitor) = repo::get_monitor(&state.db, component.monitor_id).await? else {
            continue;
        };
        states.push(monitor.state);
        let response = present::monitor_response(&monitor, stats_view(&stats), 90, Vec::new());
        public_components.push(present::PublicComponent {
            id: component.id,
            name: component.name.clone(),
            description: if component.description.is_empty() {
                monitor.description.clone()
            } else {
                component.description.clone()
            },
            state: monitor.state,
            uptime_90d: response.uptime_90d,
            bars: response.bars,
        });
    }
    let incidents = repo::incidents_for_page(&state.db, page.id).await?;
    let incident_ids = incidents
        .iter()
        .map(|incident| incident.id)
        .collect::<Vec<_>>();
    let updates = repo::updates_for(&state.db, &incident_ids).await?;
    let incidents = incidents
        .iter()
        .map(|incident| {
            let component = components
                .iter()
                .find(|component| Some(component.monitor_id) == incident.monitor_id)
                .map(|component| component.name.as_str());
            present::public_incident(incident, &updates, component)
        })
        .collect::<Vec<_>>();
    let open = incidents
        .iter()
        .filter(|incident| incident.status != IncidentStatus::Resolved)
        .map(|incident| OpenIncident {
            impact: incident.impact,
        })
        .collect::<Vec<_>>();
    let banner = page_banner(&states, &open);
    let now = TimestampMillis::now();
    let maintenances = repo::maintenances_for_page(&state.db, page.id)
        .await?
        .iter()
        .map(|maintenance| present::maintenance_response(maintenance, now))
        .filter(|maintenance| maintenance.ends_at.as_millis() >= now.as_millis() - DAY_MS)
        .map(present::PublicMaintenance::from)
        .collect();
    Ok(Json(present::PublicPage {
        title: page.title,
        description: page.description,
        slug: page.slug,
        homepage_url: page.homepage_url,
        banner: present::banner_label(banner),
        components: public_components,
        incidents,
        maintenances,
    }))
}

struct LoadedStats {
    daily: Vec<repo::DailyRow>,
    counts: Vec<repo::StatusCount>,
}

fn stats_view(stats: &LoadedStats) -> Stats<'_> {
    Stats {
        daily: &stats.daily,
        counts: &stats.counts,
    }
}

fn monitor_ids(monitors: &[Monitor]) -> Vec<Id> {
    monitors.iter().map(|monitor| monitor.id).collect()
}

async fn load_stats(state: &AppState, monitor_ids: &[Id]) -> Result<LoadedStats, AppError> {
    let now = TimestampMillis::now();
    Ok(LoadedStats {
        daily: repo::daily_since(&state.db, monitor_ids, now.unix_day() - 89).await?,
        counts: repo::counts_since(&state.db, monitor_ids, now.as_millis() - DAY_MS).await?,
    })
}

async fn detail(
    state: &AppState,
    member: &Member,
    id: Id,
) -> Result<present::MonitorDetail, AppError> {
    let monitor = workspace_monitor(state, member, id).await?;
    let stats = load_stats(state, &[id]).await?;
    let recent = repo::recent_checks(&state.db, id, 60).await?;
    let channels = repo::channels_for_monitor(
        &state.db,
        &state.config.app_key,
        monitor.project_id,
        Some(id),
    )
    .await?;
    let alert_channel_count = channels.len();
    let alert_channels = if member.is_editor() {
        channels
            .into_iter()
            .map(|channel| present::LinkedChannel {
                id: channel.id,
                name: channel.name,
                provider: channel.provider,
            })
            .collect()
    } else {
        Vec::new()
    };
    let status_pages = repo::pages_for_monitor(&state.db, member.workspace_id, id)
        .await?
        .into_iter()
        .map(|page| present::LinkedPage {
            id: page.id,
            title: page.title,
            slug: page.slug,
        })
        .collect();
    Ok(present::MonitorDetail {
        monitor: monitor_view(
            member.is_editor(),
            present::monitor_response(&monitor, stats_view(&stats), 90, recent),
        ),
        open_incident_id: repo::open_incident_for_monitor(&state.db, member.workspace_id, id)
            .await?,
        alert_channels,
        alert_channel_count,
        status_pages,
        last_error: repo::last_error_check(&state.db, id)
            .await?
            .map(present::check_response),
        latency_24h: present::latency_summary(
            repo::answered_latencies(&state.db, id, TimestampMillis::now().as_millis() - DAY_MS)
                .await?,
        ),
        incidents: repo::incidents_for_monitor(&state.db, member.workspace_id, id, 10)
            .await?
            .into_iter()
            .map(present::MonitorIncident::from)
            .collect(),
    })
}

async fn incident_views(
    state: &AppState,
    incidents: Vec<Incident>,
) -> Result<Vec<present::IncidentResponse>, AppError> {
    let ids = incidents
        .iter()
        .map(|incident| incident.id)
        .collect::<Vec<_>>();
    let updates = repo::updates_for(&state.db, &ids).await?;
    Ok(incidents
        .iter()
        .map(|incident| present::incident_response(incident, &updates))
        .collect())
}

async fn notify_incident(state: &AppState, incident: &Incident, message: &str) {
    let monitor = match incident.monitor_id {
        Some(id) => repo::get_monitor(&state.db, id).await.ok().flatten(),
        None => None,
    };
    let payload = NotificationPayload {
        event: NotificationEvent::IncidentUpdated.as_str().to_owned(),
        sent_at: TimestampMillis::now().to_string(),
        monitor: monitor
            .as_ref()
            .map(|monitor| notify::payload_monitor(monitor, monitor.state)),
        check: None,
        incident: Some(owlee_domain::PayloadIncident {
            id: incident.id.to_string(),
            title: incident.title.clone(),
            status: incident.status,
            impact: incident.impact,
        }),
        message: Some(message.to_owned()),
    };
    notify::fanout(
        state,
        &payload,
        incident.project_id,
        incident.monitor_id,
        Some(incident.id),
    )
    .await;
}

fn monitor_input(body: MonitorBody) -> MonitorInput {
    MonitorInput {
        name: body.name,
        description: body.description.unwrap_or_default(),
        job_type: body.job_type,
        url: body.url.trim().to_owned(),
        method: body.method.unwrap_or_else(|| "GET".to_owned()),
        headers: body.headers.unwrap_or_default(),
        body: body.body.unwrap_or_default(),
        timeout_ms: body.timeout_ms.unwrap_or(30_000),
        interval_seconds: body.interval_seconds.unwrap_or(60),
        degraded_after_ms: body.degraded_after_ms,
        expected_status: body.expected_status,
        assertions: body.assertions.unwrap_or_default(),
        follow_redirects: body.follow_redirects.unwrap_or(true),
        failure_threshold: body.failure_threshold.unwrap_or(3),
        active: body.active.unwrap_or(true),
    }
}

fn materialize(
    id: Id,
    project_id: Id,
    input: &MonitorInput,
    now: TimestampMillis,
    previous: Option<&Monitor>,
) -> Monitor {
    Monitor {
        id,
        project_id,
        name: input.name.trim().to_owned(),
        description: input.description.trim().to_owned(),
        job_type: input.job_type,
        url: input.url.clone(),
        method: input.method.clone(),
        headers: input.headers.clone(),
        body: input.body.clone(),
        timeout_ms: input.timeout_ms,
        interval_seconds: input.interval_seconds,
        degraded_after_ms: input.degraded_after_ms,
        expected_status: input.expected_status,
        assertions: input.assertions.clone(),
        follow_redirects: input.follow_redirects,
        failure_threshold: input.failure_threshold,
        active: input.active,
        state: previous
            .map(|monitor| monitor.state)
            .unwrap_or(MonitorState::Pending),
        consecutive_failures: previous
            .map(|monitor| monitor.consecutive_failures)
            .unwrap_or(0),
        last_checked_at: previous.and_then(|monitor| monitor.last_checked_at),
        next_check_at: previous.map(|monitor| monitor.next_check_at).unwrap_or(now),
        created_at: previous.map(|monitor| monitor.created_at).unwrap_or(now),
        updated_at: now,
        state_changed_at: previous
            .map(|monitor| monitor.state_changed_at)
            .unwrap_or(now),
        agent_id: None,
        check_type: None,
        agent_lost_at: None,
    }
}

fn validate_page(body: &PageBody) -> Result<(String, String, String, Option<String>), AppError> {
    let mut fields = BTreeMap::new();
    let title = body.title.trim().to_owned();
    if title.is_empty() || title.chars().count() > 120 {
        fields.insert("title".to_owned(), "Use 1 to 120 characters.".to_owned());
    }
    let description = body.description.clone().unwrap_or_default();
    if description.chars().count() > 2_000 {
        fields.insert(
            "description".to_owned(),
            "Keep the description under 2000 characters.".to_owned(),
        );
    }
    let slug = body.slug.trim().to_lowercase();
    if validate_slug(&slug).is_err() {
        fields.insert(
            "slug".to_owned(),
            "Use 1 to 64 lowercase letters, numbers, and single hyphens.".to_owned(),
        );
    }
    let homepage = body
        .homepage_url
        .clone()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if let Some(url) = &homepage {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            fields.insert(
                "homepage_url".to_owned(),
                "Homepage must be an http(s) URL.".to_owned(),
            );
        }
    }
    if fields.is_empty() {
        Ok((title, description.trim().to_owned(), slug, homepage))
    } else {
        Err(AppError::Validation(fields))
    }
}

fn validate_incident(title: &str, message: &str) -> Result<(String, String), AppError> {
    let mut fields = BTreeMap::new();
    let title = title.trim().to_owned();
    let message = message.trim().to_owned();
    if title.is_empty() || title.chars().count() > 200 {
        fields.insert("title".to_owned(), "Use 1 to 200 characters.".to_owned());
    }
    if message.is_empty() || message.chars().count() > 5_000 {
        fields.insert(
            "message".to_owned(),
            "Write an update between 1 and 5000 characters.".to_owned(),
        );
    }
    if fields.is_empty() {
        Ok((title, message))
    } else {
        Err(AppError::Validation(fields))
    }
}

/// Responses never carry a channel's URL or secret, so on update an omitted or
/// blank `url` or `secret` keeps the saved one while the provider stays the same.
fn validate_channel(
    body: &ChannelBody,
    existing: Option<&Channel>,
    allow_private: bool,
) -> Result<(String, String, Value), AppError> {
    let name = body.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(AppError::field("name", "Use 1 to 80 characters."));
    }
    let provider = body.provider.trim().to_lowercase();
    let saved = existing
        .filter(|channel| channel.provider == provider)
        .and_then(|channel| channel.config.as_ref());
    let field = |key: &str| {
        let given = body
            .config
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if given.is_empty() {
            saved
                .and_then(|config| config.get(key))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        } else {
            given.to_owned()
        }
    };
    let config = match provider.as_str() {
        "email" => {
            let to = body
                .config
                .get("to")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if !valid_email(to) {
                return Err(AppError::field("config", "Enter the recipient email."));
            }
            json!({ "to": to })
        }
        "webhook" => {
            let url = require_http_url(&field("url"), allow_private)?;
            json!({ "url": url, "secret": field("secret") })
        }
        "slack" | "discord" => {
            let url = require_http_url(&field("url"), allow_private)?;
            json!({ "url": url })
        }
        _ => {
            return Err(AppError::field(
                "provider",
                "Use email, webhook, slack, or discord.",
            ));
        }
    };
    Ok((name, provider, config))
}

fn require_http_url(url: &str, allow_private: bool) -> Result<String, AppError> {
    let url = url.to_owned();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(AppError::field("config", "Enter an http(s) URL."));
    }
    if !allow_private {
        crate::outbound::check_url(&url).map_err(|message| AppError::field("config", message))?;
    }
    Ok(url)
}

/// The `project_id` of a create body. Missing is a field error, and a project
/// outside the member's workspace is 404.
async fn body_project(
    state: &AppState,
    member: &Member,
    project_id: Option<Id>,
) -> Result<Id, AppError> {
    let project_id =
        project_id.ok_or_else(|| AppError::field("project_id", "Choose a project."))?;
    require_project(&state.db, member.workspace_id, project_id).await?;
    Ok(project_id)
}

async fn workspace_monitor(state: &AppState, member: &Member, id: Id) -> Result<Monitor, AppError> {
    repo::workspace_monitor(&state.db, member.workspace_id, id)
        .await?
        .ok_or(AppError::NotFound)
}

/// Linked monitors must sit in the member's workspace. Any other id reads as
/// missing, so a foreign id is indistinguishable from an unknown one.
async fn component_names(
    state: &AppState,
    member: &Member,
    ids: &[Id],
) -> Result<Vec<(Id, String)>, AppError> {
    let mut names = Vec::new();
    for id in ids {
        let monitor = repo::workspace_monitor(&state.db, member.workspace_id, *id)
            .await?
            .ok_or_else(|| AppError::field("monitor_ids", "One of the monitors does not exist."))?;
        names.push((monitor.id, monitor.name));
    }
    Ok(names)
}

async fn ensure_links(
    state: &AppState,
    member: &Member,
    page_id: Option<Id>,
    monitor_id: Option<Id>,
) -> Result<(), AppError> {
    if let Some(page_id) = page_id
        && repo::workspace_page(&state.db, member.workspace_id, page_id)
            .await?
            .is_none()
    {
        return Err(AppError::field("page_id", "Choose a status page."));
    }
    if let Some(monitor_id) = monitor_id
        && repo::workspace_monitor(&state.db, member.workspace_id, monitor_id)
            .await?
            .is_none()
    {
        return Err(AppError::field("monitor_id", "Choose a monitor."));
    }
    Ok(())
}

async fn ensure_monitors(state: &AppState, member: &Member, ids: &[Id]) -> Result<(), AppError> {
    component_names(state, member, ids).await.map(|_| ())
}

async fn channel_view(
    state: &AppState,
    channel: Channel,
) -> Result<present::ChannelResponse, AppError> {
    let monitors = repo::channel_monitors(&state.db, channel.id).await?;
    let deliveries = repo::recent_deliveries(&state.db, channel.id).await?;
    Ok(present::channel_response(&channel, monitors, deliveries))
}

fn reject_agent_monitor(monitor: &Monitor, reason: &str) -> Result<(), AppError> {
    match monitor.agent_id {
        Some(_) => Err(AppError::Conflict(reason.to_owned())),
        None => Ok(()),
    }
}

async fn require_agent(state: &AppState, headers: &HeaderMap) -> Result<Agent, AppError> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)?;
    repo::authenticate_agent(&state.db, &hash_token(token.trim()), TimestampMillis::now())
        .await?
        .ok_or(AppError::Unauthorized)
}

pub(crate) async fn user_response(
    state: &AppState,
    user: &User,
) -> Result<present::UserResponse, AppError> {
    let root_id = instance_settings::root_user_id(&state.db).await?;
    let workspaces = workspaces::access_for_user(&state.db, user.id).await?;
    Ok(present::UserResponse::with_access(
        user, root_id, workspaces,
    ))
}

pub(crate) async fn require_root(state: &AppState, jar: &CookieJar) -> Result<User, AppError> {
    let user = require_user(state, jar).await?;
    let root_id = instance_settings::root_user_id(&state.db).await?;
    if root_id != Some(user.id) {
        return Err(AppError::Forbidden(
            "Only the root user can change instance settings.",
        ));
    }
    Ok(user)
}

/// Root, or an account the root user promoted. Guards `/admin/*`.
pub(crate) async fn require_installation_admin(
    state: &AppState,
    jar: &CookieJar,
) -> Result<User, AppError> {
    let user = require_user(state, jar).await?;
    if !is_installation_admin(state, &user).await? {
        return Err(AppError::Forbidden("Only an admin can change this."));
    }
    Ok(user)
}

pub(crate) async fn is_installation_admin(state: &AppState, user: &User) -> Result<bool, AppError> {
    let root_id = instance_settings::root_user_id(&state.db).await?;
    Ok(user.installation_admin || root_id == Some(user.id))
}

/// Request headers, bodies, URL credentials, and query strings often carry API
/// tokens, so members get monitors without them.
fn monitor_view(editor: bool, mut response: present::MonitorResponse) -> present::MonitorResponse {
    if !editor {
        response.headers.clear();
        response.body.clear();
        response.url = present::without_secrets(&response.url);
    }
    response
}

pub(crate) async fn require_user(state: &AppState, jar: &CookieJar) -> Result<User, AppError> {
    let token_hash = session_token_hash(state, jar).ok_or(AppError::Unauthorized)?;
    repo::authenticate(&state.db, &token_hash, TimestampMillis::now())
        .await?
        .ok_or(AppError::Unauthorized)
}

/// `__Host-` makes the browser refuse the cookie unless it is Secure, for path
/// `/`, and set by this host, so a sibling subdomain cannot plant one.
fn cookie_name(state: &AppState) -> &'static str {
    if state.config.secure_cookies() {
        "__Host-owlee_session"
    } else {
        "owlee_session"
    }
}

fn session_token_hash(state: &AppState, jar: &CookieJar) -> Option<String> {
    jar.get(cookie_name(state))
        .map(|cookie| hash_token(cookie.value()))
}

fn expired_cookie(state: &AppState) -> Cookie<'static> {
    Cookie::build(cookie_name(state))
        .path("/")
        .secure(state.config.secure_cookies())
        .build()
}

pub(crate) async fn issue_session(
    state: &AppState,
    jar: CookieJar,
    user: &User,
    now: TimestampMillis,
) -> Result<CookieJar, AppError> {
    let token = generate_token();
    let idle = TimestampMillis::from_millis(now.as_millis() + 30 * DAY_MS);
    let absolute = TimestampMillis::from_millis(now.as_millis() + 90 * DAY_MS);
    repo::insert_session(
        &state.db,
        Id::new(),
        &hash_token(&token),
        user.id,
        now,
        idle,
        absolute,
    )
    .await?;
    Ok(session_jar(state, jar, token))
}

fn session_jar(state: &AppState, jar: CookieJar, token: String) -> CookieJar {
    let cookie = Cookie::build((cookie_name(state), token))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .secure(state.config.secure_cookies())
        .max_age(time::Duration::days(90))
        .build();
    jar.add(cookie)
}

pub(crate) struct AccountFields {
    pub(crate) email: String,
    pub(crate) normalized: String,
    pub(crate) display_name: String,
}

pub(crate) fn account_fields(
    email: &str,
    display_name: Option<String>,
) -> Result<AccountFields, AppError> {
    let email = email.trim();
    let normalized = normalize_email(email);
    if !valid_email(&normalized) {
        return Err(AppError::field("email", "Enter a valid email address."));
    }
    let display_name = display_name_field(display_name)?.unwrap_or_else(|| {
        normalized
            .split('@')
            .next()
            .unwrap_or("Operator")
            .to_owned()
    });
    Ok(AccountFields {
        email: email.to_owned(),
        normalized,
        display_name,
    })
}

/// `None` when blank, so the caller picks a default.
pub(crate) fn display_name_field(display_name: Option<String>) -> Result<Option<String>, AppError> {
    let display_name = display_name.unwrap_or_default().trim().to_owned();
    if display_name.chars().count() > 80 {
        return Err(AppError::field(
            "display_name",
            "Use at most 80 characters.",
        ));
    }
    Ok(Some(display_name).filter(|name| !name.is_empty()))
}

fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && domain.contains('.')
        && !email.contains(char::is_whitespace)
        && email.len() <= 200
}

pub(crate) fn password_field(error: PasswordError) -> AppError {
    AppError::field("password", error.to_string())
}
