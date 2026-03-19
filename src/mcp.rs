use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::command;
use crate::devtools::DevtoolsBridge;
use crate::state::Mode;

const SERVER_NAME: &str = "mcp3000";
const SERVER_VERSION: &str = "4.0.0";
const PROTOCOL_VERSION: &str = "2025-03-26";

// ── JSON-RPC types ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }
    pub fn error(id: Option<Value>, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

// ── Session ─────────────────────────────────────────────────

#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub initialized: bool,
}

impl Session {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            initialized: false,
        }
    }
}

// ── Handler ─────────────────────────────────────────────────

pub async fn handle_request(
    req: &JsonRpcRequest,
    session: &mut Session,
    workspace_root: &str,
    mode: Mode,
    devtools: &Option<Arc<Mutex<DevtoolsBridge>>>,
) -> Option<JsonRpcResponse> {
    match req.method.as_str() {
        "initialize" => {
            // Also initialize devtools bridge if available
            if let Some(bridge) = devtools {
                let init_req = json!({
                    "jsonrpc": "2.0",
                    "id": "dt-init",
                    "method": "initialize",
                    "params": {
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {},
                        "clientInfo": {"name": "mcp3000-bridge", "version": SERVER_VERSION}
                    }
                });
                let mut b = bridge.lock().await;
                let _ = b.request(&init_req).await;
                // Send initialized notification
                let _ = b
                    .notify(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                    .await;
            }
            Some(handle_initialize(req, session))
        }
        m if m.starts_with("notifications/") => {
            if m == "notifications/initialized" {
                session.initialized = true;
            }
            None
        }
        "tools/list" => Some(handle_tools_list(req, mode, devtools).await),
        "tools/call" => Some(handle_tools_call(req, workspace_root, mode, devtools).await),
        "ping" => Some(JsonRpcResponse::success(req.id.clone(), json!({}))),
        _ => Some(JsonRpcResponse::error(
            req.id.clone(),
            -32601,
            format!("Method not found: {}", req.method),
        )),
    }
}

fn handle_initialize(req: &JsonRpcRequest, session: &mut Session) -> JsonRpcResponse {
    session.initialized = true;
    JsonRpcResponse::success(
        req.id.clone(),
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
        }),
    )
}

// ── tools/list ──────────────────────────────────────────────

async fn handle_tools_list(
    req: &JsonRpcRequest,
    mode: Mode,
    devtools: &Option<Arc<Mutex<DevtoolsBridge>>>,
) -> JsonRpcResponse {
    let mut tools: Vec<Value> = Vec::new();

    // Computer tools
    if mode.computer_enabled() {
        tools.push(json!({
            "name": "run_command",
            "title": "Run command",
            "description": "Execute a shell command inside the workspace root. Returns stdout and stderr.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The shell command to execute" },
                    "cwd": { "type": "string", "description": "Working directory relative to workspace root or absolute path within it" },
                    "timeout": { "type": "number", "description": "Timeout in milliseconds. Clamped to 120000." }
                },
                "required": ["command"]
            },
            "annotations": { "readOnlyHint": false, "openWorldHint": true, "destructiveHint": true }
        }));
    }

    // Browser tools — get from devtools bridge
    if mode.browser_enabled() {
        if let Some(bridge) = devtools {
            let list_req = json!({
                "jsonrpc": "2.0",
                "id": "dt-tools-list",
                "method": "tools/list",
                "params": {}
            });
            let mut b = bridge.lock().await;
            if let Ok(resp) = b.request(&list_req).await {
                if let Some(result) = resp.get("result") {
                    if let Some(dt_tools) = result.get("tools").and_then(|t| t.as_array()) {
                        tools.extend(dt_tools.iter().cloned());
                    }
                }
            }
        }
    }

    JsonRpcResponse::success(req.id.clone(), json!({ "tools": tools }))
}

// ── tools/call ──────────────────────────────────────────────

async fn handle_tools_call(
    req: &JsonRpcRequest,
    workspace_root: &str,
    mode: Mode,
    devtools: &Option<Arc<Mutex<DevtoolsBridge>>>,
) -> JsonRpcResponse {
    let params = &req.params;
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");

    // run_command is handled locally
    if tool_name == "run_command" && mode.computer_enabled() {
        return handle_run_command(req, workspace_root).await;
    }

    // Everything else → forward to devtools bridge
    if mode.browser_enabled() {
        if let Some(bridge) = devtools {
            let forward_req = json!({
                "jsonrpc": "2.0",
                "id": req.id,
                "method": "tools/call",
                "params": params
            });
            let mut b = bridge.lock().await;
            match b.request(&forward_req).await {
                Ok(resp) => {
                    if let Some(result) = resp.get("result") {
                        return JsonRpcResponse::success(req.id.clone(), result.clone());
                    }
                    if let Some(error) = resp.get("error") {
                        let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-32000);
                        let msg = error
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("Unknown error");
                        return JsonRpcResponse::error(req.id.clone(), code, msg.to_string());
                    }
                }
                Err(e) => {
                    return JsonRpcResponse::success(
                        req.id.clone(),
                        json!({
                            "content": [{"type": "text", "text": format!("DevTools bridge error: {e}")}],
                            "isError": true,
                        }),
                    );
                }
            }
        }
    }

    JsonRpcResponse::error(req.id.clone(), -32602, format!("Unknown tool: {tool_name}"))
}

async fn handle_run_command(req: &JsonRpcRequest, workspace_root: &str) -> JsonRpcResponse {
    let params = &req.params;
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    let cmd = match arguments.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return JsonRpcResponse::success(
                req.id.clone(),
                json!({
                    "content": [{"type": "text", "text": "Missing required parameter: command"}],
                    "isError": true,
                }),
            );
        }
    };

    let cwd_input = arguments.get("cwd").and_then(|v| v.as_str());
    let timeout_ms = arguments.get("timeout").and_then(|v| v.as_u64());

    let cwd = match command::resolve_workspace_path(workspace_root, cwd_input) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse::success(
                req.id.clone(),
                json!({
                    "content": [{"type": "text", "text": format!("code: PATH_OUTSIDE_WORKSPACE\nmessage: {e}")}],
                    "isError": true,
                }),
            );
        }
    };

    let effective_timeout = command::clamp_timeout(timeout_ms);
    let result = command::run_command(cmd, &cwd, effective_timeout).await;
    let output = command::format_result(&result);

    if result.success {
        JsonRpcResponse::success(
            req.id.clone(),
            json!({ "content": [{"type": "text", "text": output}] }),
        )
    } else {
        JsonRpcResponse::success(
            req.id.clone(),
            json!({ "content": [{"type": "text", "text": output}], "isError": true }),
        )
    }
}
