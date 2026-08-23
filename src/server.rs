use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Form, Path, State},
    http::{Response, StatusCode, header},
    response::Json,
    routing::{delete, get, post},
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, mpsc::UnboundedSender};

use crate::command_jobs::CommandJobManager;
use crate::devtools::DevtoolsBridge;
use crate::mcp::{self, JsonRpcRequest, WIDGET_PAYLOAD_META_KEY};
use crate::state::{
    AgentsPathMode, FlowDirection, ServerUiEvent, SharedState, ShowDetailMode, TokenStatsLayout,
    UsageTotals, parse_seed_hex, save_agents_path_mode, save_show_detail_mode,
    save_token_stats_layout,
};

const STATELESS_FLOW_ID: &str = "stateless";
const STATELESS_FLOW_LABEL: &str = "stateless";

#[derive(Clone)]
struct ServerState {
    app: SharedState,
    devtools: Option<Arc<Mutex<DevtoolsBridge>>>,
    command_jobs: CommandJobManager,
    ui_events: UnboundedSender<ServerUiEvent>,
    catdesk_instruction_called: Arc<AtomicBool>,
}

/// Build the axum router.
pub fn router(
    app_state: SharedState,
    devtools: Option<Arc<Mutex<DevtoolsBridge>>>,
    command_jobs: CommandJobManager,
    mcp_path: String,
    ui_events: UnboundedSender<ServerUiEvent>,
) -> Router {
    let state = ServerState {
        app: app_state,
        devtools,
        command_jobs,
        ui_events,
        catdesk_instruction_called: Arc::new(AtomicBool::new(false)),
    };
    let secret_prefix = mcp_path
        .strip_suffix("/mcp")
        .expect("MCP path must end with /mcp");
    let health_path = secret_prefix.to_string();
    let archive_save_path = format!("{secret_prefix}/binagotchy/archive/{{folder}}/save");
    let partner_path = format!("{secret_prefix}/binagotchy/partner");
    let agents_path_mode = format!("{secret_prefix}/agents/path-mode");
    let agents_path_state = format!("{secret_prefix}/agents/path-state");
    let token_stats_layout = format!("{secret_prefix}/layout/token-stats");
    let show_detail_mode = format!("{secret_prefix}/layout/show-detail");

    Router::new()
        .route(&health_path, get(health))
        .route(
            &archive_save_path,
            post(post_save_binagotchy_folder).options(options_binagotchy_archive_save),
        )
        .route(
            &partner_path,
            post(post_binagotchy_partner)
                .delete(delete_binagotchy_partner)
                .options(options_binagotchy_partner),
        )
        .route(
            &agents_path_mode,
            post(post_agents_path_mode).options(options_agents_path_mode),
        )
        .route(
            &agents_path_state,
            get(get_agents_path_state).options(options_agents_path_state),
        )
        .route(
            &token_stats_layout,
            post(post_token_stats_layout).options(options_token_stats_layout),
        )
        .route(
            &show_detail_mode,
            post(post_show_detail_mode).options(options_show_detail_mode),
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
    builder = builder.header(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        "GET, POST, DELETE, OPTIONS",
    );
    builder = builder.header(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "content-type, ngrok-skip-browser-warning",
    );
    builder = builder.header(header::CACHE_CONTROL, "no-store");
    builder
}

fn jsonrpc_error_response(status: StatusCode, code: i64, msg: &str) -> Response<Body> {
    let body = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "error": {"code": code, "message": msg}
    }))
    .unwrap();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
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

fn request_tool_arguments(req: &Value) -> Option<&serde_json::Map<String, Value>> {
    req.get("params")
        .and_then(|v| v.get("arguments"))
        .and_then(Value::as_object)
}

fn flow_file_name(path: &str) -> Option<String> {
    let trimmed = path.trim();
    let trimmed = trimmed.trim_end_matches(|c| c == '/' || c == '\\');
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn flow_argument_summary(tool: &str, arguments: &serde_json::Map<String, Value>) -> Option<String> {
    let (key, file_name_only) = match tool {
        "run_command" | "start_command" => ("command", false),
        "poll_command" | "cancel_command" => ("job_id", false),
        "read" | "write" | "edit" | "delete" => ("path", true),
        "search" => ("pattern", false),
        _ => return None,
    };
    let value = arguments.get(key)?.as_str()?;
    if file_name_only {
        return flow_file_name(value);
    }
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!compact.is_empty()).then_some(compact)
}

fn request_resource_uri(req: &Value) -> Option<&str> {
    req.get("params")
        .and_then(|v| v.get("uri"))
        .and_then(Value::as_str)
}

fn query_param_value<'a>(uri: &'a str, key: &str) -> Option<&'a str> {
    let query = uri.split_once('?')?.1;
    query.split('&').find_map(|part| {
        let (param_key, param_value) = part.split_once('=')?;
        if param_key == key {
            Some(param_value)
        } else {
            None
        }
    })
}

