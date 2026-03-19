use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::command;
use crate::devtools::DevtoolsBridge;
use crate::state::{Mode, ToolMode};
use crate::workspace_tools;

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
    tool_mode: ToolMode,
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
        "tools/list" => Some(handle_tools_list(req, mode, tool_mode, devtools).await),
        "tools/call" => {
            Some(handle_tools_call(req, workspace_root, mode, tool_mode, devtools).await)
        }
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
    tool_mode: ToolMode,
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

        if tool_mode.multi_enabled() {
            tools.push(json!({
                "name": "read_file",
                "title": "Read file",
                "description": "Read a text file from workspace.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path relative to workspace root or absolute path within it" }
                    },
                    "required": ["path"]
                },
                "annotations": { "readOnlyHint": true, "openWorldHint": false, "destructiveHint": false }
            }));
            tools.push(json!({
                "name": "write_file",
                "title": "Write file",
                "description": "Create or overwrite a file in workspace.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" },
                        "create_dirs": { "type": "boolean", "description": "Create parent directories if missing" }
                    },
                    "required": ["path", "content"]
                },
                "annotations": { "readOnlyHint": false, "openWorldHint": false, "destructiveHint": true }
            }));
            tools.push(json!({
                "name": "append_file",
                "title": "Append file",
                "description": "Append text to an existing file (or create file).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" },
                        "create_dirs": { "type": "boolean", "description": "Create parent directories if missing" }
                    },
                    "required": ["path", "content"]
                },
                "annotations": { "readOnlyHint": false, "openWorldHint": false, "destructiveHint": true }
            }));
            tools.push(json!({
                "name": "list_files",
                "title": "List files",
                "description": "Recursively list files and directories under workspace.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory path (default: workspace root)" },
                        "include_hidden": { "type": "boolean", "description": "Include dotfiles and dot-directories" },
                        "limit": { "type": "number", "description": "Max listed items (1..1000)" }
                    }
                },
                "annotations": { "readOnlyHint": true, "openWorldHint": false, "destructiveHint": false }
            }));
            tools.push(json!({
                "name": "search_text",
                "title": "Search text",
                "description": "Search text across files in workspace.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search string" },
                        "path": { "type": "string", "description": "Directory path (default: workspace root)" },
                        "include_hidden": { "type": "boolean", "description": "Include dotfiles and dot-directories" },
                        "limit": { "type": "number", "description": "Max matches (1..500)" }
                    },
                    "required": ["query"]
                },
                "annotations": { "readOnlyHint": true, "openWorldHint": false, "destructiveHint": false }
            }));
            tools.push(json!({
                "name": "make_directory",
                "title": "Make directory",
                "description": "Create a directory in workspace.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "recursive": { "type": "boolean", "description": "Create parent directories if missing (default true)" }
                    },
                    "required": ["path"]
                },
                "annotations": { "readOnlyHint": false, "openWorldHint": false, "destructiveHint": true }
            }));
            tools.push(json!({
                "name": "move_path",
                "title": "Move path",
                "description": "Move or rename file/directory inside workspace.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "from": { "type": "string" },
                        "to": { "type": "string" },
                        "overwrite": { "type": "boolean", "description": "Overwrite destination if it exists" },
                        "create_dirs": { "type": "boolean", "description": "Create destination parent directories if missing" }
                    },
                    "required": ["from", "to"]
                },
                "annotations": { "readOnlyHint": false, "openWorldHint": false, "destructiveHint": true }
            }));
            tools.push(json!({
                "name": "delete_path",
                "title": "Delete path",
                "description": "Delete file or directory in workspace.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "recursive": { "type": "boolean", "description": "Delete directories recursively" }
                    },
                    "required": ["path"]
                },
                "annotations": { "readOnlyHint": false, "openWorldHint": false, "destructiveHint": true }
            }));
            tools.push(json!({
                "name": "replace_in_file",
                "title": "Replace in file",
                "description": "Replace text in a file.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "find": { "type": "string" },
                        "replace": { "type": "string" },
                        "all": { "type": "boolean", "description": "Replace all occurrences (default true)" }
                    },
                    "required": ["path", "find", "replace"]
                },
                "annotations": { "readOnlyHint": false, "openWorldHint": false, "destructiveHint": true }
            }));
        }
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
    tool_mode: ToolMode,
    devtools: &Option<Arc<Mutex<DevtoolsBridge>>>,
) -> JsonRpcResponse {
    let params = &req.params;
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");

    // Local computer tools
    if mode.computer_enabled() {
        if tool_name == "run_command" {
            return handle_run_command(req, workspace_root).await;
        }
        if tool_mode.multi_enabled() {
            match tool_name {
                "read_file" => return handle_read_file(req, workspace_root),
                "write_file" => return handle_write_file(req, workspace_root),
                "append_file" => return handle_append_file(req, workspace_root),
                "list_files" => return handle_list_files(req, workspace_root),
                "search_text" => return handle_search_text(req, workspace_root),
                "make_directory" => return handle_make_directory(req, workspace_root),
                "move_path" => return handle_move_path(req, workspace_root),
                "delete_path" => return handle_delete_path(req, workspace_root),
                "replace_in_file" => return handle_replace_in_file(req, workspace_root),
                _ => {}
            }
        }
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

fn tool_success_response(req: &JsonRpcRequest, text: String) -> JsonRpcResponse {
    JsonRpcResponse::success(
        req.id.clone(),
        json!({ "content": [{"type": "text", "text": text}] }),
    )
}

fn tool_error_response(req: &JsonRpcRequest, text: String) -> JsonRpcResponse {
    JsonRpcResponse::success(
        req.id.clone(),
        json!({ "content": [{"type": "text", "text": text}], "isError": true }),
    )
}

fn tool_arguments(req: &JsonRpcRequest) -> Value {
    req.params.get("arguments").cloned().unwrap_or(json!({}))
}

fn handle_read_file(req: &JsonRpcRequest, workspace_root: &str) -> JsonRpcResponse {
    let arguments = tool_arguments(req);
    let path = match arguments.get("path").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: path".into()),
    };
    match workspace_tools::read_file(workspace_root, path) {
        Ok(text) => tool_success_response(req, text),
        Err(e) => tool_error_response(req, e),
    }
}

fn handle_write_file(req: &JsonRpcRequest, workspace_root: &str) -> JsonRpcResponse {
    let arguments = tool_arguments(req);
    let path = match arguments.get("path").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: path".into()),
    };
    let content = match arguments.get("content").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: content".into()),
    };
    let create_dirs = arguments
        .get("create_dirs")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    match workspace_tools::write_file(workspace_root, path, content, create_dirs) {
        Ok(text) => tool_success_response(req, text),
        Err(e) => tool_error_response(req, e),
    }
}

