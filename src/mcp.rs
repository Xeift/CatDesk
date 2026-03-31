use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiktoken_rs::o200k_base_singleton;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::command;
use crate::devtools::DevtoolsBridge;
use crate::state::{Mode, ToolMode};
use crate::workspace_tools;

const SERVER_NAME: &str = "catdesk";
const SERVER_VERSION: &str = "4.0.0";
const PROTOCOL_VERSION: &str = "2025-03-26";
const UI_TEMPLATE_URI: &str = "ui://widget/catdesk-dashboard.html";
const UI_TEMPLATE_MIME_TYPE: &str = "text/html;profile=mcp-app";
const RENDER_FINAL_SUMMARY_WIDGET_TOOL: &str = "render_final_summary_widget";
const CATDESK_WIDGET_HTML: &str = include_str!("widget/catdesk_dashboard.html");
const MAX_DIFF_FILES: usize = 16;
const MAX_DIFF_CHARS_PER_FILE: usize = 12_000;
const MAX_COMMAND_OUTPUT_CHARS: usize = 24_000;
const MAX_SEARCH_RESULTS_WIDGET: usize = 200;
const MAX_SEARCH_LINE_CHARS: usize = 320;
const MAX_WATCHED_FILES: usize = 512;
const MAX_FILE_CAPTURE_BYTES: usize = 128 * 1024;
const MAX_TEXT_CAPTURE_LINES: usize = 420;

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
    changed_files: HashMap<String, FileDiffEntry>,
    baseline_files: HashMap<String, Option<FileSnapshot>>,
    pending_turn_token_usage: TokenUsage,
    pending_tool_call_count: u64,
}

impl Session {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            initialized: false,
            changed_files: HashMap::new(),
            baseline_files: HashMap::new(),
            pending_turn_token_usage: TokenUsage::default(),
            pending_tool_call_count: 0,
        }
    }
}

#[derive(Clone, Default)]
struct TokenUsage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

impl TokenUsage {
    fn from_counts(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens.saturating_add(output_tokens),
        }
    }

    fn accumulate(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.total_tokens = self.input_tokens.saturating_add(self.output_tokens);
    }
}

#[derive(Clone, Default)]
struct FileDiffEntry {
    path: String,
    status: String,
    added: u64,
    removed: u64,
    diff: String,
}

#[derive(Clone, Default)]
struct SearchResultEntry {
    path: String,
    line: u64,
    text: String,
}

#[derive(Clone, Default)]
struct SearchWidgetContent {
    query: String,
    path: String,
    files_scanned: u64,
    matches: u64,
    results: Vec<SearchResultEntry>,
    truncated: bool,
}

#[derive(Clone, Default)]
struct WatchedSnapshot {
    files: HashMap<String, FileSnapshot>,
}

#[derive(Clone)]
struct FileSnapshot {
    digest: u64,
    size_bytes: usize,
    is_binary: bool,
    is_directory: bool,
    text: String,
    text_truncated: bool,
}

#[derive(Clone)]
struct WatchTarget {
    path: PathBuf,
    recursive: bool,
}

#[derive(Clone)]
struct AutoWidgetContext {
    is_error: bool,
    turn_files: Vec<FileDiffEntry>,
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
                        "clientInfo": {"name": "catdesk-bridge", "version": SERVER_VERSION}
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
            Some(handle_tools_call(req, session, workspace_root, mode, tool_mode, devtools).await)
        }
        "resources/list" => Some(handle_resources_list(req)),
        "resources/read" => Some(handle_resources_read(req)),
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
                "tools": { "listChanged": false },
                "resources": { "listChanged": false }
            },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
        }),
    )
}

fn handle_resources_list(req: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(
        req.id.clone(),
        json!({
            "resources": [{
                "uri": UI_TEMPLATE_URI,
                "name": "CatDesk dashboard widget",
                "description": "Embedded ChatGPT widget for CatDesk status and timeline data.",
                "mimeType": UI_TEMPLATE_MIME_TYPE,
                "_meta": { "ui": { "prefersBorder": true } }
            }],
            "nextCursor": null
        }),
    )
}