fn resource_read_flow_label(req: &Value) -> String {
    let Some(uri) = request_resource_uri(req) else {
        return "resources/read:?".to_string();
    };
    if let Some(tool_name) = query_param_value(uri, "toolName").filter(|value| !value.is_empty()) {
        return format!("resources/read:{tool_name}");
    }
    "resources/read:base".to_string()
}

fn request_flow_label(req: &Value) -> String {
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("<invalid-method>");
    if method == "tools/call" {
        let tool = request_tool_name(req).unwrap_or_else(|| "?".into());
        if let Some(summary) = request_tool_arguments(req)
            .and_then(|arguments| flow_argument_summary(&tool, arguments))
        {
            return format!("tools/call:{tool} › {summary}");
        }
        return format!("tools/call:{tool}");
    }
    if method == "resources/read" {
        return resource_read_flow_label(req);
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
    if let Some(result) = resp.get("result") {
        if let Some(protocol_version) = result.get("protocolVersion").and_then(Value::as_str) {
            return format!("id={id}:result protocolVersion={protocol_version}");
        }
        return format!("id={id}:result");
    }
    format!("id={id}:unknown")
}

fn extract_turn_token_usage(result: Option<&Value>) -> Option<(u64, u64)> {
    let usage = result
        .and_then(|value| value.get("_meta"))
        .and_then(Value::as_object)
        .and_then(|meta| meta.get(WIDGET_PAYLOAD_META_KEY))
        .and_then(Value::as_object)
        .and_then(|payload| payload.get("turnTokenUsage"))?;
    let tool_input_tokens = usage.get("inputTokens").and_then(Value::as_u64)?;
    let tool_output_tokens = usage.get("outputTokens").and_then(Value::as_u64)?;
    Some((tool_input_tokens, tool_output_tokens))
}

fn turn_token_usage_for_response(
    req: &JsonRpcRequest,
    result: Option<&Value>,
) -> Option<(u64, u64)> {
    let result = result?;
    Some(
        extract_turn_token_usage(Some(result))
            .unwrap_or_else(|| mcp::estimate_turn_token_counts(req, result)),
    )
}

fn attach_history_usage(result: &mut Option<Value>, usage_totals: &UsageTotals) {
    let Some(result_obj) = result.as_mut().and_then(Value::as_object_mut) else {
        return;
    };
    let history_usage = json!({
        "inputTokens": usage_totals.tool_input_tokens,
        "outputTokens": usage_totals.tool_output_tokens,
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

// ── GET /<slug> — health ───────────────────────────────────

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
    mcp_path: &str,
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

    let public_action_base_url = public_base_url
        .zip(mcp_path.strip_suffix("/mcp"))
        .map(|(base, secret_prefix)| format!("{base}{secret_prefix}"));
    let binagotchy_action_base_url = public_action_base_url
        .as_deref()
        .map(|base| format!("{base}/binagotchy"));
    widget_payload.insert(
        "binagotchyApiBaseUrl".to_string(),
        json!(binagotchy_action_base_url.clone().unwrap_or_default()),
    );
    widget_payload.insert(
        "agentsPathModeUrl".to_string(),
        json!(
            public_action_base_url
                .as_deref()
                .map(|base| format!("{base}/agents/path-mode"))
                .unwrap_or_default()
        ),
    );
    widget_payload.insert(
        "agentsPathStateUrl".to_string(),
        json!(
            public_action_base_url
                .as_deref()
                .map(|base| format!("{base}/agents/path-state"))
                .unwrap_or_default()
        ),
    );
    widget_payload.insert(
        "tokenStatsLayoutUrl".to_string(),
        json!(
            public_action_base_url
                .as_deref()
                .map(|base| format!("{base}/layout/token-stats"))
                .unwrap_or_default()
        ),
    );
    widget_payload.insert(
        "showDetailModeUrl".to_string(),
        json!(
            public_action_base_url
                .as_deref()
                .map(|base| format!("{base}/layout/show-detail"))
                .unwrap_or_default()
        ),
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

fn parse_token_stats_layout(value: &str) -> Option<TokenStatsLayout> {
    match value.trim() {
        "disable" => Some(TokenStatsLayout::Disable),
        "right" => Some(TokenStatsLayout::Right),
        "bottom" => Some(TokenStatsLayout::Bottom),
        _ => None,
    }
}

fn parse_show_detail_mode(value: &str) -> Option<ShowDetailMode> {
    match value.trim() {
        "disable" => Some(ShowDetailMode::Disable),
        "expanded" => Some(ShowDetailMode::Expanded),
        "collapsed" => Some(ShowDetailMode::Collapsed),
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

async fn post_token_stats_layout(
    State(_s): State<ServerState>,
    Form(form): Form<HashMap<String, String>>,
) -> Response<Body> {
    let Some(layout_raw) = form.get("layout").map(String::as_str) else {
        return with_widget_action_cors(Response::builder())
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": "missing layout" }).to_string(),
            ))
            .unwrap();
    };
    let Some(layout) = parse_token_stats_layout(layout_raw) else {
        return with_widget_action_cors(Response::builder())
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": "invalid layout" }).to_string(),
            ))
            .unwrap();
    };

    if let Err(error) = save_token_stats_layout(layout) {
        return with_widget_action_cors(Response::builder())
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": error.to_string() }).to_string(),
            ))
            .unwrap();
    }

    with_widget_action_cors(Response::builder())
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "ok": true,
                "tokenStatsLayout": layout.as_str(),
            })
            .to_string(),
        ))
        .unwrap()
}

