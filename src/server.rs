use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Form, Path, State},
    http::{HeaderMap, Response, StatusCode, header},
    response::Json,
    routing::{delete, get, post},
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc::UnboundedSender};

use crate::devtools::DevtoolsBridge;
use crate::mcp::{self, JsonRpcRequest, Session, WIDGET_PAYLOAD_META_KEY};
use crate::state::{
    AgentsPathMode, FlowDirection, ServerUiEvent, SharedState, UsageTotals, parse_seed_hex,
    save_agents_path_mode,
};

type Sessions = Arc<Mutex<HashMap<String, Session>>>;

#[derive(Clone)]
struct ServerState {
    sessions: Sessions,
    app: SharedState,
    devtools: Option<Arc<Mutex<DevtoolsBridge>>>,
    ui_events: UnboundedSender<ServerUiEvent>,
}

/// Build the axum router.
pub fn router(
    app_state: SharedState,
    devtools: Option<Arc<Mutex<DevtoolsBridge>>>,
    mcp_path: String,
    ui_events: UnboundedSender<ServerUiEvent>,
) -> Router {
    let state = ServerState {
        sessions: Arc::new(Mutex::new(HashMap::new())),
        app: app_state,
        devtools,
        ui_events,
    };
    Router::new()
        .route("/", get(health))
        .route(
            "/binagotchy/archive/{folder}/save",
            post(post_save_binagotchy_folder).options(options_binagotchy_archive_save),
        )
        .route(
            "/binagotchy/partner",
            post(post_binagotchy_partner).options(options_binagotchy_partner),
        )
        .route(
            "/agents/path-mode",
            post(post_agents_path_mode).options(options_agents_path_mode),
        )
        .route(
            "/agents/path-state",
            get(get_agents_path_state).options(options_agents_path_state),
        )
        .route(&mcp_path, post(post_mcp))
        .route(&mcp_path, get(get_mcp))
        .route(&mcp_path, delete(delete_mcp))
        .with_state(state)
}

fn with_widget_action_cors(
    mut builder: axum::http::response::Builder,
) -> axum::http::response::Builder {
    builder = builder.header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
    builder = builder.header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, OPTIONS");
    builder = builder.header(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "content-type, ngrok-skip-browser-warning",
    );
    builder = builder.header(header::CACHE_CONTROL, "no-store");
    builder
}