fn handle_resources_read(req: &JsonRpcRequest) -> JsonRpcResponse {
    let uri = req
        .params
        .get("uri")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if uri != UI_TEMPLATE_URI {
        return JsonRpcResponse::error(req.id.clone(), -32602, format!("Unknown resource: {uri}"));
    }
    JsonRpcResponse::success(
        req.id.clone(),
        json!({
            "contents": [{
                "uri": UI_TEMPLATE_URI,
                "mimeType": UI_TEMPLATE_MIME_TYPE,
                "text": CATDESK_WIDGET_HTML,
                "_meta": { "ui": { "prefersBorder": true } }
            }]
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
        if tool_mode.run_command_enabled() {
            tools.push(json!({
                "name": "run_command",
                "title": "Run command",
                "description": "Execute a shell command inside the workspace root. Prefer dedicated tools first, and use this only when available tools cannot complete the operation. Returns stdout and stderr.",
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

        if tool_mode.read_tools_enabled() {
            tools.push(json!({
                "name": "catdesk_instruction",
                "title": "Get usage instructions",
                "description": "Read CatDesk operating guidance. Call this first if you are unsure which tool to use. Prefer dedicated tools over run_command whenever possible.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                },
                "annotations": { "readOnlyHint": true, "openWorldHint": false, "destructiveHint": false }
            }));
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
        }

        if tool_mode.write_tools_enabled() {
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
            if let Some(dt_tools) = fetch_devtools_tools(bridge).await {
                if tool_mode.read_only() {
                    tools.extend(dt_tools.into_iter().filter(tool_is_read_only));
                } else {
                    tools.extend(dt_tools);
                }
            }
        }
    }

    // Render tool for ChatGPT embedded UI.
    tools.push(json!({
        "name": RENDER_FINAL_SUMMARY_WIDGET_TOOL,
        "title": "Render final summary panel",
        "description": "Render the final summary UI with current tool call diff and cumulative changed files.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Optional panel title." },
                "panelMode": { "type": "string", "description": "tool_call or final_review" },
                "changedFiles": {
                    "type": "array",
                    "description": "Changed files to render in the panel.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "status": { "type": "string" },
                            "added": { "type": "number" },
                            "removed": { "type": "number" },
                            "diff": { "type": "string" }
                        }
                    }
                },
                "state": { "type": "string" },
                "showApproval": { "type": "boolean" },
                "approvePrompt": { "type": "string" },
                "rejectPrompt": { "type": "string" }
            }
        },
        "_meta": {
            "ui": { "resourceUri": UI_TEMPLATE_URI },
            "openai/outputTemplate": UI_TEMPLATE_URI,
            "openai/toolInvocation/invoking": "Rendering widget...",
            "openai/toolInvocation/invoked": "Widget rendered."
        },
        "annotations": { "readOnlyHint": true, "openWorldHint": false, "destructiveHint": false }
    }));

    for tool in &mut tools {
        ensure_tool_descriptor_widget_template(tool);
    }

    JsonRpcResponse::success(req.id.clone(), json!({ "tools": tools }))
}

// ── tools/call ──────────────────────────────────────────────

async fn handle_tools_call(
    req: &JsonRpcRequest,
    session: &mut Session,
    workspace_root: &str,
    mode: Mode,
    tool_mode: ToolMode,
    devtools: &Option<Arc<Mutex<DevtoolsBridge>>>,
) -> JsonRpcResponse {
    let params = &req.params;
    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if tool_name == RENDER_FINAL_SUMMARY_WIDGET_TOOL {
        return handle_render_final_summary_widget(req, session);
    }

    let watch_targets = collect_watch_targets(req, workspace_root);
    let before_snapshot = collect_watched_snapshot(&watch_targets, workspace_root);

    let mut response = {
        // Local computer tools
        if mode.computer_enabled() {
            if tool_name == "run_command" {
                if tool_mode.run_command_enabled() {
                    handle_run_command(req, workspace_root).await
                } else if tool_mode.read_only() {
                    read_only_blocked_response(req, &tool_name)
                } else {
                    tool_error_response(req, format!("Unknown tool: {tool_name}"))
                }
            } else if tool_mode.read_tools_enabled() {
                match tool_name.as_str() {
                    "catdesk_instruction" => {
                        handle_catdesk_instruction(req, workspace_root, mode, tool_mode)
                    }
                    "read_file" => handle_read_file(req, workspace_root),
                    "list_files" => handle_list_files(req, workspace_root),
                    "search_text" => handle_search_text(req, workspace_root),
                    _ => {
                        if tool_mode.write_tools_enabled() {
                            match tool_name.as_str() {
                                "write_file" => handle_write_file(req, workspace_root),
                                "append_file" => handle_append_file(req, workspace_root),
                                "make_directory" => handle_make_directory(req, workspace_root),
                                "move_path" => handle_move_path(req, workspace_root),
                                "delete_path" => handle_delete_path(req, workspace_root),
                                "replace_in_file" => handle_replace_in_file(req, workspace_root),
                                _ => {
                                    if mode.browser_enabled() {
                                        forward_to_devtools(req, &tool_name, tool_mode, devtools)
                                            .await
                                    } else {
                                        tool_error_response(
                                            req,
                                            format!("Unknown tool: {tool_name}"),
                                        )
                                    }
                                }
                            }
                        } else if tool_mode.read_only() && is_local_destructive_tool(&tool_name) {
                            read_only_blocked_response(req, &tool_name)
                        } else if mode.browser_enabled() {
                            forward_to_devtools(req, &tool_name, tool_mode, devtools).await
                        } else {
                            tool_error_response(req, format!("Unknown tool: {tool_name}"))
                        }
                    }
                }
            } else if tool_mode.write_tools_enabled() {
                match tool_name.as_str() {
                    "write_file" => handle_write_file(req, workspace_root),
                    "append_file" => handle_append_file(req, workspace_root),
                    "make_directory" => handle_make_directory(req, workspace_root),
                    "move_path" => handle_move_path(req, workspace_root),
                    "delete_path" => handle_delete_path(req, workspace_root),
                    "replace_in_file" => handle_replace_in_file(req, workspace_root),
                    _ => {
                        if mode.browser_enabled() {
                            forward_to_devtools(req, &tool_name, tool_mode, devtools).await
                        } else {
                            tool_error_response(req, format!("Unknown tool: {tool_name}"))
                        }
                    }
                }
            } else if tool_mode.read_only() && is_local_destructive_tool(&tool_name) {
                read_only_blocked_response(req, &tool_name)
            } else if mode.browser_enabled() {
                forward_to_devtools(req, &tool_name, tool_mode, devtools).await
            } else {
                tool_error_response(req, format!("Unknown tool: {tool_name}"))
            }
        } else if mode.browser_enabled() {
            forward_to_devtools(req, &tool_name, tool_mode, devtools).await
        } else {
            tool_error_response(req, format!("Unknown tool: {tool_name}"))
        }
    };

    let after_snapshot = collect_watched_snapshot(&watch_targets, workspace_root);
    let turn_files = diff_changed_files(&before_snapshot, &after_snapshot);
    update_session_changed_files(session, &before_snapshot, &after_snapshot);
    let is_error = response
        .result
        .as_ref()
        .and_then(|v| v.get("isError"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let has_turn_changes = !turn_files.is_empty();
    let widget_context = AutoWidgetContext {
        is_error,
        turn_files,
    };

    let tool_name = tool_name_from_request(req);
    if let Some(result) = response.result.take() {
        if has_turn_changes {
            response.result = Some(enrich_tool_result(req, result, Some(&widget_context)));
        } else {
            response.result = Some(enrich_tool_result(req, result, None));
        }
    }

    if let Some(result) = response.result.as_mut() {
        let turn_token_usage = estimate_turn_token_usage(req, &tool_name, result);
        session
            .pending_turn_token_usage
            .accumulate(&turn_token_usage);
        session.pending_tool_call_count = session.pending_tool_call_count.saturating_add(1);
        attach_turn_token_usage(result, &turn_token_usage);
    }

    response
}

async fn forward_to_devtools(
    req: &JsonRpcRequest,
    tool_name: &str,
    tool_mode: ToolMode,
    devtools: &Option<Arc<Mutex<DevtoolsBridge>>>,
) -> JsonRpcResponse {
    let params = &req.params;
    let Some(bridge) = devtools else {
        return tool_error_response(req, format!("Unknown tool: {tool_name}"));
    };

    if tool_mode.read_only() {
        match devtools_tool_is_read_only(bridge, tool_name).await {
            Some(true) => {}
            Some(false) => return read_only_blocked_response(req, tool_name),
            None => {
                return tool_error_response(
                    req,
                    format!(
                        "Tool '{tool_name}' is blocked in read-only mode (cannot verify readOnlyHint)"
                    ),
                );
            }
        }
    }

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
                return tool_error_response(
                    req,
                    format!("DevTools tool error (code {code}): {msg}"),
                );
            }
            tool_error_response(req, "DevTools bridge returned empty response".into())
        }
        Err(e) => tool_error_response(req, format!("DevTools bridge error: {e}")),
    }
}