fn handle_append_file(req: &JsonRpcRequest, workspace_root: &str) -> JsonRpcResponse {
    let arguments = tool_arguments(req);
    let path = match arguments.get("path").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: path".into()),
    };
    let content = match arguments.get("content").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: content".into()),
    };
    let create_dirs = arguments
        .get("create_dirs")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    match workspace_tools::append_file(workspace_root, path, content, create_dirs) {
        Ok(text) => tool_success_response(req, text),
        Err(e) => tool_error_response(req, e),
    }
}

fn handle_list_files(req: &JsonRpcRequest, workspace_root: &str) -> JsonRpcResponse {
    let arguments = tool_arguments(req);
    let path = arguments.get("path").and_then(|v| v.as_str());
    let include_hidden = arguments
        .get("include_hidden")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    match workspace_tools::list_files(workspace_root, path, include_hidden, limit) {
        Ok(text) => tool_success_response(req, text),
        Err(e) => tool_error_response(req, e),
    }
}

fn handle_search_text(req: &JsonRpcRequest, workspace_root: &str) -> JsonRpcResponse {
    let arguments = tool_arguments(req);
    let query = match arguments.get("query").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: query".into()),
    };
    let path = arguments.get("path").and_then(|v| v.as_str());
    let include_hidden = arguments
        .get("include_hidden")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    match workspace_tools::search_text(workspace_root, query, path, include_hidden, limit) {
        Ok(text) => tool_success_response(req, text),
        Err(e) => tool_error_response(req, e),
    }
}

fn handle_make_directory(req: &JsonRpcRequest, workspace_root: &str) -> JsonRpcResponse {
    let arguments = tool_arguments(req);
    let path = match arguments.get("path").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: path".into()),
    };
    let recursive = arguments
        .get("recursive")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    match workspace_tools::make_directory(workspace_root, path, recursive) {
        Ok(text) => tool_success_response(req, text),
        Err(e) => tool_error_response(req, e),
    }
}

fn handle_move_path(req: &JsonRpcRequest, workspace_root: &str) -> JsonRpcResponse {
    let arguments = tool_arguments(req);
    let from = match arguments.get("from").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: from".into()),
    };
    let to = match arguments.get("to").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: to".into()),
    };
    let overwrite = arguments
        .get("overwrite")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let create_dirs = arguments
        .get("create_dirs")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    match workspace_tools::move_path(workspace_root, from, to, overwrite, create_dirs) {
        Ok(text) => tool_success_response(req, text),
        Err(e) => tool_error_response(req, e),
    }
}

fn handle_delete_path(req: &JsonRpcRequest, workspace_root: &str) -> JsonRpcResponse {
    let arguments = tool_arguments(req);
    let path = match arguments.get("path").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: path".into()),
    };
    let recursive = arguments
        .get("recursive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    match workspace_tools::delete_path(workspace_root, path, recursive) {
        Ok(text) => tool_success_response(req, text),
        Err(e) => tool_error_response(req, e),
    }
}

fn handle_replace_in_file(req: &JsonRpcRequest, workspace_root: &str) -> JsonRpcResponse {
    let arguments = tool_arguments(req);
    let path = match arguments.get("path").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: path".into()),
    };
    let find = match arguments.get("find").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: find".into()),
    };
    let replace = match arguments.get("replace").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error_response(req, "Missing required parameter: replace".into()),
    };
    let replace_all = arguments
        .get("all")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    match workspace_tools::replace_in_file(workspace_root, path, find, replace, replace_all) {
        Ok(text) => tool_success_response(req, text),
        Err(e) => tool_error_response(req, e),
    }
}
