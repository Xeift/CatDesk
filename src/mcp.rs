use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::command;

const SERVER_NAME: &str = "xeduck-v4-mcp-server";
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

/// Returns None for notifications (no response needed).
pub async fn handle_request(
    req: &JsonRpcRequest,
    session: &mut Session,
    workspace_root: &str,
) -> Option<JsonRpcResponse> {
    match req.method.as_str() {
        "initialize" => Some(handle_initialize(req, session)),
        // All notifications — no response
        m if m.starts_with("notifications/") => {
            if m == "notifications/initialized" {
                session.initialized = true;
            }
            None
        }
        "tools/list" => Some(handle_tools_list(req)),
        "tools/call" => Some(handle_tools_call(req, workspace_root).await),
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
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION,
            }
        }),
    )
}

fn handle_tools_list(req: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(
        req.id.clone(),
        json!({
            "tools": [
                {
                    "name": "run_command",
                    "title": "Run command",
                    "description": "Execute a shell command inside the workspace root. Returns stdout and stderr.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "command": {
                                "type": "string",
                                "description": "The shell command to execute"
                            },
                            "cwd": {
                                "type": "string",
                                "description": "Working directory relative to workspace root or absolute path within it"
                            },
                            "timeout": {
                                "type": "number",
                                "description": "Timeout in milliseconds. Clamped to 120000."
                            }
                        },
                        "required": ["command"]
                    },
                    "annotations": {
                        "readOnlyHint": false,
                        "openWorldHint": true,
                        "destructiveHint": true
                    }
                }
            ]
        }),
    )
}

async fn handle_tools_call(req: &JsonRpcRequest, workspace_root: &str) -> JsonRpcResponse {
    let params = &req.params;
    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if tool_name != "run_command" {
        return JsonRpcResponse::error(
            req.id.clone(),
            -32602,
            format!("Unknown tool: {tool_name}"),
        );
    }

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
            json!({
                "content": [{"type": "text", "text": output}],
            }),
        )
    } else {
        JsonRpcResponse::success(
            req.id.clone(),
            json!({
                "content": [{"type": "text", "text": output}],
                "isError": true,
            }),
        )
    }
}