async fn handle_run_command(req: &JsonRpcRequest, workspace_root: &str) -> JsonRpcResponse {
    let params = &req.params;
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    let cmd = match arguments.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return tool_error_response(req, "Missing required parameter: command".into());
        }
    };

    let cwd_input = arguments.get("cwd").and_then(|v| v.as_str());
    let timeout_ms = arguments.get("timeout").and_then(|v| v.as_u64());

    let cwd = match command::resolve_workspace_path(workspace_root, cwd_input) {
        Ok(p) => p,
        Err(e) => {
            return tool_error_response(req, format!("code: PATH_OUTSIDE_WORKSPACE\nmessage: {e}"));
        }
    };

    let effective_timeout = command::clamp_timeout(timeout_ms);
    let result = command::run_command(cmd, &cwd, effective_timeout).await;
    let output = command::format_result(&result);

    if result.success {
        tool_success_response(req, output)
    } else {
        tool_error_response(req, output)
    }
}

fn tool_success_response(req: &JsonRpcRequest, text: String) -> JsonRpcResponse {
    let result = enrich_tool_result(
        req,
        json!({ "content": [{"type": "text", "text": text}] }),
        None,
    );
    JsonRpcResponse::success(req.id.clone(), result)
}

fn tool_success_response_with_structured(
    req: &JsonRpcRequest,
    text: String,
    structured: Value,
) -> JsonRpcResponse {
    let result = enrich_tool_result(
        req,
        json!({
            "structuredContent": structured,
            "content": [{"type": "text", "text": text}]
        }),
        None,
    );
    JsonRpcResponse::success(req.id.clone(), result)
}

fn handle_render_final_summary_widget(
    req: &JsonRpcRequest,
    session: &mut Session,
) -> JsonRpcResponse {
    let arguments = tool_arguments(req);
    let title = arguments
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Final Summary")
        .to_string();
    let panel_mode = arguments
        .get("panelMode")
        .and_then(Value::as_str)
        .unwrap_or("final_review")
        .to_string();
    let changed_files = arguments
        .get("changedFiles")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let show_approval = arguments
        .get("showApproval")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            changed_files
                .as_array()
                .map(|arr| !arr.is_empty())
                .unwrap_or(false)
        });
    let approve_prompt = arguments
        .get("approvePrompt")
        .and_then(Value::as_str)
        .unwrap_or("Approve these file changes and continue.")
        .to_string();
    let reject_prompt = arguments
        .get("rejectPrompt")
        .and_then(Value::as_str)
        .unwrap_or("Reject these file changes and ask for a safer revision.")
        .to_string();

    let has_changes = changed_files
        .as_array()
        .map(|arr| !arr.is_empty())
        .unwrap_or(false);
    let state = arguments
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or(if has_changes {
            "waiting_approval"
        } else {
            "done"
        });

    let structured = json!({
        "schema": "catdesk.review.v1",
        "panelMode": panel_mode,
        "title": title,
        "state": state,
        "changedFiles": changed_files,
        "hasChanges": has_changes,
        "showApproval": show_approval,
        "approvePrompt": approve_prompt,
        "rejectPrompt": reject_prompt,
    });

    let mut result = enrich_tool_result(
        req,
        json!({
            "structuredContent": structured,
            "content": [{
                "type": "text",
                "text": "Rendered final summary UI in ChatGPT."
            }]
        }),
        None,
    );
    let turn_token_usage = session.pending_turn_token_usage.clone();
    attach_turn_token_usage(&mut result, &turn_token_usage);
    attach_tool_call_count(&mut result, session.pending_tool_call_count);
    session.pending_turn_token_usage = TokenUsage::default();
    session.pending_tool_call_count = 0;
    JsonRpcResponse::success(req.id.clone(), result)
}

fn tool_error_response(req: &JsonRpcRequest, text: String) -> JsonRpcResponse {
    let result = enrich_tool_result(
        req,
        json!({ "content": [{"type": "text", "text": text}], "isError": true }),
        None,
    );
    JsonRpcResponse::success(req.id.clone(), result)
}

fn read_only_blocked_response(req: &JsonRpcRequest, tool_name: &str) -> JsonRpcResponse {
    tool_error_response(
        req,
        format!("Tool '{tool_name}' is disabled in read-only mode"),
    )
}

fn tool_arguments(req: &JsonRpcRequest) -> Value {
    req.params.get("arguments").cloned().unwrap_or(json!({}))
}

fn tool_name_from_request(req: &JsonRpcRequest) -> String {
    req.params
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown_tool")
        .to_string()
}

fn workspace_agents_path(workspace_root: &str) -> PathBuf {
    Path::new(workspace_root).join("AGENTS.md")
}

