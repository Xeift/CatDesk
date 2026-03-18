use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Response, StatusCode, header},
    response::Json,
    routing::{delete, get, post},
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::mcp::{self, JsonRpcRequest, Session};
use crate::state::SharedState;

type Sessions = Arc<Mutex<HashMap<String, Session>>>;

#[derive(Clone)]
struct ServerState {
    sessions: Sessions,
    app: SharedState,
}

/// Build the axum router.
pub fn router(app_state: SharedState) -> Router {
    let state = ServerState {
        sessions: Arc::new(Mutex::new(HashMap::new())),
        app: app_state,
    };
    Router::new()
        .route("/", get(health))
        .route("/mcp", post(post_mcp))
        .route("/mcp", get(get_mcp))
        .route("/mcp", delete(delete_mcp))
        .with_state(state)
}

/// Wrap a JSON-RPC error as an SSE event with text/event-stream Content-Type.
fn sse_error_response(status: StatusCode, code: i64, msg: &str, session_id: Option<&str>) -> Response<Body> {
    let error_json = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "error": {"code": code, "message": msg}
    }))
    .unwrap();
    let sse_body = format!("event: message\ndata: {error_json}\n\n");
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-transform");
    if let Some(sid) = session_id {
        builder = builder.header("mcp-session-id", sid);
    }
    builder.body(Body::from(sse_body)).unwrap()
}

// ── GET / — health ──────────────────────────────────────────

async fn health(State(s): State<ServerState>) -> Json<Value> {
    let app = s.app.lock().await;
    Json(json!({
        "status": "ok",
        "name": "XeduckV4 MCP Server (Rust)",
        "description": "Minimal MCP server with a single run_command tool",
        "workspace": app.workspace_root,
        "tools": ["run_command"],
    }))
}

// ── POST /mcp ───────────────────────────────────────────────
//
// Accept raw bytes to avoid axum's Json extractor returning application/json
// rejection errors that violate the MCP client's expectation.

async fn post_mcp(
    State(s): State<ServerState>,
    headers: HeaderMap,
    body_bytes: Bytes,
) -> Response<Body> {
    // Parse body ourselves so we control the error Content-Type
    let body: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(e) => {
            return sse_error_response(
                StatusCode::BAD_REQUEST,
                -32700,
                &format!("Parse error: {e}"),
                None,
            );
        }
    };

    // Increment request count
    {
        let mut app = s.app.lock().await;
        app.request_count += 1;
    }

    let workspace_root = {
        let app = s.app.lock().await;
        app.workspace_root.clone()
    };

    // Support both single request and batch
    let requests: Vec<Value> = if body.is_array() {
        body.as_array().unwrap().clone()
    } else {
        vec![body]
    };

    let session_id_header = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Check if batch contains an initialize request
    let has_initialize = requests
        .iter()
        .any(|r| r.get("method").and_then(|m| m.as_str()) == Some("initialize"));

    // Check if any message is a JSON-RPC request (has "id")
    let has_any_request = requests.iter().any(|r| r.get("id").is_some());

    // Session lookup/creation per MCP spec:
    // - initialize → create new session
    // - other → session-id MUST be present and valid; if not found → 404
    let mut sessions = s.sessions.lock().await;

    let session_id: String;
    if has_initialize {
        let new_session = Session::new();
        session_id = new_session.id.clone();
        sessions.insert(session_id.clone(), new_session);
        // Mark remote as connected
        let mut app = s.app.lock().await;
        app.remote_connected = true;
    } else if let Some(ref sid) = session_id_header {
        if !sessions.contains_key(sid) {
            drop(sessions);
            let mut app = s.app.lock().await;
            app.log(
                "WARN",
                format!("Session not found: {}", &sid[..sid.len().min(8)]),
            );
            return sse_error_response(
                StatusCode::NOT_FOUND,
                -32000,
                "Session not found. Please reinitialize.",
                None,
            );
        }
        session_id = sid.clone();
    } else {
        drop(sessions);
        return sse_error_response(
            StatusCode::BAD_REQUEST,
            -32000,
            "Missing Mcp-Session-Id header",
            None,
        );
    }

    // Process all messages
    let mut responses: Vec<Value> = Vec::new();

    for req_val in &requests {
        let req: JsonRpcRequest = match serde_json::from_value(req_val.clone()) {
            Ok(r) => r,
            Err(e) => {
                responses.push(
                    serde_json::to_value(mcp::JsonRpcResponse::error(
                        None,
                        -32700,
                        format!("Parse error: {e}"),
                    ))
                    .unwrap(),
                );
                continue;
            }
        };

        let session = sessions.get_mut(&session_id).unwrap();
        if let Some(resp) = mcp::handle_request(&req, session, &workspace_root).await {
            responses.push(serde_json::to_value(resp).unwrap());
        }
    }

    // Update session count + log
    {
        let mut app = s.app.lock().await;
        app.session_count = sessions.len();
        app.log(
            "INFO",
            format!(
                "POST /mcp session={} ({} msg(s))",
                &session_id[..session_id.len().min(8)],
                requests.len()
            ),
        );
    }

    drop(sessions);

    // MCP spec response rules:
    // - Pure notifications → 202 Accepted, empty body
    // - Has requests → SSE stream with event: message + data: JSON-RPC response
    if !has_any_request {
        return Response::builder()
            .status(StatusCode::ACCEPTED)
            .header("mcp-session-id", &session_id)
            .body(Body::empty())
            .unwrap();
    }

    // Build SSE response body
    let mut sse_body = String::new();
    for resp in &responses {
        sse_body.push_str("event: message\n");
        sse_body.push_str("data: ");
        sse_body.push_str(&serde_json::to_string(resp).unwrap());
        sse_body.push_str("\n\n");
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header(header::CONNECTION, "keep-alive")
        .header("mcp-session-id", &session_id)
        .body(Body::from(sse_body))
        .unwrap()
}

