//! Remote MCP server over Streamable HTTP. Every reply is one JSON body, with
//! no sessions and no SSE. A tool call is a request to `product_routes` with
//! the caller's token, so MCP can never do more than the REST API allows.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, ORIGIN};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use owlee_platform::TimestampMillis;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

use crate::api_tokens::{self, Access, ApiProduct, Scope};
use crate::error::AppError;
use crate::state::AppState;

/// Newest first. An unknown requested version gets the newest.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
const INSTRUCTIONS: &str = "Owlee error tracking. List apps and issues, read issues and their events, \
     and resolve or ignore issues. Issue ids also accept short ids such as APP-12. \
     The same token works with the REST API, described by the OpenAPI document at /api/v1/openapi.json.";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

#[derive(Clone, Copy)]
enum In {
    Path,
    Query,
    Body,
}

struct Param {
    name: &'static str,
    description: &'static str,
    at: In,
    required: bool,
    values: &'static [&'static str],
}

struct Tool {
    name: &'static str,
    description: &'static str,
    product: ApiProduct,
    method: Method,
    /// `{name}` segments take the path params.
    path: &'static str,
    params: &'static [Param],
}

impl Tool {
    fn scope(&self) -> Scope {
        Scope::new(self.product, Access::for_method(&self.method))
    }
}

const ISSUE_ID: Param = Param {
    name: "id",
    description: "Issue id, or a short id such as APP-12.",
    at: In::Path,
    required: true,
    values: &[],
};

static TOOLS: [Tool; 7] = [
    Tool {
        name: "list_error_apps",
        description: "List the error tracking apps in the workspace, with issue and event counts.",
        product: ApiProduct::Errors,
        method: Method::GET,
        path: "/error-apps",
        params: &[],
    },
    Tool {
        name: "list_issues",
        description: "List error issues, 50 per page. Pass nextCursor from the result as cursor for the next page.",
        product: ApiProduct::Errors,
        method: Method::GET,
        path: "/error-issues",
        params: &[
            Param {
                name: "status",
                description: "Filter by status. Default all.",
                at: In::Query,
                required: false,
                values: &["unresolved", "resolved", "ignored", "all"],
            },
            Param {
                name: "app_id",
                description: "Only issues of this app, from list_error_apps.",
                at: In::Query,
                required: false,
                values: &[],
            },
            Param {
                name: "q",
                description: "Search text.",
                at: In::Query,
                required: false,
                values: &[],
            },
            Param {
                name: "environment",
                description: "Only events from this environment, such as production.",
                at: In::Query,
                required: false,
                values: &[],
            },
            Param {
                name: "sort",
                description: "Sort order. Default last_seen.",
                at: In::Query,
                required: false,
                values: &[
                    "last_seen",
                    "first_seen",
                    "events",
                    "users",
                    "title",
                    "priority",
                ],
            },
            Param {
                name: "cursor",
                description: "nextCursor from the previous page.",
                at: In::Query,
                required: false,
                values: &[],
            },
        ],
    },
    Tool {
        name: "get_issue",
        description: "Get one issue with its latest event, histogram, and tags.",
        product: ApiProduct::Errors,
        method: Method::GET,
        path: "/error-issues/{id}",
        params: &[ISSUE_ID],
    },
    Tool {
        name: "list_issue_events",
        description: "List the events of one issue, newest first.",
        product: ApiProduct::Errors,
        method: Method::GET,
        path: "/error-issues/{id}/events",
        params: &[ISSUE_ID],
    },
    Tool {
        name: "get_event",
        description: "Get one event with its stack trace, tags, and context.",
        product: ApiProduct::Errors,
        method: Method::GET,
        path: "/error-events/{id}",
        params: &[Param {
            name: "id",
            description: "Event id.",
            at: In::Path,
            required: true,
            values: &[],
        }],
    },
    Tool {
        name: "list_replays",
        description: "List the session replays of one error app, newest first, 50 per page. Metadata only: \
             pages, duration, browser, release, user label, and linked error count. \
             Pass nextCursor from the result as cursor for the next page.",
        product: ApiProduct::Errors,
        method: Method::GET,
        path: "/error-apps/{id}/replays",
        params: &[
            Param {
                name: "id",
                description: "Error app id, from list_error_apps.",
                at: In::Path,
                required: true,
                values: &[],
            },
            Param {
                name: "url",
                description: "Only replays that visited a URL containing this text.",
                at: In::Query,
                required: false,
                values: &[],
            },
            Param {
                name: "release",
                description: "Only replays of this release.",
                at: In::Query,
                required: false,
                values: &[],
            },
            Param {
                name: "environment",
                description: "Only replays from this environment, such as production.",
                at: In::Query,
                required: false,
                values: &[],
            },
            Param {
                name: "hasErrors",
                description: "true lists only replays with a linked error event.",
                at: In::Query,
                required: false,
                values: &["true", "false"],
            },
            Param {
                name: "cursor",
                description: "nextCursor from the previous page.",
                at: In::Query,
                required: false,
                values: &[],
            },
        ],
    },
    Tool {
        name: "update_issue_status",
        description: "Resolve, ignore, or reopen an issue.",
        product: ApiProduct::Errors,
        method: Method::PATCH,
        path: "/error-issues/{id}",
        params: &[
            ISSUE_ID,
            Param {
                name: "status",
                description: "The new status. unresolved reopens the issue.",
                at: In::Body,
                required: true,
                values: &["unresolved", "resolved", "ignored"],
            },
        ],
    },
];

struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub async fn handle(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let expected = state.config.public_origin.trim_end_matches('/');
    if let Some(origin) = headers.get(ORIGIN)
        && origin.as_bytes() != expected.as_bytes()
    {
        return AppError::CrossOrigin {
            expected: expected.to_owned(),
            received: Some(String::from_utf8_lossy(origin.as_bytes()).into_owned()),
        }
        .into_response();
    }
    let Some(token) = api_tokens::bearer(&headers) else {
        return AppError::Unauthorized.into_response();
    };
    let owner = match api_tokens::authenticate(&state, token, TimestampMillis::now()).await {
        Ok(owner) => owner,
        Err(error) => return error.into_response(),
    };
    let message = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(message)) => message,
        Ok(_) => {
            return failure(
                StatusCode::BAD_REQUEST,
                Value::Null,
                RpcError::new(INVALID_REQUEST, "Send one JSON-RPC message, not a batch."),
            );
        }
        Err(_) => {
            return failure(
                StatusCode::BAD_REQUEST,
                Value::Null,
                RpcError::new(PARSE_ERROR, "The body is not JSON."),
            );
        }
    };
    // Notifications and client responses carry no id and get no reply.
    let (Some(method), Some(id)) = (
        message.get("method").and_then(Value::as_str),
        message.get("id").cloned(),
    ) else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "server/discover" => Ok(discover()),
        "tools/list" => Ok(json!({
            "tools": TOOLS
                .iter()
                .filter(|tool| owner.scopes.contains(&tool.scope()))
                .map(describe)
                .collect::<Vec<_>>(),
        })),
        "tools/call" => call_tool(&state, &headers, &params).await,
        _ => Err(RpcError::new(
            METHOD_NOT_FOUND,
            format!("Unknown method {method}."),
        )),
    };
    match result {
        Ok(result) => Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response(),
        Err(error) => failure(StatusCode::OK, id, error),
    }
}

fn failure(status: StatusCode, id: Value, error: RpcError) -> Response {
    (
        status,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": error.code, "message": error.message },
        })),
    )
        .into_response()
}

fn server_info() -> Value {
    json!({ "name": "owlee", "version": env!("CARGO_PKG_VERSION") })
}

