//! Personal API tokens. A token acts as its user in one workspace, limited to
//! its scopes and to the routers in `product_routes`. Every other route
//! refuses it, so a new route stays closed to tokens until it joins a product.

use std::collections::BTreeMap;

use axum::extract::{Path, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use owlee_platform::{Id, TimestampMillis, generate_token, hash_token};
use serde::{Deserialize, Serialize, Serializer};
use sqlx::Row;

use crate::error::AppError;
use crate::repo::{self, User, millis, parse_id};
use crate::state::AppState;
use crate::workspaces::{Member, Role, audit};

pub const TOKEN_PREFIX: &str = "owl_";
const DISPLAY_PREFIX_LEN: usize = 12;
const LAST_USED_THROTTLE_MS: i64 = 5 * 60 * 1000;
const EXPIRY_DAYS: [i64; 4] = [7, 30, 90, 365];
const DAY_MS: i64 = 86_400_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiProduct {
    Errors,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Access {
    Read,
    Write,
    Delete,
}

impl Access {
    #[must_use]
    pub fn for_method(method: &Method) -> Self {
        match *method {
            Method::GET | Method::HEAD => Self::Read,
            Method::DELETE => Self::Delete,
            _ => Self::Write,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Scope {
    pub product: ApiProduct,
    pub access: Access,
}

impl Scope {
    pub const ALL: [Self; 3] = [
        Self::new(ApiProduct::Errors, Access::Read),
        Self::new(ApiProduct::Errors, Access::Write),
        Self::new(ApiProduct::Errors, Access::Delete),
    ];

    #[must_use]
    pub const fn new(product: ApiProduct, access: Access) -> Self {
        Self { product, access }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match (self.product, self.access) {
            (ApiProduct::Errors, Access::Read) => "errors:read",
            (ApiProduct::Errors, Access::Write) => "errors:write",
            (ApiProduct::Errors, Access::Delete) => "errors:delete",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scope| scope.as_str() == text)
    }
}

impl Serialize for Scope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// The projects and error apps a token reaches. `None` on a token reaches
/// every resource of its workspace. A project covers all of its apps, also
/// ones created later.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceLimit {
    pub projects: Vec<Id>,
    pub error_apps: Vec<String>,
}

/// The routers a token can call, each tagged with the product its scopes name.
pub fn product_routes() -> Router<AppState> {
    crate::errors::api_routes().route_layer(Extension(ApiProduct::Errors))
}

/// Cookie only. `deny_tokens` covers them, so a token cannot mint more tokens.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api-tokens", get(list).post(create))
        .route("/api-tokens/{id}", delete(revoke))
        .route("/api-tokens/{id}/rotate", post(rotate))
        .route("/api-tokens/{id}/pause", post(pause))
        .route("/api-tokens/{id}/resume", post(resume))
}

/// The token of an `Authorization: Bearer owl_…` header. Another bearer, such
/// as one a proxy forwards, is not ours and leaves the cookie in charge.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && token.starts_with(TOKEN_PREFIX)).then_some(token)
}

/// Route layer for every cookie route outside `product_routes`.
pub(crate) async fn deny_tokens(request: Request, next: Next) -> Response {
    if bearer(request.headers()).is_some() {
        return AppError::TokenNotAllowed.into_response();
    }
    next.run(request).await
}

pub(crate) struct TokenOwner {
    pub user: User,
    pub workspace_id: Id,
    pub role: Role,
    pub scopes: Vec<Scope>,
    pub limit: Option<ResourceLimit>,
}

/// 401 when the token is unknown, revoked, paused, or expired, when its user is
/// disabled, or when the user left the workspace.
pub(crate) async fn authenticate(
    state: &AppState,
    token: &str,
    now: TimestampMillis,
) -> Result<TokenOwner, AppError> {
    let token_hash = hash_token(token);
    let row = sqlx::query(
        "SELECT t.id, t.workspace_id, t.user_id, t.scopes_json, t.resources_json, t.last_used_at, t.expires_at, m.role \
         FROM api_tokens t JOIN memberships m ON m.workspace_id = t.workspace_id AND m.user_id = t.user_id \
         WHERE t.token_hash = $1 AND t.revoked_at IS NULL AND t.paused_at IS NULL",
    )
    .bind(&token_hash)
    .fetch_optional(state.db.pool())
    .await?
    .ok_or(AppError::Unauthorized)?;
    let expires_at: Option<i64> = row.try_get("expires_at")?;
    if expires_at.is_some_and(|expires| now.as_millis() >= expires) {
        return Err(AppError::Unauthorized);
    }
    let user = repo::user_by_id(&state.db, parse_id(&row, "user_id")?)
        .await?
        .filter(|user| !user.disabled)
        .ok_or(AppError::Unauthorized)?;
    let last_used: Option<i64> = row.try_get("last_used_at")?;
    if last_used.is_none_or(|last| now.as_millis() - last > LAST_USED_THROTTLE_MS) {
        sqlx::query("UPDATE api_tokens SET last_used_at = $1 WHERE token_hash = $2")
            .bind(now.as_millis())
            .bind(&token_hash)
            .execute(state.db.pool())
            .await?;
    }
    Ok(TokenOwner {
        user,
        workspace_id: parse_id(&row, "workspace_id")?,
        role: Role::from_row(&row)?,
        scopes: parse_scopes(&row)?,
        limit: parse_resources(&row)?,
    })
}

fn parse_scopes(row: &sqlx::any::AnyRow) -> Result<Vec<Scope>, sqlx::Error> {
    let raw: String = row.try_get("scopes_json")?;
    let strings: Vec<String> =
        serde_json::from_str(&raw).map_err(|error| sqlx::Error::Decode(error.into()))?;
    strings
        .iter()
        .map(|text| {
            Scope::parse(text)
                .ok_or_else(|| sqlx::Error::Decode(format!("unknown token scope {text}").into()))
        })
        .collect()
}

fn parse_resources(row: &sqlx::any::AnyRow) -> Result<Option<ResourceLimit>, sqlx::Error> {
    row.try_get::<Option<String>, _>("resources_json")?
        .map(|raw| serde_json::from_str(&raw).map_err(|error| sqlx::Error::Decode(error.into())))
        .transpose()
}

#[derive(Serialize)]
struct TokenRow {
    id: Id,
    name: String,
    prefix: String,
    scopes: Vec<Scope>,
    resources: Option<ResourceLimit>,
    created_at: TimestampMillis,
    last_used_at: Option<TimestampMillis>,
    expires_at: Option<TimestampMillis>,
    paused_at: Option<TimestampMillis>,
    /// Only in the create and rotate responses. Owlee keeps the hash, not the token.
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
}

const COLUMNS: &str = "id, name, token_prefix, scopes_json, resources_json, created_at, last_used_at, expires_at, paused_at";

fn map_row(row: &sqlx::any::AnyRow) -> Result<TokenRow, sqlx::Error> {
    let optional = |name: &str| -> Result<Option<TimestampMillis>, sqlx::Error> {
        Ok(row
            .try_get::<Option<i64>, _>(name)?
            .map(TimestampMillis::from_millis))
    };
    Ok(TokenRow {
        id: parse_id(row, "id")?,
        name: row.try_get("name")?,
        prefix: row.try_get("token_prefix")?,
        scopes: parse_scopes(row)?,
        resources: parse_resources(row)?,
        created_at: millis(row, "created_at")?,
        last_used_at: optional("last_used_at")?,
        expires_at: optional("expires_at")?,
        paused_at: optional("paused_at")?,
        token: None,
    })
}

async fn list(
    State(state): State<AppState>,
    member: Member,
) -> Result<Json<Vec<TokenRow>>, AppError> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM api_tokens WHERE workspace_id = $1 AND user_id = $2 AND revoked_at IS NULL \
         ORDER BY created_at DESC, id DESC"
    ))
    .bind(member.workspace_id.to_string())
    .bind(member.user.id.to_string())
    .fetch_all(state.db.pool())
    .await?;
    Ok(Json(
        rows.iter().map(map_row).collect::<Result<Vec<_>, _>>()?,
    ))
}