fn workspace_agents_text(workspace_root: &str) -> Option<String> {
    let content = workspace_tools::read_file(workspace_root, "AGENTS.md").ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn catdesk_instruction_text(workspace_root: &str, mode: Mode, tool_mode: ToolMode) -> String {
    let mut lines = vec![
        "CatDesk usage instructions".to_string(),
        "".to_string(),
        "Prefer dedicated MCP tools whenever a dedicated tool can complete the task.".to_string(),
    ];

    if mode.computer_enabled() && tool_mode.read_tools_enabled() {
        lines.push(
            "Use read_file to read files, list_files to inspect directories, and search_text to search the workspace."
                .to_string(),
        );
        if tool_mode.write_tools_enabled() {
            lines.push(
                "Use write_file, append_file, replace_in_file, make_directory, move_path, and delete_path for direct workspace edits and filesystem changes."
                    .to_string(),
            );
        }
    }

    if mode.browser_enabled() {
        lines.push(
            "For browser tasks, prefer the dedicated browser and DevTools tools exposed by the server."
                .to_string(),
        );
        lines.push(
            "If the custom connector disconnects or returns an empty list, always call api_tool.list_resources to refresh."
                .to_string(),
        );
    }

    if mode.computer_enabled() && tool_mode.run_command_enabled() {
        lines.push(
            "Use run_command only as a last resort when the available dedicated tools cannot complete the operation."
                .to_string(),
        );
    }

    lines.push(
        "You may encounter connector tool paths that include a link segment, for example \"/some_connector_name/link_69c7196cc06c8191b774a1102e140d77/search_text\"."
            .to_string(),
    );
    lines.push(
        "Always ignore the link_ segment and call the original tool name instead, for example \"/some_connector_name/search_text\". This improves tool-calling stability."
            .to_string(),
    );
    lines.push(
        "Keep file and directory operations inside the workspace root unless a tool explicitly says otherwise."
            .to_string(),
    );
    if let Some(agents_text) = workspace_agents_text(workspace_root) {
        lines.push("".to_string());
        lines.push("Workspace-specific instructions from AGENTS.md:".to_string());
        lines.push(agents_text);
    }
    lines.push(
        "When the work is complete and you are ready to report back, always call render_final_summary_widget."
            .to_string(),
    );
    lines.join("\n")
}

fn catdesk_instruction_structured(workspace_root: &str, mode: Mode, tool_mode: ToolMode) -> Value {
    let agents_path = workspace_agents_path(workspace_root);
    json!({
        "schema": "catdesk.review.v1",
        "panelMode": "tool_call",
        "title": "CatDesk Instruction",
        "state": "done",
        "toolName": "catdesk_instruction",
        "instructionText": catdesk_instruction_text(workspace_root, mode, tool_mode),
        "workspacePath": workspace_root,
        "agentsPath": agents_path.to_string_lossy(),
        "changedFiles": [],
        "hasChanges": false
    })
}

fn handle_catdesk_instruction(
    req: &JsonRpcRequest,
    workspace_root: &str,
    mode: Mode,
    tool_mode: ToolMode,
) -> JsonRpcResponse {
    tool_success_response_with_structured(
        req,
        catdesk_instruction_text(workspace_root, mode, tool_mode),
        catdesk_instruction_structured(workspace_root, mode, tool_mode),
    )
}

fn build_turn_token_payload(req: &JsonRpcRequest, tool_name: &str) -> Value {
    json!({
        "name": tool_name,
        "arguments": tool_arguments(req),
    })
}

fn estimate_tokens_o200k(text: &str) -> u64 {
    o200k_base_singleton()
        .encode_with_special_tokens(text)
        .len()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn estimate_value_tokens_o200k(value: &Value) -> u64 {
    match serde_json::to_string(value) {
        Ok(serialized) => estimate_tokens_o200k(&serialized),
        Err(_) => 0,
    }
}

fn estimate_turn_token_usage(req: &JsonRpcRequest, tool_name: &str, result: &Value) -> TokenUsage {
    let input_payload = build_turn_token_payload(req, tool_name);
    let input_tokens = estimate_value_tokens_o200k(&input_payload);
    let output_payload = sanitize_result_for_turn_token_count(result);
    let output_tokens = estimate_value_tokens_o200k(&output_payload);
    TokenUsage::from_counts(input_tokens, output_tokens)
}

fn sanitize_result_for_turn_token_count(result: &Value) -> Value {
    let mut sanitized = result.clone();
    let Some(obj) = sanitized.as_object_mut() else {
        return sanitized;
    };
    obj.remove("_meta");
    if let Some(structured) = obj
        .get_mut("structuredContent")
        .and_then(Value::as_object_mut)
    {
        structured.remove("turnTokenUsage");
    }
    sanitized
}

fn attach_turn_token_usage(result: &mut Value, usage: &TokenUsage) {
    let Some(obj) = result.as_object_mut() else {
        return;
    };
    let Some(structured) = obj
        .get_mut("structuredContent")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    structured.insert(
        "turnTokenUsage".to_string(),
        json!({
            "inputTokens": usage.input_tokens,
            "outputTokens": usage.output_tokens,
            "totalTokens": usage.total_tokens,
        }),
    );
}

fn attach_tool_call_count(result: &mut Value, tool_call_count: u64) {
    let Some(obj) = result.as_object_mut() else {
        return;
    };
    let Some(structured) = obj
        .get_mut("structuredContent")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    structured.insert("toolCallCount".to_string(), json!(tool_call_count));
}

fn ensure_output_template_meta(meta_value: &mut Value) {
    if !meta_value.is_object() {
        *meta_value = json!({});
    }
    let Some(meta_obj) = meta_value.as_object_mut() else {
        return;
    };
    meta_obj.insert(
        "openai/outputTemplate".to_string(),
        Value::String(UI_TEMPLATE_URI.to_string()),
    );
    let ui_entry = meta_obj
        .entry("ui".to_string())
        .or_insert_with(|| json!({}));
    if !ui_entry.is_object() {
        *ui_entry = json!({});
    }
    if let Some(ui_obj) = ui_entry.as_object_mut() {
        ui_obj.insert(
            "resourceUri".to_string(),
            Value::String(UI_TEMPLATE_URI.to_string()),
        );
    }
}

fn tool_descriptor_should_attach_widget(name: &str) -> bool {
    matches!(
        name,
        "run_command"
            | "catdesk_instruction"
            | "list_files"
            | "search_text"
            | "write_file"
            | "append_file"
            | "make_directory"
            | "move_path"
            | "delete_path"
            | "replace_in_file"
            | RENDER_FINAL_SUMMARY_WIDGET_TOOL
    )
}

fn ensure_tool_descriptor_widget_template(tool: &mut Value) {
    let Some(tool_obj) = tool.as_object_mut() else {
        return;
    };
    let Some(name) = tool_obj.get("name").and_then(Value::as_str) else {
        return;
    };
    if !tool_descriptor_should_attach_widget(name) {
        return;
    }
    let meta_value = tool_obj
        .entry("_meta".to_string())
        .or_insert_with(|| json!({}));
    ensure_output_template_meta(meta_value);
}

fn extract_tool_result_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .take(3)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn truncate_for_widget(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return "...".to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let mut out = String::with_capacity(max_chars);
    out.extend(text.chars().take(keep));
    out.push_str("...");
    out
}

fn truncate_diff_for_widget(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(96);
    let mut out = String::with_capacity(max_chars + 64);
    out.extend(text.chars().take(keep));
    out.push_str("\n\n[diff truncated]\n");
    out
}

fn summarize_tool_detail(raw_text: &str, is_error: bool) -> String {
    let first_line = raw_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(if is_error {
            "Tool returned an error."
        } else {
            "Tool call completed."
        });
    truncate_for_widget(first_line, 220)
}

fn diff_line_stats(diff: &str) -> (u64, u64) {
    let mut added: u64 = 0;
    let mut removed: u64 = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added = added.saturating_add(1);
        } else if line.starts_with('-') {
            removed = removed.saturating_add(1);
        }
    }
    (added, removed)
}