// ── GET /mcp — SSE for server-initiated messages ────────────

async fn get_mcp(
    State(s): State<ServerState>,
    headers: HeaderMap,
) -> Response<Body> {
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let sessions = s.sessions.lock().await;
    match &session_id {
        Some(sid) if sessions.contains_key(sid) => {}
        Some(_) => {
            return sse_error_response(StatusCode::NOT_FOUND, -32000, "Session not found", None);
        }
        None => {
            return sse_error_response(
                StatusCode::BAD_REQUEST,
                -32000,
                "Missing Mcp-Session-Id header",
                None,
            );
        }
    }
    let sid = session_id.unwrap();
    drop(sessions);

    // Channel-based SSE stream that stays open
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::convert::Infallible>>(16);

    // Send initial endpoint event
    let _ = tx
        .send(Ok("event: endpoint\ndata: /mcp\n\n".to_string()))
        .await;

    // Keep connection alive with periodic comments
    let sid_clone = sid.clone();
    let app_state = s.app.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if tx.send(Ok(": keepalive\n\n".to_string())).await.is_err() {
                let mut app = app_state.lock().await;
                app.log(
                    "INFO",
                    format!(
                        "SSE stream closed: {}",
                        &sid_clone[..sid_clone.len().min(8)]
                    ),
                );
                break;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header(header::CONNECTION, "keep-alive")
        .header("mcp-session-id", &sid)
        .body(body)
        .unwrap()
}

// ── DELETE /mcp ─────────────────────────────────────────────

async fn delete_mcp(
    State(s): State<ServerState>,
    headers: HeaderMap,
) -> Response<Body> {
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if let Some(sid) = session_id {
        let mut sessions = s.sessions.lock().await;
        if sessions.remove(&sid).is_some() {
            let mut app = s.app.lock().await;
            app.session_count = sessions.len();
            if sessions.is_empty() {
                app.remote_connected = false;
            }
            app.log(
                "INFO",
                format!("Session closed: {}", &sid[..sid.len().min(8)]),
            );
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"status":"session closed"}"#))
                .unwrap();
        }
    }

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"error":"Session not found"}"#))
        .unwrap()
}