#[derive(Deserialize)]
struct NewToken {
    name: String,
    scopes: Vec<String>,
    expires_in_days: Option<i64>,
    #[serde(default)]
    resources: Option<NewResources>,
}

#[derive(Deserialize)]
struct NewResources {
    projects: Vec<String>,
    error_apps: Vec<String>,
}

/// The inner error is the message for the `resources` field. Every id must
/// name a project or an error app of the member's workspace.
async fn parse_limit(
    state: &AppState,
    workspace_id: Id,
    body: &NewResources,
) -> Result<Result<ResourceLimit, &'static str>, AppError> {
    const FOREIGN: &str = "Choose projects and apps in this workspace.";
    if body.projects.is_empty() && body.error_apps.is_empty() {
        return Ok(Err("Choose at least one project or app."));
    }
    let projects: Vec<String> =
        sqlx::query_scalar("SELECT id FROM projects WHERE workspace_id = $1")
            .bind(workspace_id.to_string())
            .fetch_all(state.db.pool())
            .await?;
    let apps: Vec<String> = sqlx::query_scalar(
        "SELECT a.id FROM error_apps a JOIN projects pr ON pr.id = a.project_id WHERE pr.workspace_id = $1",
    )
    .bind(workspace_id.to_string())
    .fetch_all(state.db.pool())
    .await?;
    let mut limit = ResourceLimit {
        projects: Vec::new(),
        error_apps: Vec::new(),
    };
    for raw in &body.projects {
        let Some(id) = raw
            .parse::<Id>()
            .ok()
            .filter(|id| projects.contains(&id.to_string()))
        else {
            return Ok(Err(FOREIGN));
        };
        if !limit.projects.contains(&id) {
            limit.projects.push(id);
        }
    }
    for raw in &body.error_apps {
        if !apps.contains(raw) {
            return Ok(Err(FOREIGN));
        }
        if !limit.error_apps.contains(raw) {
            limit.error_apps.push(raw.clone());
        }
    }
    Ok(Ok(limit))
}