fn file_entry_json(file: &FileDiffEntry) -> Value {
    json!({
        "path": file.path,
        "status": file.status,
        "added": file.added,
        "removed": file.removed,
        "diff": file.diff,
    })
}

fn search_result_entry_json(entry: &SearchResultEntry) -> Value {
    json!({
        "path": entry.path,
        "line": entry.line,
        "text": entry.text,
    })
}

fn parse_search_match_line(line: &str) -> Option<SearchResultEntry> {
    let mut parts = line.splitn(3, ':');
    let path = parts.next()?.trim();
    let line_no = parts.next()?.trim().parse::<u64>().ok()?;
    let text_raw = parts.next()?.trim_start();
    if path.is_empty() {
        return None;
    }
    Some(SearchResultEntry {
        path: path.to_string(),
        line: line_no,
        text: truncate_for_widget(text_raw, MAX_SEARCH_LINE_CHARS),
    })
}

fn parse_search_text_output(output: &str) -> SearchWidgetContent {
    let normalized = output.replace("\r\n", "\n");
    let mut lines: Vec<&str> = normalized.lines().collect();
    let mut parsed = SearchWidgetContent::default();

    let query_line = lines.first().copied().unwrap_or_default();
    if let Some(value) = query_line.strip_prefix("query: ") {
        parsed.query = value.trim().to_string();
    }

    let path_line = lines.get(1).copied().unwrap_or_default();
    if let Some(value) = path_line.strip_prefix("path: ") {
        parsed.path = value.trim().to_string();
    }

    let files_line = lines.get(2).copied().unwrap_or_default();
    if let Some(value) = files_line.strip_prefix("files_scanned: ") {
        parsed.files_scanned = value.trim().parse::<u64>().unwrap_or(0);
    }

    let matches_line = lines.get(3).copied().unwrap_or_default();
    if let Some(value) = matches_line.strip_prefix("matches: ") {
        parsed.matches = value.trim().parse::<u64>().unwrap_or(0);
    }

    if lines.len() <= 4 {
        return parsed;
    }

    lines.drain(0..4);
    while lines
        .first()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        lines.remove(0);
    }
    while lines
        .last()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        lines.pop();
    }
    if let Some(last) = lines.last().copied() {
        if last.starts_with("[truncated at ") && last.ends_with(" matches]") {
            parsed.truncated = true;
            lines.pop();
            while lines
                .last()
                .map(|line| line.trim().is_empty())
                .unwrap_or(false)
            {
                lines.pop();
            }
        }
    }

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        if parsed.results.len() >= MAX_SEARCH_RESULTS_WIDGET {
            parsed.truncated = true;
            break;
        }
        if let Some(entry) = parse_search_match_line(line) {
            parsed.results.push(entry);
        }
    }

    parsed
}

fn build_auto_widget_structured_content(
    req: &JsonRpcRequest,
    result: &Value,
    widget_context: Option<&AutoWidgetContext>,
) -> Value {
    let tool_name = tool_name_from_request(req);
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if tool_name == "run_command" {
        let arguments = tool_arguments(req);
        let command_text = arguments
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let output_text =
            truncate_for_widget(&extract_tool_result_text(result), MAX_COMMAND_OUTPUT_CHARS);
        let (state, changed_files, has_changes) = if let Some(ctx) = widget_context {
            (
                if ctx.is_error {
                    "failed"
                } else if ctx.turn_files.is_empty() {
                    "done"
                } else {
                    "changed"
                },
                ctx.turn_files
                    .iter()
                    .map(file_entry_json)
                    .collect::<Vec<_>>(),
                !ctx.turn_files.is_empty(),
            )
        } else {
            (if is_error { "failed" } else { "done" }, Vec::new(), false)
        };
        return json!({
            "schema": "catdesk.review.v1",
            "panelMode": "tool_call",
            "title": "Command Output",
            "state": state,
            "toolName": "run_command",
            "command": command_text,
            "output": output_text,
            "changedFiles": changed_files,
            "hasChanges": has_changes
        });
    }

    if tool_name == "search_text" {
        let arguments = tool_arguments(req);
        let query_arg = arguments
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let path_arg = arguments.get("path").and_then(Value::as_str).unwrap_or(".");
        let parsed = parse_search_text_output(&extract_tool_result_text(result));
        return json!({
            "schema": "catdesk.review.v1",
            "panelMode": "tool_call",
            "title": "Search Results",
            "state": if is_error { "failed" } else { "done" },
            "toolName": "search_text",
            "searchQuery": if parsed.query.is_empty() { query_arg } else { parsed.query.as_str() },
            "searchPath": if parsed.path.is_empty() { path_arg } else { parsed.path.as_str() },
            "filesScanned": parsed.files_scanned,
            "matchCount": parsed.matches,
            "searchTruncated": parsed.truncated,
            "searchResults": parsed.results.iter().map(search_result_entry_json).collect::<Vec<_>>(),
            "changedFiles": [],
            "hasChanges": false
        });
    }

    let line = summarize_tool_detail(&extract_tool_result_text(result), is_error);
    if let Some(ctx) = widget_context {
        let state = if ctx.is_error {
            "failed"
        } else if ctx.turn_files.is_empty() {
            "done"
        } else {
            "changed"
        };
        let changed_files: Vec<Value> = ctx.turn_files.iter().map(file_entry_json).collect();
        return json!({
            "schema": "catdesk.review.v1",
            "panelMode": "tool_call",
            "title": "Changed Files",
            "state": state,
            "toolName": tool_name,
            "changedFiles": changed_files,
            "hasChanges": !ctx.turn_files.is_empty()
        });
    }

    json!({
        "schema": "catdesk.review.v1",
        "panelMode": "tool_call",
        "title": "Changed Files",
        "state": if is_error { "failed" } else { "done" },
        "toolName": tool_name,
        "call": format!("call {}", tool_name),
        "detail": line,
        "changedFiles": [],
        "hasChanges": false
    })
}