fn sse_error_response(
    status: StatusCode,
    code: i64,
    msg: &str,
    session_id: Option<&str>,
) -> Response<Body> {
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

fn short_session_id(sid: &str) -> String {
    sid[..sid.len().min(8)].to_string()
}

fn single_active_session_id(sessions: &HashMap<String, Session>) -> Option<String> {
    if sessions.len() == 1 {
        sessions.keys().next().cloned()
    } else {
        None
    }
}

fn summarize_list(items: &[String], max_items: usize) -> String {
    if items.is_empty() {
        return "-".into();
    }
    if items.len() <= max_items {
        return items.join(", ");
    }
    format!(
        "{} ... (+{} more)",
        items[..max_items].join(", "),
        items.len() - max_items
    )
}

fn request_id(req: &Value) -> String {
    req.get("id").map_or("-".into(), |v| match v {
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    })
}

fn request_tool_name(req: &Value) -> Option<String> {
    req.get("params")
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

fn request_flow_label(req: &Value) -> String {
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("<invalid-method>");
    if method == "tools/call" {
        let tool = request_tool_name(req).unwrap_or_else(|| "?".into());
        return format!("tools/call:{tool}");
    }
    method.to_string()
}

fn summarize_request(req: &Value) -> String {
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("<invalid-method>");
    let id = request_id(req);
    if method == "tools/call" {
        let tool = request_tool_name(req).unwrap_or_else(|| "?".into());
        return format!("tools/call({tool},id={id})");
    }
    format!("{method}(id={id})")
}

fn summarize_response(resp: &Value) -> String {
    let id = resp.get("id").map_or("-".into(), |v| match v {
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    });
    if let Some(err) = resp.get("error") {
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(-32000);
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Unknown error");
        return format!("id={id}:error({code} {msg})");
    }
    if resp.get("result").is_some() {
        return format!("id={id}:result");
    }
    format!("id={id}:unknown")
}

fn tool_name_from_jsonrpc_request(req: &JsonRpcRequest) -> &str {
    req.params
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown_tool")
}

fn extract_turn_token_usage(result: Option<&Value>) -> Option<(u64, u64)> {
    let usage = result
        .and_then(|value| value.get("_meta"))
        .and_then(Value::as_object)
        .and_then(|meta| meta.get(WIDGET_PAYLOAD_META_KEY))
        .and_then(Value::as_object)
        .and_then(|payload| payload.get("turnTokenUsage"))?;
    let input_tokens = usage.get("inputTokens").and_then(Value::as_u64)?;
    let output_tokens = usage.get("outputTokens").and_then(Value::as_u64)?;
    Some((input_tokens, output_tokens))
}

fn attach_history_usage(result: &mut Option<Value>, usage_totals: &UsageTotals) {
    let Some(result_obj) = result.as_mut().and_then(Value::as_object_mut) else {
        return;
    };
    let history_usage = json!({
        "inputTokens": usage_totals.input_tokens,
        "outputTokens": usage_totals.output_tokens,
        "totalTokens": usage_totals.total_tokens,
    });
    let history_tool_call_count = json!(usage_totals.tool_call_count);
    if let Some(widget_payload) = result_obj
        .get_mut("_meta")
        .and_then(Value::as_object_mut)
        .and_then(|meta| meta.get_mut(WIDGET_PAYLOAD_META_KEY))
        .and_then(Value::as_object_mut)
    {
        widget_payload.insert("historyTurnTokenUsage".to_string(), history_usage);
        widget_payload.insert("historyToolCallCount".to_string(), history_tool_call_count);
    }
}

// ── GET / — health ──────────────────────────────────────────

async fn health(State(s): State<ServerState>) -> Json<Value> {
    let app = s.app.lock().await;
    Json(json!({
        "status": "ok",
        "name": "CatDesk",
        "description": "MCP Tools for ChatGPT to control your computer and browser",
        "mode": app.mode.label(),
        "tool_mode": app.tool_mode.label(),
        "workspace": app.workspace_root,
    }))
}

fn attach_catdesk_instruction_actions(
    result: &mut Option<Value>,
    public_base_url: Option<&str>,
    mascot_seed: u64,
    partner_binagotchy_seed: Option<&str>,
) {
    let Some(result_obj) = result.as_mut().and_then(Value::as_object_mut) else {
        return;
    };
    let Some(structured) = result_obj
        .get_mut("structuredContent")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let Some(tool_name) = structured.get("toolName").and_then(Value::as_str) else {
        return;
    };
    if tool_name != "catdesk_instruction" {
        return;
    }

    let Some(widget_payload) = result_obj
        .get_mut("_meta")
        .and_then(Value::as_object_mut)
        .and_then(|meta| meta.get_mut(WIDGET_PAYLOAD_META_KEY))
        .and_then(Value::as_object_mut)
    else {
        return;
    };

    let binagotchy_action_base_url = public_base_url.map(|base| format!("{base}/binagotchy"));
    widget_payload.insert(
        "binagotchyApiBaseUrl".to_string(),
        json!(binagotchy_action_base_url.clone().unwrap_or_default()),
    );
    widget_payload.insert(
        "agentsPathModeUrl".to_string(),
        json!(public_base_url
            .map(|base| format!("{base}/agents/path-mode"))
            .unwrap_or_default()),
    );
    widget_payload.insert(
        "agentsPathStateUrl".to_string(),
        json!(public_base_url
            .map(|base| format!("{base}/agents/path-state"))
            .unwrap_or_default()),
    );
    widget_payload.insert(
        "partnerBinagotchySeed".to_string(),
        json!(partner_binagotchy_seed.unwrap_or("")),
    );
    widget_payload.insert(
        "widgetMascot".to_string(),
        json!(crate::mascot::build_widget_mascot(mascot_seed)),
    );

    if let Some(cards) = widget_payload
        .get_mut("binagotchyCards")
        .and_then(Value::as_array_mut)
    {
        for card in cards.iter_mut() {
            let Some(card_obj) = card.as_object_mut() else {
                continue;
            };
            let Some(folder) = card_obj
                .get("folder")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            let is_partner = partner_binagotchy_seed
                .zip(card_obj.get("seed").and_then(Value::as_str))
                .is_some_and(|(partner_seed, card_seed)| partner_seed == card_seed);
            card_obj.insert("isPartner".to_string(), json!(is_partner));
            if let Some(base) = binagotchy_action_base_url.as_deref() {
                card_obj.insert(
                    "saveFolderUrl".to_string(),
                    json!(format!("{base}/archive/{folder}/save")),
                );
                card_obj.insert(
                    "setPartnerUrl".to_string(),
                    json!(format!("{base}/partner")),
                );
            }
        }
    }
}

async fn post_save_binagotchy_folder(
    Path(folder): Path<String>,
    State(_s): State<ServerState>,
) -> Response<Body> {
    match crate::mascot::save_archived_binagotchy_folder(&folder) {
        Ok(saved_path) => with_widget_action_cors(Response::builder())
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "ok": true,
                    "folder": folder,
                    "savedPath": saved_path.to_string_lossy(),
                })
                .to_string(),
            ))
            .unwrap(),
        Err(error) => with_widget_action_cors(Response::builder())
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": error.to_string() }).to_string(),
            ))
            .unwrap(),
    }
}