async fn post_show_detail_mode(
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
    let Some(mode) = parse_show_detail_mode(mode_raw) else {
        return with_widget_action_cors(Response::builder())
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": "invalid mode" }).to_string(),
            ))
            .unwrap();
    };

    if let Err(error) = save_show_detail_mode(mode) {
        return with_widget_action_cors(Response::builder())
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": error.to_string() }).to_string(),
            ))
            .unwrap();
    }

    sync_show_detail_mode_state(&s, mode).await;

    with_widget_action_cors(Response::builder())
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "ok": true,
                "showDetailMode": mode.as_str(),
            })
            .to_string(),
        ))
        .unwrap()
}

async fn sync_show_detail_mode_state(s: &ServerState, mode: ShowDetailMode) {
    let mut app = s.app.lock().await;
    app.show_detail_mode = mode;
}

async fn options_agents_path_mode(State(_s): State<ServerState>) -> Response<Body> {
    with_widget_action_cors(Response::builder())
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap()
}

async fn options_token_stats_layout(State(_s): State<ServerState>) -> Response<Body> {
    with_widget_action_cors(Response::builder())
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap()
}

async fn options_show_detail_mode(State(_s): State<ServerState>) -> Response<Body> {
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

async fn delete_binagotchy_partner(State(s): State<ServerState>) -> Response<Body> {
    let random_seed = rand::random::<u64>();
    let random_seed_hex = format!("{random_seed:016x}");
    let mascot = crate::mascot::build_workspace_mascot(random_seed);
    let widget_mascot = crate::mascot::build_widget_mascot(random_seed);

    #[cfg(not(test))]
    if let Err(error) = crate::mascot::archive_startup_mascot(random_seed) {
        return with_widget_action_cors(Response::builder())
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": error.to_string() }).to_string(),
            ))
            .unwrap();
    }

    let mut app = s.app.lock().await;
    app.partner_binagotchy_seed = None;
    app.mascot_seed = random_seed;
    app.mascot = mascot;
    if let Err(error) = app.persist_state() {
        return with_widget_action_cors(Response::builder())
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "ok": false, "error": error.to_string() }).to_string(),
            ))
            .unwrap();
    }

    with_widget_action_cors(Response::builder())
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "ok": true,
                "seed": random_seed_hex,
                "message": "partner reset",
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
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::{Mutex, mpsc::unbounded_channel};

    #[test]
    fn tool_flow_label_includes_selected_argument_summary() {
        let run_command = json!({
            "method": "tools/call",
            "params": {
                "name": "run_command",
                "arguments": { "command": "cargo   build\n--release" }
            }
        });
        assert_eq!(
            request_flow_label(&run_command),
            "tools/call:run_command › cargo build --release"
        );

        let read = json!({
            "method": "tools/call",
            "params": {
                "name": "read",
                "arguments": { "path": "src/widget/catdesk_dashboard.html" }
            }
        });
        assert_eq!(
            request_flow_label(&read),
            "tools/call:read › catdesk_dashboard.html"
        );

        let edit = json!({
            "method": "tools/call",
            "params": {
                "name": "edit",
                "arguments": { "path": "src\\widget\\catdesk_dashboard.html" }
            }
        });
        assert_eq!(
            request_flow_label(&edit),
            "tools/call:edit › catdesk_dashboard.html"
        );

        let search = json!({
            "method": "tools/call",
            "params": {
                "name": "search",
                "arguments": { "pattern": "FLOW_ANIM_CELLS" }
            }
        });
        assert_eq!(
            request_flow_label(&search),
            "tools/call:search › FLOW_ANIM_CELLS"
        );
    }

    #[test]
    fn tool_flow_label_keeps_plain_name_for_unlisted_arguments() {
        let instruction = json!({
            "method": "tools/call",
            "params": {
                "name": "catdesk_instruction",
                "arguments": {}
            }
        });
        assert_eq!(
            request_flow_label(&instruction),
            "tools/call:catdesk_instruction"
        );
    }

    #[test]
    fn summarize_initialize_response_includes_protocol_version() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": {}
            }
        });

        assert_eq!(
            summarize_response(&response),
            "id=1:result protocolVersion=2025-11-25"
        );
    }

    fn unique_temp_path(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}"))
    }

    #[tokio::test]
    async fn show_detail_mode_sync_updates_app_state() {
        let workspace_root = unique_temp_path("catdesk-show-detail-sync-workspace");
        let config_root = unique_temp_path("catdesk-show-detail-sync-config");
        let config_path = config_root.join("config.toml");
        std::fs::create_dir_all(&workspace_root).expect("create workspace");
        std::fs::create_dir_all(&config_root).expect("create config dir");

        let app = AppState::new_for_test(
            8787,
            workspace_root.to_string_lossy().into_owned(),
            config_path.clone(),
        )
        .expect("create app state");
        let app_state = Arc::new(Mutex::new(app));
        let (ui_tx, _ui_rx) = unbounded_channel();
        let server_state = ServerState {
            app: app_state.clone(),
            devtools: None,
            command_jobs: CommandJobManager::new(),
            ui_events: ui_tx,
            catdesk_instruction_called: Arc::new(AtomicBool::new(true)),
        };

        sync_show_detail_mode_state(&server_state, ShowDetailMode::Disable).await;

        let app = app_state.lock().await;
        assert!(matches!(app.show_detail_mode, ShowDetailMode::Disable));
        drop(app);

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(workspace_root);
        let _ = std::fs::remove_dir_all(config_root);
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

    fn mcp_request_body(method: &str, params: Value) -> Bytes {
        Bytes::from(
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": "req-mcp",
                "method": method,
                "params": params,
            }))
            .expect("serialize MCP request"),
        )
    }

    async fn post_mcp_json_with_show_detail_mode(
        server_state: &ServerState,
        body: Bytes,
        show_detail_mode: ShowDetailMode,
    ) -> Value {
        let response =
            post_mcp_with_show_detail_mode(State(server_state.clone()), body, show_detail_mode)
                .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read MCP response body");
        serde_json::from_slice(&body).expect("parse MCP response")
    }

    #[tokio::test]
    async fn public_routes_require_secret_prefix() {
        let workspace_root = unique_temp_path("catdesk-route-guard");
        let config_root = unique_temp_path("catdesk-route-guard-config");
        let config_path = config_root.join("config.toml");
        std::fs::create_dir_all(&workspace_root).expect("create workspace");
        std::fs::create_dir_all(&config_root).expect("create config dir");

        let app = AppState::new_for_test(
            8787,
            workspace_root.to_string_lossy().into_owned(),
            config_path.clone(),
        )
        .expect("create app state");
        let app_state = Arc::new(Mutex::new(app));
        let (ui_tx, _ui_rx) = unbounded_channel();
        let router = router(
            app_state,
            None,
            CommandJobManager::new(),
            "/secret-slug/mcp".to_string(),
            ui_tx,
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("test listener address");
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve test router");
        });

        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        let unprefixed = [
            ("GET", "/"),
            ("GET", "/agents/path-state"),
            ("POST", "/agents/path-mode"),
            ("POST", "/layout/token-stats"),
            ("POST", "/layout/show-detail"),
            ("POST", "/binagotchy/partner"),
        ];
        for (method, path) in unprefixed {
            let request = match method {
                "GET" => client.get(format!("{base}{path}")),
                "POST" => client.post(format!("{base}{path}")),
                _ => unreachable!(),
            };
            let response = request.send().await.expect("send unprefixed request");
            assert_eq!(
                response.status(),
                reqwest::StatusCode::NOT_FOUND,
                "unprefixed route must be hidden: {method} {path}"
            );
        }

        let response = client
            .get(format!("{base}/secret-slug/agents/path-state"))
            .send()
            .await
            .expect("send prefixed request");
        assert_eq!(response.status(), reqwest::StatusCode::OK);

        server.abort();
        let _ = server.await;
        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(workspace_root);
        let _ = std::fs::remove_dir_all(config_root);
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
    fn turn_token_usage_falls_back_without_widget_payload() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "tools/call".to_string(),
            params: json!({
                "name": "read",
                "arguments": { "path": "notes.txt" }
            }),
        };
        let result = json!({
            "content": [],
            "structuredContent": {
                "toolName": "read",
                "text": "hello"
            }
        });

        assert!(extract_turn_token_usage(Some(&result)).is_none());
        let (input_tokens, output_tokens) =
            turn_token_usage_for_response(&req, Some(&result)).expect("missing usage");
        assert!(input_tokens > 0);
        assert!(output_tokens > 0);
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
            tool_input_tokens: 120,
            tool_output_tokens: 34,
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
            "/secret-slug/mcp",
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
            Some("https://example.ngrok.app/secret-slug/binagotchy")
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
            Some("https://example.ngrok.app/secret-slug/agents/path-mode")
        );
        assert_eq!(
            widget_payload
                .get("agentsPathStateUrl")
                .and_then(Value::as_str),
            Some("https://example.ngrok.app/secret-slug/agents/path-state")
        );
        assert_eq!(
            widget_payload
                .get("tokenStatsLayoutUrl")
                .and_then(Value::as_str),
            Some("https://example.ngrok.app/secret-slug/layout/token-stats")
        );
        assert_eq!(
            widget_payload
                .get("showDetailModeUrl")
                .and_then(Value::as_str),
            Some("https://example.ngrok.app/secret-slug/layout/show-detail")
        );
        assert!(widget_payload.get("widgetMascot").is_some());
        assert_eq!(card.get("isPartner").and_then(Value::as_bool), Some(true));
        assert_eq!(
            card.get("saveFolderUrl").and_then(Value::as_str),
            Some(
                "https://example.ngrok.app/secret-slug/binagotchy/archive/20260403T010203000Z_deadbeef/save"
            )
        );
        assert_eq!(
            card.get("setPartnerUrl").and_then(Value::as_str),
            Some("https://example.ngrok.app/secret-slug/binagotchy/partner")
        );
    }

    #[tokio::test]
    async fn background_command_survives_separate_stateless_http_requests() {
        let workspace_root = unique_temp_path("catdesk-post-mcp-command-job");
        let config_root = unique_temp_path("catdesk-post-mcp-command-job-config");
        let config_path = config_root.join("config.toml");
        std::fs::create_dir_all(&workspace_root).expect("create workspace");
        std::fs::create_dir_all(&config_root).expect("create config dir");

        let app = AppState::new_for_test(
            8787,
            workspace_root.to_string_lossy().into_owned(),
            config_path.clone(),
        )
        .expect("create app state");
        let app_state = Arc::new(Mutex::new(app));
        let (ui_tx, _ui_rx) = unbounded_channel();
        let command_jobs = CommandJobManager::new();
        let server_state = ServerState {
            app: app_state,
            devtools: None,
            command_jobs: command_jobs.clone(),
            ui_events: ui_tx,
            catdesk_instruction_called: Arc::new(AtomicBool::new(true)),
        };
        let command = if cfg!(windows) {
            "Start-Sleep -Milliseconds 250; Write-Output http-job-done"
        } else {
            "sleep 0.25; printf 'http-job-done\\n'"
        };

        let start_response = post_mcp(
            State(server_state.clone()),
            tool_call_body(
                "start_command",
                json!({ "command": command, "timeout": 5_000 }),
            ),
        )
        .await;
        assert_eq!(start_response.status(), StatusCode::OK);
        let start_body = to_bytes(start_response.into_body(), usize::MAX)
            .await
            .expect("read start response");
        let start_payload: Value =
            serde_json::from_slice(&start_body).expect("parse start response");
        let job_id = start_payload
            .get("result")
            .and_then(|result| result.get("structuredContent"))
            .and_then(|structured| structured.get("jobId"))
            .and_then(Value::as_str)
            .expect("start response job id")
            .to_string();

        let mut cursor = 0u64;
        let mut seen = String::new();
        let mut terminal = None;
        for _ in 0..20 {
            let poll_response = post_mcp(
                State(server_state.clone()),
                tool_call_body(
                    "poll_command",
                    json!({ "job_id": job_id, "after": cursor, "wait_ms": 250 }),
                ),
            )
            .await;
            assert_eq!(poll_response.status(), StatusCode::OK);
            let poll_body = to_bytes(poll_response.into_body(), usize::MAX)
                .await
                .expect("read poll response");
            let poll_payload: Value =
                serde_json::from_slice(&poll_body).expect("parse poll response");
            let structured = poll_payload
                .get("result")
                .and_then(|result| result.get("structuredContent"))
                .expect("poll structured content");
            if let Some(events) = structured.get("events").and_then(Value::as_array) {
                for event in events {
                    if let Some(text) = event.get("text").and_then(Value::as_str) {
                        seen.push_str(text);
                    }
                }
            }
            cursor = structured
                .get("nextCursor")
                .and_then(Value::as_u64)
                .unwrap_or(cursor);
            let state = structured.get("state").and_then(Value::as_str);
            let has_more = structured
                .get("hasMoreOutput")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if state == Some("succeeded") && !has_more {
                terminal = Some(poll_payload);
                break;
            }
        }

        let terminal = terminal.expect("background job did not finish across HTTP requests");
        let structured = terminal
            .get("result")
            .and_then(|result| result.get("structuredContent"))
            .expect("terminal structured content");
        assert_eq!(
            structured.get("state").and_then(Value::as_str),
            Some("succeeded")
        );
        assert_eq!(
            structured.get("commandSuccess").and_then(Value::as_bool),
            Some(true)
        );
        assert!(seen.contains("http-job-done"));

        command_jobs.cancel_all().await;
        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(workspace_root);
        let _ = std::fs::remove_dir_all(config_root);
    }

    #[tokio::test]
    async fn catdesk_instruction_unlocks_tools_until_process_restart() {
        let workspace_root = unique_temp_path("catdesk-instruction-gate-workspace");
        let config_root = unique_temp_path("catdesk-instruction-gate-config");
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
        let instruction_called = Arc::new(AtomicBool::new(false));
        let server_state = ServerState {
            app: app_state,
            devtools: None,
            command_jobs: CommandJobManager::new(),
            ui_events: ui_tx,
            catdesk_instruction_called: instruction_called.clone(),
        };

        let blocked_response = post_mcp(
            State(server_state.clone()),
            tool_call_body("read", json!({ "path": "hello.txt" })),
        )
        .await;
        assert_eq!(blocked_response.status(), StatusCode::OK);
        let blocked_body = to_bytes(blocked_response.into_body(), usize::MAX)
            .await
            .expect("read blocked response body");
        let blocked_payload: Value =
            serde_json::from_slice(&blocked_body).expect("parse blocked response");
        assert_eq!(
            blocked_payload
                .get("result")
                .and_then(|result| result.get("isError"))
                .and_then(Value::as_bool),
            None
        );
        assert_eq!(
            blocked_payload
                .get("result")
                .and_then(|result| result.get("structuredContent"))
                .and_then(|structured| structured.get("success"))
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            blocked_payload
                .get("result")
                .and_then(|result| result.get("structuredContent"))
                .and_then(|structured| structured.get("errorCode"))
                .and_then(Value::as_str),
            Some("CATDESK_INSTRUCTION_REQUIRED")
        );
        assert!(!instruction_called.load(Ordering::Acquire));

        let instruction_response = post_mcp(
            State(server_state.clone()),
            tool_call_body("catdesk_instruction", json!({})),
        )
        .await;
        assert_eq!(instruction_response.status(), StatusCode::OK);
        let instruction_body = to_bytes(instruction_response.into_body(), usize::MAX)
            .await
            .expect("read instruction response body");
        let instruction_payload: Value =
            serde_json::from_slice(&instruction_body).expect("parse instruction response");
        assert_ne!(
            instruction_payload
                .get("result")
                .and_then(|result| result.get("isError"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert!(instruction_called.load(Ordering::Acquire));

        let allowed_response = post_mcp(
            State(server_state),
            tool_call_body("read", json!({ "path": "hello.txt" })),
        )
        .await;
        assert_eq!(allowed_response.status(), StatusCode::OK);
        let allowed_body = to_bytes(allowed_response.into_body(), usize::MAX)
            .await
            .expect("read allowed response body");
        let allowed_payload: Value =
            serde_json::from_slice(&allowed_body).expect("parse allowed response");
        assert_eq!(
            allowed_payload
                .get("result")
                .and_then(|result| result.get("structuredContent"))
                .and_then(|structured| structured.get("text"))
                .and_then(Value::as_str),
            Some("hello world\n")
        );

        let _ = std::fs::remove_file(workspace_root.join("hello.txt"));
        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(workspace_root);
        let _ = std::fs::remove_dir_all(config_root);
    }

    #[tokio::test]
    async fn disabled_show_detail_mode_hides_widgets_across_mcp_flow() {
        let workspace_root = unique_temp_path("catdesk-disable-mcp-flow-workspace");
        let config_root = unique_temp_path("catdesk-disable-mcp-flow-config");
        let config_path = config_root.join("config.toml");
        std::fs::create_dir_all(&workspace_root).expect("create workspace");
        std::fs::create_dir_all(&config_root).expect("create config dir");
        std::fs::write(workspace_root.join("hello.txt"), "hello world\n")
            .expect("write workspace file");

        let app = AppState::new_for_test(
            8787,
            workspace_root.to_string_lossy().into_owned(),
            config_path.clone(),
        )
        .expect("create app state");
        let app_state = Arc::new(Mutex::new(app));
        let (ui_tx, _ui_rx) = unbounded_channel();
        let instruction_called = Arc::new(AtomicBool::new(false));
        let server_state = ServerState {
            app: app_state.clone(),
            devtools: None,
            command_jobs: CommandJobManager::new(),
            ui_events: ui_tx,
            catdesk_instruction_called: instruction_called.clone(),
        };

        let tools_list = post_mcp_json_with_show_detail_mode(
            &server_state,
            mcp_request_body("tools/list", json!({})),
            ShowDetailMode::Disable,
        )
        .await;
        let tools = tools_list
            .get("result")
            .and_then(|result| result.get("tools"))
            .and_then(Value::as_array)
            .expect("missing tools");
        assert!(!tools.is_empty());
        for tool in tools {
            assert!(
                tool.get("_meta")
                    .and_then(|meta| meta.get("openai/outputTemplate"))
                    .is_none(),
                "Disable must not advertise a Widget output template"
            );
        }

        let blocked = post_mcp_json_with_show_detail_mode(
            &server_state,
            tool_call_body("read", json!({ "path": "hello.txt" })),
            ShowDetailMode::Disable,
        )
        .await;
        assert_eq!(
            blocked
                .get("result")
                .and_then(|result| result.get("structuredContent"))
                .and_then(|structured| structured.get("errorCode"))
                .and_then(Value::as_str),
            Some("CATDESK_INSTRUCTION_REQUIRED")
        );
        assert!(
            blocked
                .get("result")
                .and_then(|result| result.get("_meta"))
                .and_then(|meta| meta.get(WIDGET_PAYLOAD_META_KEY))
                .is_none()
        );

        let instruction = post_mcp_json_with_show_detail_mode(
            &server_state,
            tool_call_body("catdesk_instruction", json!({})),
            ShowDetailMode::Disable,
        )
        .await;
        assert!(
            instruction
                .get("result")
                .and_then(|result| result.get("structuredContent"))
                .and_then(|structured| structured.get("instructionText"))
                .and_then(Value::as_str)
                .is_some()
        );
        assert!(
            instruction
                .get("result")
                .and_then(|result| result.get("_meta"))
                .and_then(|meta| meta.get(WIDGET_PAYLOAD_META_KEY))
                .is_none()
        );
        assert!(instruction_called.load(Ordering::Acquire));

        let allowed = post_mcp_json_with_show_detail_mode(
            &server_state,
            tool_call_body("read", json!({ "path": "hello.txt" })),
            ShowDetailMode::Disable,
        )
        .await;
        assert_eq!(
            allowed
                .get("result")
                .and_then(|result| result.get("structuredContent"))
                .and_then(|structured| structured.get("text"))
                .and_then(Value::as_str),
            Some("hello world\n")
        );
        assert!(
            allowed
                .get("result")
                .and_then(|result| result.get("_meta"))
                .and_then(|meta| meta.get(WIDGET_PAYLOAD_META_KEY))
                .is_none()
        );

        let resources_list = post_mcp_json_with_show_detail_mode(
            &server_state,
            mcp_request_body("resources/list", json!({})),
            ShowDetailMode::Disable,
        )
        .await;
        assert_eq!(
            resources_list
                .get("result")
                .and_then(|result| result.get("resources"))
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );

        let resources_read = post_mcp_json_with_show_detail_mode(
            &server_state,
            mcp_request_body(
                "resources/read",
                json!({ "uri": "ui://widget/catdesk-dashboard.html" }),
            ),
            ShowDetailMode::Disable,
        )
        .await;
        assert_eq!(
            resources_read
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_i64),
            Some(-32602)
        );

        let app = app_state.lock().await;
        let usage = app.all_time_usage_totals();
        assert!(usage.total_tokens > 0);
        assert_eq!(usage.tool_call_count, 3);
        assert_eq!(app.session_usage_totals, usage);
        drop(app);

        let _ = std::fs::remove_file(workspace_root.join("hello.txt"));
        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(workspace_root);
        let _ = std::fs::remove_dir_all(config_root);
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
            app: app_state.clone(),
            devtools: None,
            command_jobs: CommandJobManager::new(),
            ui_events: ui_tx,
            catdesk_instruction_called: Arc::new(AtomicBool::new(true)),
        };

        let response = post_mcp(
            State(server_state),
            tool_call_body("run_command", json!({ "command": "find ." })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        let payload: Value = serde_json::from_slice(&body).expect("parse json body");

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
        let all_time_usage = app.all_time_usage_totals();
        assert!(all_time_usage.total_tokens > 0);
        assert_eq!(all_time_usage.tool_call_count, 1);
        assert_eq!(app.session_usage_totals, all_time_usage);
        assert!(matches!(app.mode, Mode::Both));
        assert!(matches!(app.tool_mode, ToolMode::MultiTools));
        drop(app);

        let _ = std::fs::remove_file(workspace_root.join("hello.txt"));
        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(workspace_root);
        let _ = std::fs::remove_dir_all(config_root);
    }
}

// ── POST /<slug>/mcp ────────────────────────────────────────

async fn post_mcp(State(s): State<ServerState>, body_bytes: Bytes) -> Response<Body> {
    post_mcp_inner(State(s), body_bytes, None).await
}

#[cfg(test)]
async fn post_mcp_with_show_detail_mode(
    state: State<ServerState>,
    body_bytes: Bytes,
    show_detail_mode: ShowDetailMode,
) -> Response<Body> {
    post_mcp_inner(state, body_bytes, Some(show_detail_mode)).await
}

async fn post_mcp_inner(
    State(s): State<ServerState>,
    body_bytes: Bytes,
    show_detail_mode: Option<ShowDetailMode>,
) -> Response<Body> {
    let body: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(e) => {
            return jsonrpc_error_response(
                StatusCode::BAD_REQUEST,
                -32700,
                &format!("Parse error: {e}"),
            );
        }
    };
    if !body.is_object() {
        return jsonrpc_error_response(
            StatusCode::BAD_REQUEST,
            -32600,
            "Invalid request: expected a single JSON-RPC message object",
        );
    }

    let _ = s.ui_events.send(ServerUiEvent::IncrementRequestCount);
    let _ = s.ui_events.send(ServerUiEvent::SetRemoteConnected(true));

    let has_method = body.get("method").and_then(Value::as_str).is_some();
    if !has_method {
        let mcp_path = {
            let app = s.app.lock().await;
            app.mcp_path()
        };
        let _ = s.ui_events.send(ServerUiEvent::Log {
            level: "INFO",
            message: format!(
                "POST {mcp_path} flow={STATELESS_FLOW_LABEL} accepted non-request JSON-RPC message"
            ),
        });
        return Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Body::empty())
            .unwrap();
    }

    let request_summary = summarize_request(&body);
    let request_flow_event = request_flow_label(&body);

    let _ = s.ui_events.send(ServerUiEvent::RecordFlow {
        flow_id: STATELESS_FLOW_ID.to_string(),
        events: vec![request_flow_event.clone()],
        direction: FlowDirection::Forward,
    });

    let req: JsonRpcRequest = match serde_json::from_value(body.clone()) {
        Ok(r) => r,
        Err(e) => {
            return jsonrpc_error_response(
                StatusCode::BAD_REQUEST,
                -32600,
                &format!("Invalid request: {e}"),
            );
        }
    };

    let (
        workspace_root,
        mascot_seed,
        mode,
        tool_mode,
        set_catdesk_as_co_author,
        ngrok_url,
        mcp_path,
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
            app.mcp_path(),
            app.partner_binagotchy_seed.clone(),
        )
    };

    let response = match show_detail_mode {
        Some(show_detail_mode) => {
            mcp::handle_request_with_show_detail_mode(
                &req,
                &workspace_root,
                mascot_seed,
                ngrok_url.as_deref(),
                mode,
                tool_mode,
                set_catdesk_as_co_author,
                s.catdesk_instruction_called.load(Ordering::Acquire),
                &s.command_jobs,
                &s.devtools,
                show_detail_mode,
            )
            .await
        }
        None => {
            mcp::handle_request(
                &req,
                &workspace_root,
                mascot_seed,
                ngrok_url.as_deref(),
                mode,
                tool_mode,
                set_catdesk_as_co_author,
                s.catdesk_instruction_called.load(Ordering::Acquire),
                &s.command_jobs,
                &s.devtools,
            )
            .await
        }
    };

    let mut response_json: Option<Value> = None;
    if let Some(resp) = response {
        let mut resp = resp;
        if req.method == "tools/call" {
            if req.params.get("name").and_then(Value::as_str) == Some("catdesk_instruction")
                && resp.error.is_none()
                && resp.result.as_ref().is_some_and(|result| {
                    result.get("isError").and_then(Value::as_bool) != Some(true)
                })
            {
                s.catdesk_instruction_called.store(true, Ordering::Release);
            }
            let turn_token_usage = turn_token_usage_for_response(&req, resp.result.as_ref());
            let usage_totals = {
                let mut app = s.app.lock().await;
                if let Some((tool_input_tokens, tool_output_tokens)) = turn_token_usage {
                    app.record_turn_usage(tool_input_tokens, tool_output_tokens);
                    let _ = s.ui_events.send(ServerUiEvent::RecordTurnUsage {
                        flow_id: STATELESS_FLOW_ID.to_string(),
                        tool_input_tokens,
                        tool_output_tokens,
                    });
                    app.persist_state_with_log();
                }
                app.all_time_usage_totals()
            };
            attach_history_usage(&mut resp.result, &usage_totals);
            attach_catdesk_instruction_actions(
                &mut resp.result,
                ngrok_url.as_deref(),
                &mcp_path,
                mascot_seed,
                partner_binagotchy_seed.as_deref(),
            );
        }
        response_json = Some(serde_json::to_value(resp).unwrap());
    }

    {
        let app = s.app.lock().await;
        let mcp_path = app.mcp_path();
        drop(app);
        if req.id.is_some() {
            let _ = s.ui_events.send(ServerUiEvent::RecordFlow {
                flow_id: STATELESS_FLOW_ID.to_string(),
                events: vec![request_flow_event.clone()],
                direction: FlowDirection::Backward,
            });
        }
        let _ = s.ui_events.send(ServerUiEvent::Log {
            level: "INFO",
            message: format!(
                "POST {mcp_path} flow={STATELESS_FLOW_LABEL} [{}]",
                request_summary,
            ),
        });
        if let Some(ref resp_json) = response_json {
            let response_summary = summarize_response(resp_json);
            let _ = s.ui_events.send(ServerUiEvent::Log {
                level: "INFO",
                message: format!(
                    "POST {mcp_path} flow={STATELESS_FLOW_LABEL} response [{response_summary}]"
                ),
            });
        }
    }

    if req.id.is_none() {
        return Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Body::empty())
            .unwrap();
    }

    let Some(response_json) = response_json else {
        return jsonrpc_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            -32603,
            "Internal error: request did not produce a JSON-RPC response",
        );
    };
    let response_body = serde_json::to_string(&response_json).unwrap();

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(response_body))
        .unwrap()
}

// ── GET /<slug>/mcp — pure HTTP mode (no SSE) ───────────────

async fn get_mcp() -> Response<Body> {
    Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"GET SSE stream is disabled in pure HTTP mode"}}"#,
        ))
        .unwrap()
}

// ── DELETE /<slug>/mcp ──────────────────────────────────────

async fn delete_mcp(State(s): State<ServerState>) -> Response<Body> {
    let _ = s.ui_events.send(ServerUiEvent::SetRemoteConnected(false));
    let _ = s.ui_events.send(ServerUiEvent::BeginFlowClose {
        flow_id: STATELESS_FLOW_ID.to_string(),
    });
    let _ = s.ui_events.send(ServerUiEvent::Log {
        level: "INFO",
        message: "DELETE mcp endpoint: stateless reset".to_string(),
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"status":"ok"}"#))
        .unwrap()
}