fn enrich_tool_result(
    req: &JsonRpcRequest,
    mut result: Value,
    widget_context: Option<&AutoWidgetContext>,
) -> Value {
    if !result.is_object() {
        result = json!({
            "content": [{
                "type": "text",
                "text": result.to_string()
            }]
        });
    }
    let should_overwrite_structured = widget_context.is_some();
    let has_structured = matches!(result.get("structuredContent"), Some(Value::Object(_)));
    let structured = if should_overwrite_structured || !has_structured {
        Some(build_auto_widget_structured_content(
            req,
            &result,
            widget_context,
        ))
    } else {
        None
    };
    if let Some(result_obj) = result.as_object_mut() {
        let meta_value = result_obj
            .entry("_meta".to_string())
            .or_insert_with(|| json!({}));
        ensure_output_template_meta(meta_value);
        if let Some(structured) = structured {
            result_obj.insert("structuredContent".to_string(), structured);
        }
    }
    result
}

fn collect_watch_targets(req: &JsonRpcRequest, workspace_root: &str) -> Vec<WatchTarget> {
    let tool_name = tool_name_from_request(req);
    let arguments = tool_arguments(req);
    let mut dedup: HashMap<PathBuf, bool> = HashMap::new();

    let mut add_target = |path_opt: Option<&str>, recursive: bool| {
        let Some(path_input) = path_opt else {
            return;
        };
        let Ok(resolved) = command::resolve_workspace_path(workspace_root, Some(path_input)) else {
            return;
        };
        let entry = dedup.entry(resolved).or_insert(false);
        *entry |= recursive;
    };

    match tool_name.as_str() {
        "write_file" | "append_file" | "replace_in_file" => {
            add_target(arguments.get("path").and_then(Value::as_str), false);
        }
        "delete_path" | "make_directory" => {
            add_target(arguments.get("path").and_then(Value::as_str), true);
        }
        "move_path" => {
            add_target(arguments.get("from").and_then(Value::as_str), true);
            add_target(arguments.get("to").and_then(Value::as_str), true);
        }
        "run_command" => {
            if let Ok(cwd) = command::resolve_workspace_path(
                workspace_root,
                arguments.get("cwd").and_then(Value::as_str),
            ) {
                let entry = dedup.entry(cwd).or_insert(false);
                *entry = true;
            }
        }
        _ => {}
    }

    dedup
        .into_iter()
        .map(|(path, recursive)| WatchTarget { path, recursive })
        .collect()
}

fn collect_watched_snapshot(targets: &[WatchTarget], workspace_root: &str) -> WatchedSnapshot {
    let root = Path::new(workspace_root)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(workspace_root));
    let mut files: HashMap<String, FileSnapshot> = HashMap::new();
    let mut remaining = MAX_WATCHED_FILES;

    for target in targets {
        if remaining == 0 {
            break;
        }
        collect_target_files(&root, target, &mut files, &mut remaining);
    }

    WatchedSnapshot { files }
}

fn collect_target_files(
    root: &Path,
    target: &WatchTarget,
    files: &mut HashMap<String, FileSnapshot>,
    remaining: &mut usize,
) {
    if *remaining == 0 {
        return;
    }
    if !target.path.exists() {
        return;
    }
    if target.path.is_file() {
        if let Some(snapshot) = capture_file(&target.path) {
            let rel = to_relative(root, &target.path);
            files.entry(rel).or_insert(snapshot);
            *remaining = remaining.saturating_sub(1);
        }
        return;
    }
    if target.path.is_dir() {
        capture_directory(root, &target.path, files, remaining);
        collect_dir_files(root, &target.path, target.recursive, files, remaining);
    }
}

fn directory_key_from_relative(rel: &str) -> String {
    if rel.is_empty() || rel == "." {
        "./".to_string()
    } else if rel.ends_with('/') {
        rel.to_string()
    } else {
        format!("{rel}/")
    }
}

fn capture_directory(
    root: &Path,
    path: &Path,
    files: &mut HashMap<String, FileSnapshot>,
    remaining: &mut usize,
) {
    if *remaining == 0 || !path.is_dir() {
        return;
    }
    let rel = directory_key_from_relative(&to_relative(root, path));
    if let std::collections::hash_map::Entry::Vacant(v) = files.entry(rel) {
        v.insert(FileSnapshot {
            digest: 0,
            size_bytes: 0,
            is_binary: true,
            is_directory: true,
            text: String::new(),
            text_truncated: false,
        });
        *remaining = remaining.saturating_sub(1);
    }
}

fn collect_dir_files(
    root: &Path,
    start: &Path,
    recursive: bool,
    files: &mut HashMap<String, FileSnapshot>,
    remaining: &mut usize,
) {
    let mut stack = vec![start.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if *remaining == 0 {
                return;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_file() {
                if let Some(snapshot) = capture_file(&path) {
                    let rel = to_relative(root, &path);
                    if let std::collections::hash_map::Entry::Vacant(v) = files.entry(rel) {
                        v.insert(snapshot);
                        *remaining = remaining.saturating_sub(1);
                    }
                }
            } else if file_type.is_dir() {
                capture_directory(root, &path, files, remaining);
                if recursive {
                    stack.push(path);
                }
            }
        }
        if !recursive {
            break;
        }
    }
}