async fn options_binagotchy_archive_save(
    Path(_folder): Path<String>,
    State(_s): State<ServerState>,
) -> Response<Body> {
    with_widget_action_cors(Response::builder())
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap()
}

async fn options_binagotchy_partner(State(_s): State<ServerState>) -> Response<Body> {
    with_widget_action_cors(Response::builder())
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap()
}

fn parse_agents_path_mode(value: &str) -> Option<AgentsPathMode> {
    match value.trim() {
        "default" => Some(AgentsPathMode::Default),
        "workspace" => Some(AgentsPathMode::Workspace),
        "catdesk" => Some(AgentsPathMode::Catdesk),
        "codex" => Some(AgentsPathMode::Codex),
        "disabled" => Some(AgentsPathMode::Disabled),
        _ => None,
    }
}

async fn post_agents_path_mode(
    State(s): State<ServerState>,
    Form(form): Form<HashMap<String, String>>,
) -> Response<Body> {
    let Some(mode_raw) = form.get("mode").map(String::as_str) else {
        return with_widget_action_cors(Response::builder())
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": "missing mode" }).to_string(),
            ))
            .unwrap();
    };
    let Some(mode) = parse_agents_path_mode(mode_raw) else {
        return with_widget_action_cors(Response::builder())
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": "invalid mode" }).to_string(),
            ))
            .unwrap();
    };

    let workspace_root = {
        let app = s.app.lock().await;
        app.workspace_root.clone()
    };

    if let Err(error) = save_agents_path_mode(mode) {
        return with_widget_action_cors(Response::builder())
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": error.to_string() }).to_string(),
            ))
            .unwrap();
    }

    agents_state_response(&workspace_root)
}

async fn options_agents_path_mode(State(_s): State<ServerState>) -> Response<Body> {
    with_widget_action_cors(Response::builder())
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap()
}

async fn get_agents_path_state(State(s): State<ServerState>) -> Response<Body> {
    let workspace_root = {
        let app = s.app.lock().await;
        app.workspace_root.clone()
    };
    agents_state_response(&workspace_root)
}

async fn options_agents_path_state(State(_s): State<ServerState>) -> Response<Body> {
    with_widget_action_cors(Response::builder())
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap()
}

fn agents_state_response(workspace_root: &str) -> Response<Body> {
    let agents_state = match mcp::agents_widget_state_payload(workspace_root) {
        Ok(value) => value,
        Err(error) => {
            return with_widget_action_cors(Response::builder())
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "ok": false, "error": error.to_string() }).to_string(),
                ))
                .unwrap();
        }
    };

    let mut payload = json!({ "ok": true });
    if let (Some(payload_obj), Some(agents_obj)) =
        (payload.as_object_mut(), agents_state.as_object())
    {
        for (key, value) in agents_obj {
            payload_obj.insert(key.clone(), value.clone());
        }
    }

    with_widget_action_cors(Response::builder())
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap()
}