async fn create(
    State(state): State<AppState>,
    member: Member,
    Json(body): Json<NewToken>,
) -> Result<(StatusCode, Json<TokenRow>), AppError> {
    let mut errors = BTreeMap::new();
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        errors.insert("name".to_owned(), "Use 1 to 100 characters.".to_owned());
    }
    let parsed: Option<Vec<Scope>> = body.scopes.iter().map(|text| Scope::parse(text)).collect();
    match parsed {
        None => {
            errors.insert("scopes".to_owned(), "Choose known scopes only.".to_owned());
        }
        Some(scopes) if scopes.is_empty() => {
            errors.insert("scopes".to_owned(), "Choose at least one scope.".to_owned());
        }
        Some(scopes)
            if !member.is_editor() && scopes.iter().any(|scope| scope.access != Access::Read) =>
        {
            errors.insert(
                "scopes".to_owned(),
                "A workspace member can choose read scopes only.".to_owned(),
            );
        }
        Some(_) => {}
    }
    if body
        .expires_in_days
        .is_some_and(|days| !EXPIRY_DAYS.contains(&days))
    {
        errors.insert(
            "expires_in_days".to_owned(),
            "Choose 7, 30, 90, or 365 days, or no expiry.".to_owned(),
        );
    }
    let limit = match &body.resources {
        None => None,
        Some(resources) => match parse_limit(&state, member.workspace_id, resources).await? {
            Ok(limit) => Some(limit),
            Err(message) => {
                errors.insert("resources".to_owned(), message.to_owned());
                None
            }
        },
    };
    if !errors.is_empty() {
        return Err(AppError::Validation(errors));
    }
    // Canonical order without duplicates.
    let scopes: Vec<Scope> = Scope::ALL
        .into_iter()
        .filter(|scope| body.scopes.iter().any(|text| text == scope.as_str()))
        .collect();
    let scope_strings: Vec<&str> = scopes.iter().map(|scope| scope.as_str()).collect();

    let now = TimestampMillis::now();
    let id = Id::new();
    let token = format!("{TOKEN_PREFIX}{}", generate_token());
    let prefix = token[..DISPLAY_PREFIX_LEN].to_owned();
    let expires_at = body
        .expires_in_days
        .map(|days| TimestampMillis::from_millis(now.as_millis() + days * DAY_MS));
    sqlx::query(
        "INSERT INTO api_tokens (id, workspace_id, user_id, name, token_prefix, token_hash, scopes_json, resources_json, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id.to_string())
    .bind(member.workspace_id.to_string())
    .bind(member.user.id.to_string())
    .bind(name)
    .bind(&prefix)
    .bind(hash_token(&token))
    .bind(serde_json::to_string(&scope_strings).map_err(|_| AppError::Internal)?)
    .bind(
        limit
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| AppError::Internal)?,
    )
    .bind(now.as_millis())
    .bind(expires_at.map(TimestampMillis::as_millis))
    .execute(state.db.pool())
    .await?;
    audit(
        &state.db,
        &member,
        "api_token.created",
        "api_token",
        Some(id),
        now,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(TokenRow {
            id,
            name: name.to_owned(),
            prefix,
            scopes,
            resources: limit,
            created_at: now,
            last_used_at: None,
            expires_at,
            paused_at: None,
            token: Some(token),
        }),
    ))
}