fn to_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn capture_file(path: &Path) -> Option<FileSnapshot> {
    let data = std::fs::read(path).ok()?;
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    let digest = hasher.finish();

    let preview = &data[..data.len().min(MAX_FILE_CAPTURE_BYTES)];
    let is_binary = preview.iter().any(|b| *b == 0);
    let mut text = String::new();
    let mut text_truncated = data.len() > MAX_FILE_CAPTURE_BYTES;

    if !is_binary {
        text = String::from_utf8_lossy(preview).to_string();
        let line_count = text.lines().count();
        if line_count > MAX_TEXT_CAPTURE_LINES {
            text = text
                .lines()
                .take(MAX_TEXT_CAPTURE_LINES)
                .collect::<Vec<_>>()
                .join("\n");
            text_truncated = true;
        }
    }

    Some(FileSnapshot {
        digest,
        size_bytes: data.len(),
        is_binary,
        is_directory: false,
        text,
        text_truncated,
    })
}

fn snapshot_equal(a: &FileSnapshot, b: &FileSnapshot) -> bool {
    a.digest == b.digest
        && a.size_bytes == b.size_bytes
        && a.is_binary == b.is_binary
        && a.is_directory == b.is_directory
}

fn build_entry_from_states(
    path: &str,
    before: Option<&FileSnapshot>,
    after: Option<&FileSnapshot>,
) -> Option<FileDiffEntry> {
    match (before, after) {
        (None, None) => None,
        (Some(b), Some(a)) if snapshot_equal(b, a) => None,
        (None, Some(a)) if a.is_directory => Some(FileDiffEntry {
            path: path.to_string(),
            status: "added".into(),
            added: 1,
            removed: 0,
            diff: format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,1 @@\n+<directory>\n"),
        }),
        (Some(b), None) if b.is_directory => Some(FileDiffEntry {
            path: path.to_string(),
            status: "deleted".into(),
            added: 0,
            removed: 1,
            diff: format!("--- a/{path}\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-<directory>\n"),
        }),
        (Some(b), Some(a)) if b.is_directory || a.is_directory => {
            if b.is_directory && a.is_directory {
                return None;
            }
            let before_marker = if b.is_directory {
                "<directory>"
            } else if b.is_binary {
                "<binary file>"
            } else {
                "<file>"
            };
            let after_marker = if a.is_directory {
                "<directory>"
            } else if a.is_binary {
                "<binary file>"
            } else {
                "<file>"
            };
            Some(FileDiffEntry {
                path: path.to_string(),
                status: "modified".into(),
                added: 1,
                removed: 1,
                diff: format!(
                    "--- a/{path}\n+++ b/{path}\n@@ -1,1 +1,1 @@\n-{before_marker}\n+{after_marker}\n"
                ),
            })
        }
        (None, Some(a)) => {
            let diff =
                truncate_diff_for_widget(&build_added_diff(path, a), MAX_DIFF_CHARS_PER_FILE);
            let (added, removed) = diff_line_stats(&diff);
            Some(FileDiffEntry {
                path: path.to_string(),
                status: "added".into(),
                added,
                removed,
                diff,
            })
        }
        (Some(b), None) => {
            let diff =
                truncate_diff_for_widget(&build_deleted_diff(path, b), MAX_DIFF_CHARS_PER_FILE);
            let (added, removed) = diff_line_stats(&diff);
            Some(FileDiffEntry {
                path: path.to_string(),
                status: "deleted".into(),
                added,
                removed,
                diff,
            })
        }
        (Some(b), Some(a)) => {
            let diff =
                truncate_diff_for_widget(&build_modified_diff(path, b, a), MAX_DIFF_CHARS_PER_FILE);
            let (added, removed) = diff_line_stats(&diff);
            Some(FileDiffEntry {
                path: path.to_string(),
                status: "modified".into(),
                added,
                removed,
                diff,
            })
        }
    }
}

fn append_prefixed_lines(out: &mut String, prefix: char, text: &str) {
    if text.is_empty() {
        out.push(prefix);
        out.push('\n');
        return;
    }
    for line in text.lines() {
        out.push(prefix);
        out.push_str(line);
        out.push('\n');
    }
}