async fn post_binagotchy_partner(
    State(s): State<ServerState>,
    Form(form): Form<HashMap<String, String>>,
) -> Response<Body> {
    let Some(seed) = form
        .get("seed")
        .map(|value| value.trim().to_ascii_lowercase())
    else {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": "missing seed" }).to_string(),
            ))
            .unwrap();
    };

    let parsed_seed = match parse_seed_hex(&seed) {
        Ok(value) => value,
        Err(error) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "ok": false, "error": error.to_string() }).to_string(),
                ))
                .unwrap();
        }
    };

    let mut app = s.app.lock().await;
    app.partner_binagotchy_seed = Some(seed.clone());
    app.mascot_seed = parsed_seed;
    app.mascot = crate::mascot::build_workspace_mascot(parsed_seed);
    let widget_mascot = crate::mascot::build_widget_mascot(parsed_seed);
    if let Err(error) = app.persist_state() {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": error.to_string() }).to_string(),
            ))
            .unwrap();
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "ok": true,
                "seed": seed,
                "message": "partner updated",
                "widgetMascot": widget_mascot
            })
            .to_string(),
        ))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, Mode, ToolMode};
    use axum::body::to_bytes;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::{Mutex, mpsc::unbounded_channel};

    fn unique_temp_path(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}"))
    }

    fn tool_call_body(name: &str, arguments: Value) -> Bytes {
        Bytes::from(
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": "req-tool",
                "method": "tools/call",
                "params": {
                    "name": name,
                    "arguments": arguments,
                }
            }))
            .expect("serialize tool call"),
        )
    }

    #[test]
    fn extract_turn_token_usage_reads_widget_payload_meta() {
        let result = json!({
            "structuredContent": {
                "schema": "catdesk.review.v1"
            },
            "_meta": {
                WIDGET_PAYLOAD_META_KEY: {
                    "schema": "catdesk.review.v1",
                    "turnTokenUsage": {
                        "inputTokens": 11,
                        "outputTokens": 22,
                        "totalTokens": 33
                    }
                }
            }
        });

        assert_eq!(extract_turn_token_usage(Some(&result)), Some((11, 22)));
    }

    #[test]
    fn attach_history_usage_updates_widget_payload_meta() {
        let mut result = Some(json!({
            "structuredContent": {
                "schema": "catdesk.review.v1"
            },
            "_meta": {
                "catdesk/widgetPayload": {
                    "schema": "catdesk.review.v1",
                    "turnTokenUsage": {
                        "inputTokens": 11,
                        "outputTokens": 22,
                        "totalTokens": 33
                    },
                    "toolCallCount": 4
                }
            }
        }));
        let usage_totals = UsageTotals {
            input_tokens: 120,
            output_tokens: 34,
            total_tokens: 154,
            tool_call_count: 7,
        };

        attach_history_usage(&mut result, &usage_totals);

        let structured = result
            .as_ref()
            .and_then(|value| value.get("structuredContent"))
            .expect("missing structuredContent");
        let widget_payload = result
            .as_ref()
            .and_then(|value| value.get("_meta"))
            .and_then(|meta| meta.get(WIDGET_PAYLOAD_META_KEY))
            .expect("missing widget payload");

        assert!(structured.get("historyTurnTokenUsage").is_none());
        assert!(structured.get("historyToolCallCount").is_none());
        assert_eq!(
            widget_payload
                .get("historyTurnTokenUsage")
                .and_then(|usage| usage.get("totalTokens"))
                .and_then(Value::as_u64),
            Some(154)
        );
        assert_eq!(
            widget_payload
                .get("historyToolCallCount")
                .and_then(Value::as_u64),
            Some(7)
        );
    }

    #[test]
    fn attach_catdesk_instruction_actions_injects_partner_and_urls() {
        let mut result = Some(json!({
            "structuredContent": {
                "schema": "catdesk.review.v1",
                "toolName": "catdesk_instruction"
            },
            "_meta": {
                WIDGET_PAYLOAD_META_KEY: {
                    "schema": "catdesk.review.v1",
                    "toolName": "catdesk_instruction",
                    "binagotchyCards": [{
                        "folder": "20260403T010203000Z_deadbeef",
                        "seed": "deadbeef"
                    }]
                }
            }
        }));

        attach_catdesk_instruction_actions(
            &mut result,
            Some("https://example.ngrok.app"),
            0xff,
            Some("deadbeef"),
        );

        let structured = result
            .as_ref()
            .and_then(|value| value.get("structuredContent"))
            .expect("missing structuredContent");
        let widget_payload = result
            .as_ref()
            .and_then(|value| value.get("_meta"))
            .and_then(|meta| meta.get(WIDGET_PAYLOAD_META_KEY))
            .expect("missing widget payload");
        let card = widget_payload
            .get("binagotchyCards")
            .and_then(Value::as_array)
            .and_then(|cards| cards.first())
            .expect("missing card");

        assert!(structured.get("binagotchyCards").is_none());
        assert!(structured.get("binagotchyApiBaseUrl").is_none());
        assert!(structured.get("partnerBinagotchySeed").is_none());
        assert_eq!(
            widget_payload
                .get("binagotchyApiBaseUrl")
                .and_then(Value::as_str),
            Some("https://example.ngrok.app/binagotchy")
        );
        assert_eq!(
            widget_payload
                .get("partnerBinagotchySeed")
                .and_then(Value::as_str),
            Some("deadbeef")
        );
        assert_eq!(
            widget_payload
                .get("agentsPathModeUrl")
                .and_then(Value::as_str),
            Some("https://example.ngrok.app/agents/path-mode")
        );
        assert_eq!(
            widget_payload
                .get("agentsPathStateUrl")
                .and_then(Value::as_str),
            Some("https://example.ngrok.app/agents/path-state")
        );
        assert!(widget_payload.get("widgetMascot").is_some());
        assert_eq!(card.get("isPartner").and_then(Value::as_bool), Some(true));
        assert_eq!(
            card.get("saveFolderUrl").and_then(Value::as_str),
            Some("https://example.ngrok.app/binagotchy/archive/20260403T010203000Z_deadbeef/save")
        );
        assert_eq!(
            card.get("setPartnerUrl").and_then(Value::as_str),
            Some("https://example.ngrok.app/binagotchy/partner")
        );
    }

    #[tokio::test]
    async fn post_mcp_accumulates_usage_from_widget_payload_meta() {
        let workspace_root = unique_temp_path("catdesk-post-mcp-workspace");
        let config_root = unique_temp_path("catdesk-post-mcp-config");
        let config_path = config_root.join("config.toml");
        std::fs::create_dir_all(&workspace_root).expect("create workspace");
        std::fs::create_dir_all(&config_root).expect("create config dir");
        std::fs::write(workspace_root.join("hello.txt"), "hello world\n").expect("write file");

        let app = AppState::new_for_test(
            8787,
            workspace_root.to_string_lossy().into_owned(),
            config_path.clone(),
        )
        .expect("create app state");
        let app_state = Arc::new(Mutex::new(app));
        let (ui_tx, _ui_rx) = unbounded_channel();
        let server_state = ServerState {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            app: app_state.clone(),
            devtools: None,
            ui_events: ui_tx,
        };

        let session = Session::new();
        let session_id = session.id.clone();
        server_state
            .sessions
            .lock()
            .await
            .insert(session_id.clone(), session);

        let mut headers = HeaderMap::new();
        headers.insert(
            "mcp-session-id",
            session_id.parse().expect("parse session header"),
        );

        let response = post_mcp(
            State(server_state),
            headers,
            tool_call_body("list_files", json!({ "path": "." })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        let sse_text = String::from_utf8(body.to_vec()).expect("response body utf8");
        let payload_line = sse_text
            .lines()
            .find(|line| line.starts_with("data: "))
            .expect("missing sse data line");
        let payload: Value = serde_json::from_str(
            payload_line
                .strip_prefix("data: ")
                .expect("strip sse prefix"),
        )
        .expect("parse sse json");

        let widget_payload = payload
            .get("result")
            .and_then(|result| result.get("_meta"))
            .and_then(|meta| meta.get(WIDGET_PAYLOAD_META_KEY))
            .expect("missing widget payload");
        let history_usage = widget_payload
            .get("historyTurnTokenUsage")
            .expect("missing history usage");
        assert!(
            history_usage
                .get("totalTokens")
                .and_then(Value::as_u64)
                .expect("history total tokens")
                > 0
        );
        assert_eq!(
            widget_payload
                .get("historyToolCallCount")
                .and_then(Value::as_u64),
            Some(1)
        );

        let app = app_state.lock().await;
        assert!(app.usage_totals.total_tokens > 0);
        assert_eq!(app.usage_totals.tool_call_count, 1);
        assert!(matches!(app.mode, Mode::Both));
        assert!(matches!(app.tool_mode, ToolMode::OneTool));
        drop(app);

        let _ = std::fs::remove_file(workspace_root.join("hello.txt"));
        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(workspace_root);
        let _ = std::fs::remove_dir_all(config_root);
    }
}

// ── POST /<slug>/mcp ────────────────────────────────────────

async fn post_mcp(
    State(s): State<ServerState>,
    headers: HeaderMap,
    body_bytes: Bytes,
) -> Response<Body> {
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

    let _ = s.ui_events.send(ServerUiEvent::IncrementRequestCount);

    let mut usage_totals = {
        let app = s.app.lock().await;
        app.usage_totals.clone()
    };

    let requests: Vec<Value> = if body.is_array() {
        body.as_array().unwrap().clone()
    } else {
        vec![body]
    };
    let request_summary: Vec<String> = requests.iter().map(summarize_request).collect();
    let request_flow_events: Vec<String> = requests.iter().map(request_flow_label).collect();
    let response_flow_events: Vec<String> = requests
        .iter()
        .filter(|r| r.get("id").is_some())
        .map(request_flow_label)
        .collect();

    let session_id_header = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let has_initialize = requests
        .iter()
        .any(|r| r.get("method").and_then(|m| m.as_str()) == Some("initialize"));
    let has_any_request = requests.iter().any(|r| r.get("id").is_some());

    let mut sessions = s.sessions.lock().await;

    let session_id: String;
    if has_initialize {
        if let Some(ref sid) = session_id_header {
            if sessions.contains_key(sid) {
                // important: ChatGPT MCP client will initialize 8 times
                // use same session id if the sid already exists
                session_id = sid.clone();
            } else if let Some(active_sid) = single_active_session_id(&sessions) {
                session_id = active_sid;
            } else {
                let new_session = Session::new();
                session_id = new_session.id.clone();
                sessions.insert(session_id.clone(), new_session);
            }
        } else if let Some(active_sid) = single_active_session_id(&sessions) {
            session_id = active_sid;
        } else {
            let new_session = Session::new();
            session_id = new_session.id.clone();
            sessions.insert(session_id.clone(), new_session);
        }
        let _ = s.ui_events.send(ServerUiEvent::SetRemoteConnected(true));
    } else if let Some(ref sid) = session_id_header {
        if !sessions.contains_key(sid) {
            drop(sessions);
            let mut app = s.app.lock().await;
            app.log(
                "WARN",
                format!("Session not found: {}", short_session_id(sid)),
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

    let mut responses: Vec<Value> = Vec::new();

    let _ = s.ui_events.send(ServerUiEvent::RecordSessionFlow {
        sid: session_id.clone(),
        events: request_flow_events.clone(),
        direction: FlowDirection::Forward,
    });

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

        let (
            workspace_root,
            mascot_seed,
            mode,
            tool_mode,
            set_catdesk_as_co_author,
            ngrok_url,
            partner_binagotchy_seed,
        ) = {
            let app = s.app.lock().await;
            (
                app.workspace_root.clone(),
                app.mascot_seed,
                app.mode,
                app.tool_mode,
                app.set_catdesk_as_co_author,
                app.ngrok_url.clone(),
                app.partner_binagotchy_seed.clone(),
            )
        };

        let session = sessions.get_mut(&session_id).unwrap();
        let tool_name = tool_name_from_jsonrpc_request(&req).to_string();
        if let Some(resp) = mcp::handle_request(
            &req,
            session,
            &workspace_root,
            mascot_seed,
            ngrok_url.as_deref(),
            mode,
            tool_mode,
            set_catdesk_as_co_author,
            &s.devtools,
        )
        .await
        {
            let mut resp = resp;
            if req.method == "tools/call" {
                if tool_name != "render_final_summary_widget" {
                    if let Some((input_tokens, output_tokens)) =
                        extract_turn_token_usage(resp.result.as_ref())
                    {
                        usage_totals.accumulate(input_tokens, output_tokens, 1);
                    }
                }
                attach_history_usage(&mut resp.result, &usage_totals);
                attach_catdesk_instruction_actions(
                    &mut resp.result,
                    ngrok_url.as_deref(),
                    mascot_seed,
                    partner_binagotchy_seed.as_deref(),
                );
            }
            responses.push(serde_json::to_value(resp).unwrap());
        }
    }

    {
        let session_count = sessions.len();
        let mut app = s.app.lock().await;
        if app.usage_totals != usage_totals {
            app.usage_totals = usage_totals.clone();
            app.persist_state_with_log();
        }
        let mcp_path = app.mcp_path();
        drop(app);
        let response_summary: Vec<String> = responses.iter().map(summarize_response).collect();
        let _ = s
            .ui_events
            .send(ServerUiEvent::SetSessionCount(session_count));
        let _ = s.ui_events.send(ServerUiEvent::RecordSessionFlow {
            sid: session_id.clone(),
            events: response_flow_events.clone(),
            direction: FlowDirection::Backward,
        });
        let _ = s.ui_events.send(ServerUiEvent::Log {
            level: "INFO",
            message: format!(
                "POST {mcp_path} request sid={} [{}]",
                short_session_id(&session_id),
                summarize_list(&request_summary, 6),
            ),
        });
        let _ = s.ui_events.send(ServerUiEvent::Log {
            level: "INFO",
            message: format!(
                "POST {mcp_path} response sid={} [{}]",
                short_session_id(&session_id),
                summarize_list(&response_summary, 6),
            ),
        });
    }

    drop(sessions);

    if !has_any_request {
        return Response::builder()
            .status(StatusCode::ACCEPTED)
            .header("mcp-session-id", &session_id)
            .body(Body::empty())
            .unwrap();
    }

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

// ── GET /<slug>/mcp — SSE ───────────────────────────────────

async fn get_mcp(State(s): State<ServerState>, headers: HeaderMap) -> Response<Body> {
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

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::convert::Infallible>>(16);
    let mcp_path = {
        let app = s.app.lock().await;
        app.mcp_path()
    };
    let _ = tx
        .send(Ok(format!("event: endpoint\ndata: {mcp_path}\n\n")))
        .await;

    let sid_clone = sid.clone();
    let ui_events = s.ui_events.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if tx.send(Ok(": keepalive\n\n".to_string())).await.is_err() {
                let _ = ui_events.send(ServerUiEvent::BeginSessionFlowClose {
                    sid: sid_clone.clone(),
                });
                let _ = ui_events.send(ServerUiEvent::Log {
                    level: "INFO",
                    message: format!("SSE stream closed: {}", short_session_id(&sid_clone)),
                });
                break;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header(header::CONNECTION, "keep-alive")
        .header("mcp-session-id", &sid)
        .body(Body::from_stream(stream))
        .unwrap()
}

// ── DELETE /<slug>/mcp ──────────────────────────────────────

async fn delete_mcp(State(s): State<ServerState>, headers: HeaderMap) -> Response<Body> {
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if let Some(sid) = session_id {
        let mut sessions = s.sessions.lock().await;
        if sessions.remove(&sid).is_some() {
            let session_count = sessions.len();
            let _ = s
                .ui_events
                .send(ServerUiEvent::SetSessionCount(session_count));
            let _ = s
                .ui_events
                .send(ServerUiEvent::BeginSessionFlowClose { sid: sid.clone() });
            if sessions.is_empty() {
                let _ = s.ui_events.send(ServerUiEvent::SetRemoteConnected(false));
            }
            let _ = s.ui_events.send(ServerUiEvent::Log {
                level: "INFO",
                message: format!("Session closed: {}", short_session_id(&sid)),
            });
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