/// Only the caller's own live tokens in this workspace match, so any other id is 404.
async fn revoke(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<StatusCode, AppError> {
    let now = TimestampMillis::now();
    let revoked = sqlx::query(
        "UPDATE api_tokens SET revoked_at = $1 \
         WHERE id = $2 AND workspace_id = $3 AND user_id = $4 AND revoked_at IS NULL",
    )
    .bind(now.as_millis())
    .bind(id.to_string())
    .bind(member.workspace_id.to_string())
    .bind(member.user.id.to_string())
    .execute(state.db.pool())
    .await?;
    if revoked.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    audit(
        &state.db,
        &member,
        "api_token.revoked",
        "api_token",
        Some(id),
        now,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

type AnyQuery<'q> = sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments<'q>>;

/// Applies `set` to one of the caller's own live tokens in this workspace and
/// audits `action`. Any other id is 404. `set` starts its placeholders at `$4`.
async fn update_own(
    state: &AppState,
    member: &Member,
    id: Id,
    action: &str,
    set: &str,
    bind: impl for<'q> FnOnce(AnyQuery<'q>) -> AnyQuery<'q>,
) -> Result<TokenRow, AppError> {
    let sql = format!(
        "UPDATE api_tokens SET {set} \
         WHERE id = $1 AND workspace_id = $2 AND user_id = $3 AND revoked_at IS NULL \
         RETURNING {COLUMNS}"
    );
    let query = sqlx::query(&sql)
        .bind(id.to_string())
        .bind(member.workspace_id.to_string())
        .bind(member.user.id.to_string());
    let row = bind(query)
        .fetch_optional(state.db.pool())
        .await?
        .ok_or(AppError::NotFound)?;
    audit(
        &state.db,
        member,
        action,
        "api_token",
        Some(id),
        TimestampMillis::now(),
    )
    .await;
    Ok(map_row(&row)?)
}

async fn rotate(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<Json<TokenRow>, AppError> {
    let token = format!("{TOKEN_PREFIX}{}", generate_token());
    let prefix = token[..DISPLAY_PREFIX_LEN].to_owned();
    let hash = hash_token(&token);
    let mut row = update_own(
        &state,
        &member,
        id,
        "api_token.rotated",
        "token_hash = $4, token_prefix = $5",
        |query| query.bind(hash).bind(prefix),
    )
    .await?;
    row.token = Some(token);
    Ok(Json(row))
}

/// Pausing again keeps the first pause time.
async fn pause(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<Json<TokenRow>, AppError> {
    let now = TimestampMillis::now().as_millis();
    let row = update_own(
        &state,
        &member,
        id,
        "api_token.paused",
        "paused_at = COALESCE(paused_at, $4)",
        |query| query.bind(now),
    )
    .await?;
    Ok(Json(row))
}

async fn resume(
    State(state): State<AppState>,
    member: Member,
    Path(id): Path<Id>,
) -> Result<Json<TokenRow>, AppError> {
    let row = update_own(
        &state,
        &member,
        id,
        "api_token.resumed",
        "paused_at = NULL",
        |query| query,
    )
    .await?;
    Ok(Json(row))
}