fn initialize(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = PROTOCOL_VERSIONS
        .into_iter()
        .find(|version| Some(*version) == requested)
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": server_info(),
        "instructions": INSTRUCTIONS,
    })
}

fn discover() -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": PROTOCOL_VERSIONS,
        "capabilities": { "tools": {} },
        "instructions": INSTRUCTIONS,
        "_meta": { "io.modelcontextprotocol/serverInfo": server_info() },
    })
}

fn describe(tool: &Tool) -> Value {
    let mut properties = Map::new();
    for param in tool.params {
        let mut schema = json!({ "type": "string", "description": param.description });
        if !param.values.is_empty() {
            schema["enum"] = json!(param.values);
        }
        properties.insert(param.name.to_owned(), schema);
    }
    let required: Vec<&str> = tool
        .params
        .iter()
        .filter(|param| param.required)
        .map(|param| param.name)
        .collect();
    json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        },
        "annotations": { "readOnlyHint": tool.scope().access == Access::Read },
    })
}

async fn call_tool(
    state: &AppState,
    headers: &HeaderMap,
    params: &Value,
) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "Name the tool to call."))?;
    let tool = TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("Unknown tool {name}.")))?;
    let empty = Map::new();
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(arguments)) => arguments,
        Some(_) => {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "arguments must be an object.",
            ));
        }
    };
    let request = build_request(tool, arguments, headers)?;
    let response = api_tokens::product_routes()
        .with_state(state.clone())
        .oneshot(request)
        .await
        .map_err(|_| RpcError::new(INTERNAL_ERROR, "The tool failed."))?;
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|_| RpcError::new(INTERNAL_ERROR, "The tool failed."))?
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if !status.is_success() {
        let detail = body
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or_else(|| status.canonical_reason().unwrap_or("The request failed."));
        return Ok(json!({
            "content": [{ "type": "text", "text": detail }],
            "isError": true,
        }));
    }
    let text = serde_json::to_string_pretty(&body).unwrap_or_default();
    let mut result = json!({ "content": [{ "type": "text", "text": text }], "isError": false });
    if body.is_object() {
        result["structuredContent"] = body;
    }
    Ok(result)
}

fn build_request(
    tool: &Tool,
    arguments: &Map<String, Value>,
    headers: &HeaderMap,
) -> Result<Request<Body>, RpcError> {
    if let Some(unknown) = arguments
        .keys()
        .find(|key| tool.params.iter().all(|param| param.name != key.as_str()))
    {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("{} has no argument {unknown}.", tool.name),
        ));
    }
    let mut path = tool.path.to_owned();
    let mut query = Vec::new();
    let mut body = Map::new();
    for param in tool.params {
        let value = match arguments.get(param.name) {
            None | Some(Value::Null) if param.required => {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    format!("{} needs {}.", tool.name, param.name),
                ));
            }
            None | Some(Value::Null) => continue,
            Some(Value::String(value)) => value,
            Some(_) => {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    format!("{} must be a string.", param.name),
                ));
            }
        };
        let encoded = utf8_percent_encode(value, NON_ALPHANUMERIC).to_string();
        match param.at {
            In::Path => path = path.replace(&format!("{{{}}}", param.name), &encoded),
            In::Query => query.push(format!("{}={encoded}", param.name)),
            In::Body => {
                body.insert(param.name.to_owned(), Value::String(value.clone()));
            }
        }
    }
    if !query.is_empty() {
        path = format!("{path}?{}", query.join("&"));
    }
    let mut request = Request::builder().method(tool.method.clone()).uri(path);
    if let Some(authorization) = headers.get(AUTHORIZATION) {
        request = request.header(AUTHORIZATION, authorization);
    }
    let body = if body.is_empty() {
        Body::empty()
    } else {
        request = request.header(CONTENT_TYPE, "application/json");
        Body::from(Value::Object(body).to_string())
    };
    request
        .body(body)
        .map_err(|_| RpcError::new(INVALID_PARAMS, "The arguments do not form a valid request."))
}