enum LineDiffOp<'a> {
    Keep(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

fn diff_lines<'a>(before: &'a [&'a str], after: &'a [&'a str]) -> Vec<LineDiffOp<'a>> {
    let n = before.len();
    let m = after.len();
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];

    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if before[i] == after[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut ops: Vec<LineDiffOp<'a>> = Vec::with_capacity(n + m);
    let mut i = 0usize;
    let mut j = 0usize;

    while i < n && j < m {
        if before[i] == after[j] {
            ops.push(LineDiffOp::Keep(before[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push(LineDiffOp::Delete(before[i]));
            i += 1;
        } else {
            ops.push(LineDiffOp::Insert(after[j]));
            j += 1;
        }
    }

    while i < n {
        ops.push(LineDiffOp::Delete(before[i]));
        i += 1;
    }
    while j < m {
        ops.push(LineDiffOp::Insert(after[j]));
        j += 1;
    }

    ops
}

fn build_added_diff(path: &str, after: &FileSnapshot) -> String {
    if after.is_binary {
        return format!(
            "--- /dev/null\n+++ b/{path}\nBinary file added ({} bytes)\n",
            after.size_bytes
        );
    }
    let mut diff = String::new();
    let lines = after.text.lines().count().max(1);
    diff.push_str(&format!(
        "--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{lines} @@\n"
    ));
    append_prefixed_lines(&mut diff, '+', &after.text);
    if after.text_truncated {
        diff.push_str("\n[file content preview truncated]\n");
    }
    diff
}

fn build_deleted_diff(path: &str, before: &FileSnapshot) -> String {
    if before.is_binary {
        return format!(
            "--- a/{path}\n+++ /dev/null\nBinary file deleted ({} bytes)\n",
            before.size_bytes
        );
    }
    let mut diff = String::new();
    let lines = before.text.lines().count().max(1);
    diff.push_str(&format!(
        "--- a/{path}\n+++ /dev/null\n@@ -1,{lines} +0,0 @@\n"
    ));
    append_prefixed_lines(&mut diff, '-', &before.text);
    if before.text_truncated {
        diff.push_str("\n[file content preview truncated]\n");
    }
    diff
}

fn build_modified_diff(path: &str, before: &FileSnapshot, after: &FileSnapshot) -> String {
    if before.is_binary || after.is_binary {
        return format!(
            "--- a/{path}\n+++ b/{path}\nBinary file changed ({} -> {} bytes)\n",
            before.size_bytes, after.size_bytes
        );
    }
    let before_lines: Vec<&str> = before.text.lines().collect();
    let after_lines: Vec<&str> = after.text.lines().collect();
    let mut ops = diff_lines(&before_lines, &after_lines);
    let has_line_level_change = ops.iter().any(|op| !matches!(op, LineDiffOp::Keep(_)));

    let mut diff = String::new();
    let before_count = before_lines.len();
    let after_count = after_lines.len();
    let before_start = if before_count == 0 { 0 } else { 1 };
    let after_start = if after_count == 0 { 0 } else { 1 };
    diff.push_str(&format!(
        "--- a/{path}\n+++ b/{path}\n@@ -{before_start},{before_count} +{after_start},{after_count} @@\n"
    ));

    if has_line_level_change {
        for op in ops {
            match op {
                LineDiffOp::Keep(line) => {
                    diff.push(' ');
                    diff.push_str(line);
                    diff.push('\n');
                }
                LineDiffOp::Delete(line) => {
                    diff.push('-');
                    diff.push_str(line);
                    diff.push('\n');
                }
                LineDiffOp::Insert(line) => {
                    diff.push('+');
                    diff.push_str(line);
                    diff.push('\n');
                }
            }
        }
    } else {
        // Fallback for non line-level text differences (for example newline-only changes).
        ops.clear();
        append_prefixed_lines(&mut diff, '-', &before.text);
        append_prefixed_lines(&mut diff, '+', &after.text);
    }

    if before.text_truncated || after.text_truncated {
        diff.push_str("\n[file content preview truncated]\n");
    }
    diff
}

fn diff_changed_files(before: &WatchedSnapshot, after: &WatchedSnapshot) -> Vec<FileDiffEntry> {
    let mut paths: Vec<String> = before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect();
    paths.sort();
    paths.dedup();

    let mut changed: Vec<FileDiffEntry> = Vec::new();
    for path in paths {
        if let Some(entry) =
            build_entry_from_states(&path, before.files.get(&path), after.files.get(&path))
        {
            changed.push(entry);
        }
    }
    if changed.len() > MAX_DIFF_FILES {
        changed.truncate(MAX_DIFF_FILES);
    }
    changed
}

fn update_session_changed_files(
    session: &mut Session,
    before: &WatchedSnapshot,
    after: &WatchedSnapshot,
) {
    let mut paths: Vec<String> = before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect();
    paths.sort();
    paths.dedup();

    for path in paths {
        session
            .baseline_files
            .entry(path.clone())
            .or_insert_with(|| before.files.get(&path).cloned());
        let baseline = session
            .baseline_files
            .get(&path)
            .and_then(|value| value.as_ref());
        let current = after.files.get(&path);

        if let Some(entry) = build_entry_from_states(&path, baseline, current) {
            session.changed_files.insert(path.clone(), entry);
        } else {
            session.changed_files.remove(&path);
        }
    }
}

fn is_local_destructive_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "run_command"
            | "write_file"
            | "append_file"
            | "make_directory"
            | "move_path"
            | "delete_path"
            | "replace_in_file"
    )
}

fn tool_is_read_only(tool: &Value) -> bool {
    tool.get("annotations")
        .and_then(|v| v.get("readOnlyHint"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

async fn fetch_devtools_tools(bridge: &Arc<Mutex<DevtoolsBridge>>) -> Option<Vec<Value>> {
    let list_req = json!({
        "jsonrpc": "2.0",
        "id": "dt-tools-list",
        "method": "tools/list",
        "params": {}
    });
    let mut b = bridge.lock().await;
    let resp = b.request(&list_req).await.ok()?;
    let dt_tools = resp
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(Value::as_array)?
        .to_vec();
    Some(dt_tools)
}

async fn devtools_tool_is_read_only(
    bridge: &Arc<Mutex<DevtoolsBridge>>,
    tool_name: &str,
) -> Option<bool> {
    let dt_tools = fetch_devtools_tools(bridge).await?;
    dt_tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(tool_name))
        .map(tool_is_read_only)
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
        Ok(listing) => {
            let text = listing.render_text();
            let structured = json!({
                "schema": "catdesk.review.v1",
                "panelMode": "tool_call",
                "title": "List Files",
                "state": "done",
                "toolName": "list_files",
                "listPath": listing.path,
                "listItemCount": listing.item_count,
                "listDirectoryCount": listing.directory_count,
                "listFileCount": listing.file_count,
                "listOtherCount": listing.other_count,
                "listTruncated": listing.truncated,
                "listLimit": listing.limit,
                "listEntries": listing.entries,
            });
            tool_success_response_with_structured(req, text, structured)
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn render_final_summary_request(arguments: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!("req-1")),
            method: "tools/call".into(),
            params: json!({
                "name": RENDER_FINAL_SUMMARY_WIDGET_TOOL,
                "arguments": arguments,
            }),
        }
    }

    #[test]
    fn final_summary_uses_accumulated_token_usage_and_resets_session_state() {
        let req = render_final_summary_request(json!({
            "title": "Summary",
            "changedFiles": [],
        }));
        let mut session = Session::new();
        session.pending_turn_token_usage = TokenUsage::from_counts(123, 45);
        session.pending_tool_call_count = 3;

        let resp = handle_render_final_summary_widget(&req, &mut session);
        let usage = resp
            .result
            .as_ref()
            .and_then(|result| result.get("structuredContent"))
            .and_then(|structured| structured.get("turnTokenUsage"))
            .cloned()
            .expect("missing turnTokenUsage");
        let tool_call_count = resp
            .result
            .as_ref()
            .and_then(|result| result.get("structuredContent"))
            .and_then(|structured| structured.get("toolCallCount"))
            .and_then(Value::as_u64);

        assert_eq!(usage.get("inputTokens").and_then(Value::as_u64), Some(123));
        assert_eq!(usage.get("outputTokens").and_then(Value::as_u64), Some(45));
        assert_eq!(usage.get("totalTokens").and_then(Value::as_u64), Some(168));
        assert_eq!(tool_call_count, Some(3));
        assert_eq!(session.pending_turn_token_usage.input_tokens, 0);
        assert_eq!(session.pending_turn_token_usage.output_tokens, 0);
        assert_eq!(session.pending_turn_token_usage.total_tokens, 0);
        assert_eq!(session.pending_tool_call_count, 0);
    }
}
