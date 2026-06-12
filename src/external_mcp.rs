//! Downstream MCP gateway support for CatDesk.
//!
//! CatDesk remains the ChatGPT-facing MCP server while this module acts as an
//! MCP client for configured downstream stdio servers. The public surface keeps
//! the ChatGPT tool list compact by exposing one proxy tool named `mcp` by
//! default, while TOML opt-in direct tools can expose selected downstream tools
//! as top-level CatDesk tools.

use crate::state::{DirectToolsConfig, ExternalMcpConfig, ExternalMcpServer};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};

pub const EXTERNAL_MCP_TOOL_NAME: &str = "mcp";
const PROTOCOL_VERSION: &str = "2025-03-26";
#[cfg(not(test))]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(test)]
const TEST_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_HTTP_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// Result payload returned by the CatDesk `mcp` proxy tool.
pub struct ExternalMcpProxyOutput {
    pub text: String,
    pub structured: Value,
}

/// Cached metadata for one downstream MCP tool.
#[derive(Clone, Debug)]
pub struct ExternalToolMeta {
    pub server_name: String,
    pub original_name: String,
    pub exposed_name: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Value,
}

/// Successful downstream tool invocation.
#[derive(Debug)]
pub struct ExternalMcpCallResult {
    pub server_name: String,
    pub original_name: String,
    pub exposed_name: String,
    pub result: Value,
}

/// Successful downstream resource read.
#[derive(Debug)]
pub struct ExternalMcpReadResourceResult {
    pub server_name: String,
    pub uri: String,
    pub result: Value,
}

#[derive(Debug, Default)]
struct MetadataRefreshReport {
    failures: Vec<String>,
}

impl MetadataRefreshReport {
    fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }

    fn failure_summary(&self) -> String {
        self.failures.join("; ")
    }
}

struct ExternalMcpConnection {
    client: ExternalMcpClient,
    last_used: Instant,
    in_flight: usize,
}

fn normalized_lifecycle(server: &ExternalMcpServer) -> String {
    let lifecycle = server.lifecycle.trim().to_ascii_lowercase();
    match lifecycle.as_str() {
        "eager" => "eager".to_string(),
        "keep-alive" | "keep_alive" => "keep-alive".to_string(),
        _ => "lazy".to_string(),
    }
}

fn server_is_keep_alive(server: &ExternalMcpServer) -> bool {
    normalized_lifecycle(server) == "keep-alive"
}

/// Owns configured downstream MCP servers, live connections, and cached tool metadata.
pub struct ExternalMcpManager {
    config: ExternalMcpConfig,
    workspace_root: PathBuf,
    connections: HashMap<String, ExternalMcpConnection>,
    tool_metadata: HashMap<String, Vec<ExternalToolMeta>>,
}

impl Default for ExternalMcpManager {
    fn default() -> Self {
        Self::new(ExternalMcpConfig::default())
    }
}

impl ExternalMcpManager {
    pub fn new(config: ExternalMcpConfig) -> Self {
        Self::with_workspace(
            config,
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        )
    }

    pub fn with_workspace(config: ExternalMcpConfig, workspace_root: PathBuf) -> Self {
        Self {
            config,
            workspace_root,
            connections: HashMap::new(),
            tool_metadata: HashMap::new(),
        }
    }

    pub fn from_workspace_and_app_config(
        workspace_root: &str,
        app_config: ExternalMcpConfig,
    ) -> Self {
        Self::with_workspace(app_config, PathBuf::from(workspace_root))
    }

    pub fn configured_server_names(&self) -> Vec<String> {
        sorted_keys(&self.config.mcp_servers)
    }

    pub fn eager_server_names(&self) -> Vec<String> {
        let mut names = self
            .config
            .mcp_servers
            .iter()
            .filter_map(|(name, server)| {
                let lifecycle = server.lifecycle.trim().to_ascii_lowercase();
                matches!(lifecycle.as_str(), "eager" | "keep-alive" | "keep_alive")
                    .then_some(name.clone())
            })
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    pub fn status_payload(&mut self) -> Value {
        let _ = self.reap_idle_connections();
        let mut servers = Vec::new();
        let mut connected_count = 0u64;
        let mut tool_count = 0u64;
        for name in self.configured_server_names() {
            let connected = self.connections.contains_key(&name);
            if connected {
                connected_count = connected_count.saturating_add(1);
            }
            let tools = self.tool_metadata.get(&name).cloned().unwrap_or_default();
            tool_count = tool_count.saturating_add(tools.len() as u64);
            let server = self.config.mcp_servers.get(&name);
            let lifecycle = server
                .map(normalized_lifecycle)
                .unwrap_or_else(|| "lazy".to_string());
            let keep_alive = server.is_some_and(server_is_keep_alive);
            let transport = server.map(server_transport_name).unwrap_or("stdio");
            let headers = server
                .map(|server| redacted_headers_payload(&server.headers))
                .unwrap_or_default();
            servers.push(json!({
                "name": name,
                "transport": transport,
                "headers": headers,
                "lifecycle": lifecycle,
                "keepAlive": keep_alive,
                "connected": connected,
                "toolCount": tools.len(),
                "directToolsEnabled": self.server_has_any_direct_tools(&name),
                "tools": tools.into_iter().map(|meta| tool_meta_payload(&meta)).collect::<Vec<_>>(),
            }));
        }
        json!({
            "toolName": EXTERNAL_MCP_TOOL_NAME,
            "action": "status",
            "serverCount": self.config.mcp_servers.len(),
            "connectedCount": connected_count,
            "toolCount": tool_count,
            "idleTimeoutMinutes": self.config.settings.idle_timeout,
            "message": if self.config.mcp_servers.is_empty() {
                "No downstream MCP servers configured. Add [mcp.mcpServers.<name>] entries to ~/.catdesk/config.toml."
            } else {
                "External MCP gateway ready"
            },
            "servers": servers,
        })
    }

    pub fn tui_status_snapshot(
        &mut self,
        failed_server_count: usize,
        browser_gateway_enabled: bool,
    ) -> crate::state::ExternalMcpTuiStatus {
        let _ = self.reap_idle_connections();
        let tool_count = self.tool_metadata.values().map(Vec::len).sum::<usize>();
        crate::state::ExternalMcpTuiStatus {
            configured_server_count: self.config.mcp_servers.len(),
            connected_server_count: self.connections.len(),
            failed_server_count,
            tool_count,
            browser_gateway_enabled,
        }
    }

    pub async fn proxy(&mut self, arguments: &Value) -> Result<ExternalMcpProxyOutput, String> {
        self.reap_idle_connections()?;
        match ProxyAction::from_arguments(arguments)? {
            ProxyAction::Status => Ok(ExternalMcpProxyOutput {
                text: "MCP gateway status".to_string(),
                structured: self.status_payload(),
            }),
            ProxyAction::Call { tool, args, server } => {
                let call = self.call_tool(&tool, args, server.as_deref()).await?;
                let structured = json!({
                    "toolName": EXTERNAL_MCP_TOOL_NAME,
                    "action": "call",
                    "server": call.server_name,
                    "tool": call.exposed_name,
                    "downstreamTool": call.original_name,
                    "downstreamToolCallCount": 1,
                    "result": call.result,
                });
                Ok(ExternalMcpProxyOutput {
                    text: format!(
                        "called {}:{} via MCP gateway",
                        call.server_name, call.original_name
                    ),
                    structured,
                })
            }
            ProxyAction::Connect { server } => {
                let structured = self.connect(&server).await?;
                Ok(ExternalMcpProxyOutput {
                    text: format!("connected downstream MCP server: {server}"),
                    structured,
                })
            }
            ProxyAction::Disconnect { server } => {
                let structured = self.disconnect(&server).await?;
                Ok(ExternalMcpProxyOutput {
                    text: format!("disconnected downstream MCP server: {server}"),
                    structured,
                })
            }
            ProxyAction::Describe { query, server } => {
                let refresh = self.refresh_metadata_for_lookup(server.as_deref()).await?;
                let matches = self.describe_tools(&query, server.as_deref())?;
                let mut text = if matches.is_empty() {
                    format!("no downstream MCP tool matched: {query}")
                } else {
                    format!("described {} downstream MCP tool(s)", matches.len())
                };
                let partial = refresh.has_failures();
                let refresh_failures = refresh.failures;
                if partial {
                    text = format!(
                        "{text}; partial metadata refresh with {} failure(s)",
                        refresh_failures.len()
                    );
                }
                Ok(ExternalMcpProxyOutput {
                    text,
                    structured: json!({
                        "toolName": EXTERNAL_MCP_TOOL_NAME,
                        "action": "describe",
                        "query": query,
                        "server": server,
                        "partial": partial,
                        "refreshFailures": refresh_failures,
                        "matches": matches,
                    }),
                })
            }
            ProxyAction::Search { query, server } => {
                let refresh = self.refresh_metadata_for_lookup(server.as_deref()).await?;
                let matches = self.search_tools(&query, server.as_deref());
                let partial = refresh.has_failures();
                let refresh_failures = refresh.failures;
                let mut text = format!("found {} downstream MCP tool(s)", matches.len());
                if partial {
                    text = format!(
                        "{text}; partial metadata refresh with {} failure(s)",
                        refresh_failures.len()
                    );
                }
                Ok(ExternalMcpProxyOutput {
                    text,
                    structured: json!({
                        "toolName": EXTERNAL_MCP_TOOL_NAME,
                        "action": "search",
                        "query": query,
                        "server": server,
                        "partial": partial,
                        "refreshFailures": refresh_failures,
                        "matches": matches,
                    }),
                })
            }
            ProxyAction::Server { server } => {
                let structured = self.connect(&server).await?;
                Ok(ExternalMcpProxyOutput {
                    text: format!("listed downstream MCP server: {server}"),
                    structured,
                })
            }
            ProxyAction::ReadResource { uri, server } => {
                let result = self.read_resource(&uri, server.as_deref()).await?;
                Ok(ExternalMcpProxyOutput {
                    text: format!(
                        "read downstream MCP resource {} from {}",
                        result.uri, result.server_name
                    ),
                    structured: json!({
                        "toolName": EXTERNAL_MCP_TOOL_NAME,
                        "action": "readResource",
                        "server": result.server_name,
                        "uri": result.uri,
                        "result": result.result,
                    }),
                })
            }
            ProxyAction::ListResources { server } => {
                let resources = self.list_resources(server.as_deref()).await?;
                Ok(ExternalMcpProxyOutput {
                    text: format!("listed {} downstream MCP resource(s)", resources.len()),
                    structured: json!({
                        "toolName": EXTERNAL_MCP_TOOL_NAME,
                        "action": "resources",
                        "server": server,
                        "resources": resources,
                    }),
                })
            }
        }
    }

    pub async fn connect(&mut self, server_name: &str) -> Result<Value, String> {
        if self.connections.contains_key(server_name) {
            return Ok(self.server_payload(server_name));
        }
        let server = self
            .config
            .mcp_servers
            .get(server_name)
            .cloned()
            .ok_or_else(|| format!("unknown downstream MCP server: {server_name}"))?;
        let mut client = if let Some(url) = server
            .url
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            ExternalMcpClient::start_http(url, &server.headers)?
        } else {
            let command = server
                .command
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    format!(
                        "downstream MCP server `{server_name}` requires stdio command or HTTP url configuration"
                    )
                })?;
            let cwd = resolve_server_cwd(&self.workspace_root, server.cwd.as_deref());
            ExternalMcpClient::start_stdio(command, &server.args, cwd.as_deref(), &server.env)
                .await?
        };
        client.initialize().await?;
        let raw_tools = client.list_tools().await?;
        let metadata = raw_tools
            .into_iter()
            .filter_map(|tool| tool_meta_from_json(server_name, &server, tool))
            .collect::<Vec<_>>();
        self.tool_metadata.insert(server_name.to_string(), metadata);
        self.connections.insert(
            server_name.to_string(),
            ExternalMcpConnection {
                client,
                last_used: Instant::now(),
                in_flight: 0,
            },
        );
        Ok(self.server_payload(server_name))
    }

    pub async fn disconnect(&mut self, server_name: &str) -> Result<Value, String> {
        if !self.config.mcp_servers.contains_key(server_name) {
            return Err(format!("unknown downstream MCP server: {server_name}"));
        }
        let disconnected = if let Some(mut connection) = self.connections.remove(server_name) {
            connection.client.stop().await;
            true
        } else {
            false
        };
        Ok(json!({
            "toolName": EXTERNAL_MCP_TOOL_NAME,
            "action": "disconnect",
            "server": server_name,
            "disconnected": disconnected,
            "connected": self.connections.contains_key(server_name),
        }))
    }

    pub async fn shutdown_all(&mut self) -> Value {
        let names = self.connections.keys().cloned().collect::<Vec<_>>();
        let mut disconnected = Vec::new();
        for name in names {
            if let Some(mut connection) = self.connections.remove(&name) {
                connection.client.stop().await;
                disconnected.push(name);
            }
        }
        disconnected.sort();
        json!({
            "toolName": EXTERNAL_MCP_TOOL_NAME,
            "action": "shutdown",
            "disconnected": disconnected,
            "connectedCount": self.connections.len(),
        })
    }

    pub fn reap_idle_connections(&mut self) -> Result<Vec<String>, String> {
        let timeout_minutes = self.config.settings.idle_timeout;
        if timeout_minutes == 0 {
            return Ok(Vec::new());
        }
        let timeout = Duration::from_secs(timeout_minutes.saturating_mul(60));
        let now = Instant::now();
        let mut reaped = Vec::new();
        for (name, connection) in &self.connections {
            let Some(server) = self.config.mcp_servers.get(name) else {
                continue;
            };
            if server_is_keep_alive(server) || connection.in_flight > 0 {
                continue;
            }
            if now.duration_since(connection.last_used) >= timeout {
                reaped.push(name.clone());
            }
        }
        for name in &reaped {
            self.connections.remove(name);
        }
        reaped.sort();
        Ok(reaped)
    }

    #[cfg(test)]
    pub(crate) fn mark_connection_idle_for_test(&mut self, server_name: &str, idle_for: Duration) {
        if let Some(connection) = self.connections.get_mut(server_name) {
            connection.last_used = Instant::now() - idle_for;
        }
    }

    #[cfg(test)]
    pub fn connected_server_count(&self) -> usize {
        self.connections.len()
    }

    pub async fn call_tool(
        &mut self,
        tool_name: &str,
        arguments: Value,
        server_hint: Option<&str>,
    ) -> Result<ExternalMcpCallResult, String> {
        let refresh = self.refresh_metadata_for_lookup(server_hint).await?;
        let meta = match self.resolve_tool(tool_name, server_hint) {
            Ok(meta) => meta,
            Err(error) if server_hint.is_none() && refresh.has_failures() => {
                return Err(format!(
                    "{error}; metadata refresh failures: {}",
                    refresh.failure_summary()
                ));
            }
            Err(error) => return Err(error),
        };
        if !self.connections.contains_key(&meta.server_name) {
            self.connect(&meta.server_name).await?;
        }
        let connection = self
            .connections
            .get_mut(&meta.server_name)
            .ok_or_else(|| format!("downstream MCP server disappeared: {}", meta.server_name))?;
        connection.in_flight = connection.in_flight.saturating_add(1);
        let result = connection
            .client
            .call_tool(&meta.original_name, arguments)
            .await;
        connection.in_flight = connection.in_flight.saturating_sub(1);
        connection.last_used = Instant::now();
        Ok(ExternalMcpCallResult {
            server_name: meta.server_name,
            original_name: meta.original_name,
            exposed_name: meta.exposed_name,
            result: result?,
        })
    }

    pub async fn call_direct_tool(
        &mut self,
        tool_name: &str,
        arguments: Value,
        read_only: bool,
    ) -> Result<Option<ExternalMcpCallResult>, String> {
        self.refresh_metadata_for_direct_tools().await?;
        let direct_names = self
            .direct_tool_metadata_for_mode(read_only)
            .into_iter()
            .map(|meta| meta.exposed_name)
            .collect::<HashSet<_>>();
        if !direct_names.contains(tool_name) {
            return Ok(None);
        }
        self.call_tool(tool_name, arguments, None).await.map(Some)
    }

    pub async fn direct_tool_descriptors(&mut self, read_only: bool) -> Result<Vec<Value>, String> {
        self.refresh_metadata_for_direct_tools().await?;
        let mut descriptors = self
            .direct_tool_metadata_for_mode(read_only)
            .into_iter()
            .map(|meta| {
                json!({
                    "name": meta.exposed_name,
                    "title": format!("MCP: {}", meta.original_name),
                    "description": direct_tool_description(&meta),
                    "inputSchema": meta.input_schema,
                    "annotations": direct_tool_annotations(&meta)
                })
            })
            .collect::<Vec<_>>();
        descriptors.sort_by(|left, right| {
            let left = left.get("name").and_then(Value::as_str).unwrap_or_default();
            let right = right
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            left.cmp(right)
        });
        Ok(descriptors)
    }

    pub async fn list_resources(
        &mut self,
        server_hint: Option<&str>,
    ) -> Result<Vec<Value>, String> {
        let server_names = self.resolve_server_names_for_resource_action(server_hint)?;
        let mut resources = Vec::new();
        for server_name in server_names {
            self.connect(&server_name).await?;
            let connection = self
                .connections
                .get_mut(&server_name)
                .ok_or_else(|| format!("downstream MCP server disappeared: {server_name}"))?;
            let listed = connection.client.list_resources().await?;
            connection.last_used = Instant::now();
            resources.extend(
                listed
                    .into_iter()
                    .map(|resource| resource_payload(&server_name, resource)),
            );
        }
        resources.sort_by(|left, right| {
            let left_server = left
                .get("server")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let right_server = right
                .get("server")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let left_uri = left.get("uri").and_then(Value::as_str).unwrap_or_default();
            let right_uri = right.get("uri").and_then(Value::as_str).unwrap_or_default();
            (left_server, left_uri).cmp(&(right_server, right_uri))
        });
        Ok(resources)
    }

    pub async fn read_resource(
        &mut self,
        uri: &str,
        server_hint: Option<&str>,
    ) -> Result<ExternalMcpReadResourceResult, String> {
        if uri.trim().is_empty() {
            return Err("resource must be a non-empty string".to_string());
        }
        let server_names = self.resolve_server_names_for_resource_action(server_hint)?;
        let mut failures = Vec::new();
        for server_name in server_names {
            self.connect(&server_name).await?;
            let connection = self
                .connections
                .get_mut(&server_name)
                .ok_or_else(|| format!("downstream MCP server disappeared: {server_name}"))?;
            match connection.client.read_resource(uri).await {
                Ok(result) => {
                    connection.last_used = Instant::now();
                    return Ok(ExternalMcpReadResourceResult {
                        server_name,
                        uri: uri.to_string(),
                        result,
                    });
                }
                Err(error) => failures.push(format!("{server_name}: {error}")),
            }
        }
        Err(format!(
            "failed to read downstream MCP resource `{uri}`: {}",
            failures.join("; ")
        ))
    }

    async fn refresh_metadata_for_lookup(
        &mut self,
        server_hint: Option<&str>,
    ) -> Result<MetadataRefreshReport, String> {
        if let Some(server_name) = server_hint {
            self.connect(server_name).await?;
            return Ok(MetadataRefreshReport::default());
        }
        let names = self.configured_server_names();
        let mut failures = Vec::new();
        for name in names {
            if self.tool_metadata.contains_key(&name) {
                continue;
            }
            if let Err(error) = self.connect(&name).await {
                failures.push(format!("{name}: {error}"));
            }
        }
        Ok(MetadataRefreshReport { failures })
    }

    async fn refresh_metadata_for_direct_tools(&mut self) -> Result<(), String> {
        let names = self
            .configured_server_names()
            .into_iter()
            .filter(|name| self.server_has_any_direct_tools(name))
            .collect::<Vec<_>>();
        let mut failures = Vec::new();
        for name in names {
            if self.tool_metadata.contains_key(&name) {
                continue;
            }
            if let Err(error) = self.connect(&name).await {
                failures.push(format!("{name}: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "failed to refresh direct downstream MCP tools: {}",
                failures.join("; ")
            ))
        }
    }

    fn resolve_tool(
        &self,
        tool_name: &str,
        server_hint: Option<&str>,
    ) -> Result<ExternalToolMeta, String> {
        let mut matches = self
            .all_tool_metadata(server_hint)
            .into_iter()
            .filter(|meta| meta.exposed_name == tool_name || meta.original_name == tool_name)
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            return Ok(matches.remove(0));
        }
        if matches.is_empty() {
            return Err(format!("unknown downstream MCP tool: {tool_name}"));
        }
        let labels = matches
            .iter()
            .map(|meta| format!("{}:{}", meta.server_name, meta.original_name))
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "ambiguous downstream MCP tool `{tool_name}`; pass server. Matches: {labels}"
        ))
    }

    fn search_tools(&self, query: &str, server_hint: Option<&str>) -> Vec<Value> {
        let query = query.trim().to_ascii_lowercase();
        self.all_tool_metadata(server_hint)
            .into_iter()
            .filter(|meta| {
                query.is_empty()
                    || meta.exposed_name.to_ascii_lowercase().contains(&query)
                    || meta.original_name.to_ascii_lowercase().contains(&query)
                    || meta.description.to_ascii_lowercase().contains(&query)
                    || meta.server_name.to_ascii_lowercase().contains(&query)
            })
            .map(|meta| tool_meta_payload(&meta))
            .collect()
    }

    fn describe_tools(&self, query: &str, server_hint: Option<&str>) -> Result<Vec<Value>, String> {
        if query.trim().is_empty() {
            return Err("describe must be a non-empty string".to_string());
        }
        let exact = self
            .all_tool_metadata(server_hint)
            .into_iter()
            .filter(|meta| meta.exposed_name == query || meta.original_name == query)
            .collect::<Vec<_>>();
        let matches = if exact.is_empty() {
            self.search_tools(query, server_hint)
        } else {
            exact
                .into_iter()
                .map(|meta| tool_meta_payload(&meta))
                .collect()
        };
        Ok(matches)
    }

    fn all_tool_metadata(&self, server_hint: Option<&str>) -> Vec<ExternalToolMeta> {
        let mut metas = Vec::new();
        for (server_name, tools) in &self.tool_metadata {
            if server_hint.is_some_and(|hint| hint != server_name) {
                continue;
            }
            metas.extend(tools.iter().cloned());
        }
        metas.sort_by(|a, b| a.exposed_name.cmp(&b.exposed_name));
        metas
    }

    fn direct_tool_metadata(&self) -> Vec<ExternalToolMeta> {
        let mut metas = Vec::new();
        for (server_name, tools) in &self.tool_metadata {
            let Some(server) = self.config.mcp_servers.get(server_name) else {
                continue;
            };
            metas.extend(
                tools
                    .iter()
                    .filter(|meta| self.tool_is_direct_for_server(server_name, server, meta))
                    .cloned(),
            );
        }
        metas.sort_by(|left, right| left.exposed_name.cmp(&right.exposed_name));
        metas
    }

    fn direct_tool_metadata_for_mode(&self, read_only: bool) -> Vec<ExternalToolMeta> {
        self.direct_tool_metadata()
            .into_iter()
            .filter(|meta| !read_only || tool_meta_is_read_only(meta))
            .collect()
    }

    fn server_has_any_direct_tools(&self, server_name: &str) -> bool {
        self.config
            .mcp_servers
            .get(server_name)
            .is_some_and(|server| match &server.direct_tools {
                Some(DirectToolsConfig::Enabled(value)) => *value,
                Some(DirectToolsConfig::Names(names)) => !names.is_empty(),
                None => self.config.settings.direct_tools,
            })
    }

    pub fn direct_tool_name_candidate(&self, tool_name: &str) -> bool {
        self.config.mcp_servers.iter().any(|(server_name, server)| {
            let prefixed_any_name =
                tool_name.starts_with(&format!("{}_", sanitize_identifier(server_name)));
            match &server.direct_tools {
                Some(DirectToolsConfig::Enabled(value)) => *value && prefixed_any_name,
                Some(DirectToolsConfig::Names(names)) => names.iter().any(|name| {
                    tool_name == name || tool_name == exposed_tool_name(server_name, server, name)
                }),
                None => self.config.settings.direct_tools && prefixed_any_name,
            }
        })
    }

    #[cfg(test)]
    pub(crate) fn set_cached_tools_for_test(&mut self, server_name: &str, tools: Vec<Value>) {
        let Some(server) = self.config.mcp_servers.get(server_name) else {
            return;
        };
        let metadata = tools
            .into_iter()
            .filter_map(|tool| tool_meta_from_json(server_name, server, tool))
            .collect::<Vec<_>>();
        self.tool_metadata.insert(server_name.to_string(), metadata);
    }

    fn tool_is_direct_for_server(
        &self,
        server_name: &str,
        server: &ExternalMcpServer,
        meta: &ExternalToolMeta,
    ) -> bool {
        match &server.direct_tools {
            Some(DirectToolsConfig::Enabled(value)) => *value,
            Some(DirectToolsConfig::Names(names)) => names.iter().any(|name| {
                name == &meta.original_name
                    || name == &meta.exposed_name
                    || exposed_tool_name(server_name, server, name) == meta.exposed_name
            }),
            None => self.config.settings.direct_tools,
        }
    }

    fn resolve_server_names_for_resource_action(
        &self,
        server_hint: Option<&str>,
    ) -> Result<Vec<String>, String> {
        if let Some(server_name) = server_hint {
            if self.config.mcp_servers.contains_key(server_name) {
                return Ok(vec![server_name.to_string()]);
            }
            return Err(format!("unknown downstream MCP server: {server_name}"));
        }
        Ok(self.configured_server_names())
    }

    fn server_payload(&self, server_name: &str) -> Value {
        let tools = self
            .tool_metadata
            .get(server_name)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|meta| tool_meta_payload(&meta))
            .collect::<Vec<_>>();
        let server = self.config.mcp_servers.get(server_name);
        json!({
            "toolName": EXTERNAL_MCP_TOOL_NAME,
            "action": "server",
            "server": server_name,
            "transport": server.map(server_transport_name).unwrap_or("stdio"),
            "headers": server.map(|server| redacted_headers_payload(&server.headers)).unwrap_or_default(),
            "connected": self.connections.contains_key(server_name),
            "directToolsEnabled": self.server_has_any_direct_tools(server_name),
            "toolCount": tools.len(),
            "tools": tools,
        })
    }
}

struct ExternalMcpClient {
    transport: ExternalMcpClientTransport,
    next_id: u64,
}

struct ExternalMcpStdioTransport {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
}

struct ExternalMcpHttpTransport {
    client: reqwest::Client,
    url: String,
    headers: HeaderMap,
    session_id: Option<String>,
}

enum ExternalMcpClientTransport {
    Stdio(ExternalMcpStdioTransport),
    Http(ExternalMcpHttpTransport),
}

impl Drop for ExternalMcpClient {
    fn drop(&mut self) {
        if let ExternalMcpClientTransport::Stdio(transport) = &mut self.transport {
            let _ = transport.child.start_kill();
        }
    }
}

impl ExternalMcpClient {
    async fn start_stdio(
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
    ) -> Result<Self, String> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        let mut child = cmd
            .spawn()
            .map_err(|error| format!("spawn {command}: {error}"))?;
        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{command} exposed no stdin"))?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("{command} exposed no stdout"))?;
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(child_stdout);
            let mut line = String::new();
            let mut close_reason = "downstream MCP server closed stdout".to_string();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Err(error) => {
                        close_reason = format!("downstream MCP stdout read error: {error}");
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let message = match serde_json::from_str::<Value>(trimmed) {
                            Ok(message) => message,
                            Err(error) => {
                                close_reason =
                                    format!("downstream MCP returned malformed JSON: {error}");
                                break;
                            }
                        };
                        let Some(id) = message.get("id") else {
                            continue;
                        };
                        let mut pending = pending_reader.lock().await;
                        if let Some(tx) = pending.remove(&id_key(id)) {
                            let _ = tx.send(message);
                        }
                    }
                }
            }

            let mut pending = pending_reader.lock().await;
            for (_, tx) in pending.drain() {
                let _ = tx.send(json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32000, "message": close_reason},
                }));
            }
        });
        Ok(Self {
            transport: ExternalMcpClientTransport::Stdio(ExternalMcpStdioTransport {
                child,
                stdin: BufWriter::new(child_stdin),
                pending,
            }),
            next_id: 1,
        })
    }

    fn start_http(url: &str, headers: &HashMap<String, String>) -> Result<Self, String> {
        let resolved_headers = resolve_http_headers(headers)?;
        Ok(Self {
            transport: ExternalMcpClientTransport::Http(ExternalMcpHttpTransport {
                client: reqwest::Client::new(),
                url: url.trim().to_string(),
                headers: resolved_headers,
                session_id: None,
            }),
            next_id: 1,
        })
    }

    async fn initialize(&mut self) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "catdesk-mcp-gateway",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
        )
        .await?;
        self.notify("notifications/initialized", json!({})).await
    }

    async fn list_tools(&mut self) -> Result<Vec<Value>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor
                .as_ref()
                .map(|value| json!({ "cursor": value }))
                .unwrap_or_else(|| json!({}));
            let result = self.request("tools/list", params).await?;
            if let Some(entries) = result.get("tools").and_then(Value::as_array) {
                tools.extend(entries.iter().cloned());
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        self.request(
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments,
            }),
        )
        .await
    }

    async fn list_resources(&mut self) -> Result<Vec<Value>, String> {
        let mut resources = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor
                .as_ref()
                .map(|value| json!({ "cursor": value }))
                .unwrap_or_else(|| json!({}));
            let result = self.request("resources/list", params).await?;
            if let Some(entries) = result.get("resources").and_then(Value::as_array) {
                resources.extend(entries.iter().cloned());
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(resources)
    }

    async fn read_resource(&mut self, uri: &str) -> Result<Value, String> {
        self.request("resources/read", json!({ "uri": uri })).await
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = json!(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        match &mut self.transport {
            ExternalMcpClientTransport::Stdio(transport) => {
                let line = serde_json::to_string(&request).map_err(|error| error.to_string())?;
                let (tx, rx) = oneshot::channel();
                {
                    let mut pending = transport.pending.lock().await;
                    pending.insert(id_key(&id), tx);
                }
                if let Err(error) = transport.stdin.write_all(line.as_bytes()).await {
                    transport.pending.lock().await.remove(&id_key(&id));
                    return Err(format!("stdin write: {error}"));
                }
                if let Err(error) = transport.stdin.write_all(b"\n").await {
                    transport.pending.lock().await.remove(&id_key(&id));
                    return Err(format!("stdin write newline: {error}"));
                }
                if let Err(error) = transport.stdin.flush().await {
                    transport.pending.lock().await.remove(&id_key(&id));
                    return Err(format!("stdin flush: {error}"));
                }
                let request_timeout = current_request_timeout();
                let response = tokio::time::timeout(request_timeout, rx)
                    .await
                    .map_err(|_| {
                        format!(
                            "downstream MCP request timed out after {}s",
                            request_timeout.as_secs()
                        )
                    })?
                    .map_err(|_| "downstream MCP response channel closed".to_string())?;
                parse_json_rpc_result(response)
            }
            ExternalMcpClientTransport::Http(transport) => transport.request(request).await,
        }
    }

    async fn stop(&mut self) {
        if let ExternalMcpClientTransport::Stdio(transport) = &mut self.transport {
            let _ = transport.child.kill().await;
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        match &mut self.transport {
            ExternalMcpClientTransport::Stdio(transport) => {
                let line =
                    serde_json::to_string(&notification).map_err(|error| error.to_string())?;
                transport
                    .stdin
                    .write_all(line.as_bytes())
                    .await
                    .map_err(|error| format!("stdin write: {error}"))?;
                transport
                    .stdin
                    .write_all(b"\n")
                    .await
                    .map_err(|error| format!("stdin write newline: {error}"))?;
                transport
                    .stdin
                    .flush()
                    .await
                    .map_err(|error| format!("stdin flush: {error}"))
            }
            ExternalMcpClientTransport::Http(transport) => transport.notify(notification).await,
        }
    }
}

impl ExternalMcpHttpTransport {
    async fn notify(&mut self, notification: Value) -> Result<(), String> {
        let mut builder = self
            .client
            .post(&self.url)
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .headers(self.headers.clone())
            .json(&notification);
        if let Some(session_id) = &self.session_id {
            builder = builder.header("Mcp-Session-Id", session_id);
        }
        let request_timeout = current_request_timeout();
        let response = tokio::time::timeout(request_timeout, builder.send())
            .await
            .map_err(|_| {
                format!(
                    "downstream MCP HTTP notification timed out after {}s",
                    request_timeout.as_secs()
                )
            })?
            .map_err(|error| format!("downstream MCP HTTP notification failed: {error}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!(
                "downstream MCP HTTP notification error {}: {}",
                status.as_u16(),
                text
            ));
        }
        Ok(())
    }

    async fn request(&mut self, request: Value) -> Result<Value, String> {
        let mut builder = self
            .client
            .post(&self.url)
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .headers(self.headers.clone())
            .json(&request);
        if let Some(session_id) = &self.session_id {
            builder = builder.header("Mcp-Session-Id", session_id);
        }
        let request_timeout = current_request_timeout();
        let response = tokio::time::timeout(request_timeout, builder.send())
            .await
            .map_err(|_| {
                format!(
                    "downstream MCP HTTP request timed out after {}s",
                    request_timeout.as_secs()
                )
            })?
            .map_err(|error| format!("downstream MCP HTTP request failed: {error}"))?;
        if let Some(session_id) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.trim().is_empty())
        {
            self.session_id = Some(session_id.to_string());
        }
        let status = response.status();
        let content_type = parse_content_type_header(response.headers().get(CONTENT_TYPE));
        let text = tokio::time::timeout(request_timeout, response.text())
            .await
            .map_err(|_| {
                format!(
                    "downstream MCP HTTP response read timed out after {}s",
                    request_timeout.as_secs()
                )
            })?
            .map_err(|error| format!("downstream MCP HTTP response read failed: {error}"))?;
        if text.len() > MAX_HTTP_RESPONSE_BYTES {
            return Err(format!(
                "downstream MCP HTTP response exceeded {} bytes",
                MAX_HTTP_RESPONSE_BYTES
            ));
        }
        if !status.is_success() {
            return Err(format!(
                "downstream MCP HTTP error {}: {}",
                status.as_u16(),
                text
            ));
        }
        let response = parse_http_response_body(&text, content_type.as_deref())?;
        parse_json_rpc_result(response)
    }
}

#[derive(Debug, PartialEq)]
enum ProxyAction {
    Status,
    Call {
        tool: String,
        args: Value,
        server: Option<String>,
    },
    Connect {
        server: String,
    },
    Disconnect {
        server: String,
    },
    Describe {
        query: String,
        server: Option<String>,
    },
    Search {
        query: String,
        server: Option<String>,
    },
    Server {
        server: String,
    },
    ListResources {
        server: Option<String>,
    },
    ReadResource {
        uri: String,
        server: Option<String>,
    },
}

impl ProxyAction {
    fn from_arguments(arguments: &Value) -> Result<Self, String> {
        let server = optional_string_owned(arguments, "server")?;
        let tool = optional_string_owned(arguments, "tool")?;
        let connect = optional_string_owned(arguments, "connect")?;
        let disconnect = optional_string_owned(arguments, "disconnect")?;
        let describe = optional_string_owned(arguments, "describe")?;
        let search = optional_string_owned(arguments, "search")?;
        let resource = optional_string_owned(arguments, "resource")?;
        let resources = optional_bool(arguments, "resources")?;
        let args_present = arguments.get("args").is_some();
        let primary_count = [
            tool.is_some(),
            connect.is_some(),
            disconnect.is_some(),
            describe.is_some(),
            search.is_some(),
            resource.is_some(),
            resources,
        ]
        .into_iter()
        .filter(|present| *present)
        .count();

        if primary_count > 1 {
            return Err(
                "mcp proxy accepts exactly one action among tool, connect, disconnect, describe, search, resource, and resources"
                    .to_string(),
            );
        }
        if args_present && tool.is_none() {
            return Err("args can only be used with tool calls".to_string());
        }
        if let Some(tool) = tool {
            return Ok(Self::Call {
                tool,
                args: parse_proxy_args(arguments)?,
                server,
            });
        }
        if let Some(connect) = connect {
            return Ok(Self::Connect { server: connect });
        }
        if let Some(disconnect) = disconnect {
            return Ok(Self::Disconnect { server: disconnect });
        }
        if let Some(describe) = describe {
            return Ok(Self::Describe {
                query: describe,
                server,
            });
        }
        if let Some(search) = search {
            return Ok(Self::Search {
                query: search,
                server,
            });
        }
        if let Some(resource) = resource {
            return Ok(Self::ReadResource {
                uri: resource,
                server,
            });
        }
        if resources {
            return Ok(Self::ListResources { server });
        }
        if let Some(server) = server {
            return Ok(Self::Server { server });
        }
        Ok(Self::Status)
    }
}

pub fn parse_proxy_args(arguments: &Value) -> Result<Value, String> {
    let Some(raw) = arguments.get("args") else {
        return Ok(json!({}));
    };
    if raw.is_object() {
        return Ok(raw.clone());
    }
    let Some(raw) = raw.as_str() else {
        return Err("args must be a JSON object or JSON object string".to_string());
    };
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let parsed: Value =
        serde_json::from_str(raw).map_err(|error| format!("invalid args JSON: {error}"))?;
    if !parsed.is_object() {
        return Err("args must be a JSON object or parse to a JSON object".to_string());
    }
    Ok(parsed)
}

pub fn sanitize_exposed_tool_name(server_name: &str, tool_name: &str) -> String {
    let server = sanitize_identifier(server_name);
    let tool = sanitize_identifier(tool_name);
    if server.is_empty() {
        tool
    } else if tool.is_empty() {
        server
    } else {
        format!("{server}_{tool}")
    }
}

fn exposed_tool_name(server_name: &str, server: &ExternalMcpServer, tool_name: &str) -> String {
    if server.unprefixed_tools {
        sanitize_identifier(tool_name)
    } else {
        sanitize_exposed_tool_name(server_name, tool_name)
    }
}

fn sanitize_identifier(value: &str) -> String {
    let mut output = String::new();
    let mut last_was_separator = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            output.push('_');
            last_was_separator = true;
        }
    }
    output.trim_matches('_').to_string()
}

fn optional_string<'a>(arguments: &'a Value, name: &str) -> Result<Option<&'a str>, String> {
    match arguments.get(name) {
        Some(value) => value
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .map(Some)
            .ok_or_else(|| format!("{name} must be a non-empty string")),
        None => Ok(None),
    }
}

fn optional_string_owned(arguments: &Value, name: &str) -> Result<Option<String>, String> {
    Ok(optional_string(arguments, name)?.map(str::to_string))
}

fn optional_bool(arguments: &Value, name: &str) -> Result<bool, String> {
    match arguments.get(name) {
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{name} must be a boolean")),
        None => Ok(false),
    }
}

fn id_key(value: &Value) -> String {
    match value {
        Value::String(text) => format!("s:{text}"),
        _ => format!("j:{value}"),
    }
}

fn tool_meta_from_json(
    server_name: &str,
    server: &ExternalMcpServer,
    tool: Value,
) -> Option<ExternalToolMeta> {
    let object = tool.as_object()?;
    let original_name = object.get("name")?.as_str()?.to_string();
    let exposed_name = exposed_tool_name(server_name, server, &original_name);
    let excludes = server
        .exclude_tools
        .iter()
        .map(|value| value.as_str())
        .collect::<HashSet<_>>();
    if excludes.contains(original_name.as_str()) || excludes.contains(exposed_name.as_str()) {
        return None;
    }
    let title = object
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string);
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let input_schema = object
        .get("inputSchema")
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
    let annotations = object
        .get("annotations")
        .cloned()
        .unwrap_or_else(|| json!({}));
    Some(ExternalToolMeta {
        server_name: server_name.to_string(),
        original_name,
        exposed_name,
        title,
        description,
        input_schema,
        annotations,
    })
}

fn tool_meta_payload(meta: &ExternalToolMeta) -> Value {
    json!({
        "server": meta.server_name,
        "name": meta.exposed_name,
        "downstreamName": meta.original_name,
        "title": meta.title,
        "description": meta.description,
        "inputSchema": meta.input_schema,
        "annotations": meta.annotations,
        "callExample": {
            "tool": meta.exposed_name,
            "server": meta.server_name,
            "args": "{}"
        }
    })
}

fn server_transport_name(server: &ExternalMcpServer) -> &'static str {
    if server
        .url
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        "http"
    } else {
        "stdio"
    }
}

fn redacted_headers_payload(headers: &HashMap<String, String>) -> Value {
    let mut entries = serde_json::Map::new();
    for name in sorted_keys(headers) {
        entries.insert(name, json!("<redacted>"));
    }
    Value::Object(entries)
}

fn resolve_http_headers(headers: &HashMap<String, String>) -> Result<HeaderMap, String> {
    let mut output = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("invalid HTTP header name `{name}`: {error}"))?;
        let value = resolve_env_interpolations(value)?;
        let value = HeaderValue::from_str(&value)
            .map_err(|error| format!("invalid HTTP header value for `{name}`: {error}"))?;
        output.insert(name, value);
    }
    Ok(output)
}

fn resolve_env_interpolations(value: &str) -> Result<String, String> {
    let mut output = String::new();
    let mut remaining = value;
    while let Some(start) = remaining.find("${") {
        output.push_str(&remaining[..start]);
        let after_start = &remaining[start + 2..];
        let Some(end) = after_start.find('}') else {
            output.push_str(&remaining[start..]);
            return Ok(output);
        };
        let name = &after_start[..end];
        if name.is_empty() {
            return Err("empty environment variable interpolation".to_string());
        }
        let resolved = std::env::var(name)
            .map_err(|_| format!("missing environment variable for HTTP header: {name}"))?;
        output.push_str(&resolved);
        remaining = &after_start[end + 1..];
    }
    output.push_str(remaining);
    Ok(output)
}

fn parse_http_response_body(text: &str, content_type: Option<&str>) -> Result<Value, String> {
    let is_event_stream = content_type
        .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or_else(|| {
            text.lines()
                .any(|line| line.trim_start().starts_with("data:"))
        });
    if is_event_stream {
        return parse_sse_json_rpc_event(text);
    }
    serde_json::from_str(text)
        .map_err(|error| format!("downstream MCP HTTP returned invalid JSON: {error}"))
}

fn parse_sse_json_rpc_event(text: &str) -> Result<Value, String> {
    let mut data_lines = Vec::new();
    let mut saw_data = false;
    let mut last_error = None;
    for line in text.lines().chain(std::iter::once("")) {
        let line = line.trim_end_matches('\r');
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start());
            saw_data = true;
            continue;
        }
        if line.is_empty() && !data_lines.is_empty() {
            let payload = data_lines.join("\n");
            data_lines.clear();
            match serde_json::from_str::<Value>(&payload) {
                Ok(value) if value.get("result").is_some() || value.get("error").is_some() => {
                    return Ok(value);
                }
                Ok(_) => {}
                Err(error) => last_error = Some(error.to_string()),
            }
        }
    }
    if let Some(error) = last_error {
        return Err(format!(
            "downstream MCP HTTP SSE returned invalid JSON: {error}"
        ));
    }
    if saw_data {
        Err("downstream MCP HTTP SSE response contained no JSON-RPC response event".to_string())
    } else {
        Err("downstream MCP HTTP SSE response contained no data event".to_string())
    }
}

fn parse_content_type_header(value: Option<&HeaderValue>) -> Option<String> {
    value
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn parse_json_rpc_result(response: Value) -> Result<Value, String> {
    if let Some(error) = response.get("error") {
        return Err(format!("downstream MCP error: {error}"));
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

fn tool_meta_is_read_only(meta: &ExternalToolMeta) -> bool {
    meta.annotations
        .get("readOnlyHint")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn direct_tool_annotations(meta: &ExternalToolMeta) -> Value {
    if meta.annotations.is_object()
        && !meta
            .annotations
            .as_object()
            .is_some_and(|object| object.is_empty())
    {
        meta.annotations.clone()
    } else {
        json!({ "readOnlyHint": false, "openWorldHint": true, "destructiveHint": true })
    }
}

fn direct_tool_description(meta: &ExternalToolMeta) -> String {
    if meta.description.trim().is_empty() {
        format!(
            "Direct tool from downstream MCP server `{}`. Original tool: `{}`.",
            meta.server_name, meta.original_name
        )
    } else {
        format!(
            "{}\n\nDownstream MCP server: `{}`. Original tool: `{}`.",
            meta.description, meta.server_name, meta.original_name
        )
    }
}

fn resource_payload(server_name: &str, resource: Value) -> Value {
    let mut object = resource.as_object().cloned().unwrap_or_default();
    object.insert("server".to_string(), json!(server_name));
    Value::Object(object)
}

fn current_request_timeout() -> Duration {
    #[cfg(test)]
    {
        TEST_REQUEST_TIMEOUT
    }
    #[cfg(not(test))]
    {
        REQUEST_TIMEOUT
    }
}

fn sorted_keys<T>(map: &HashMap<String, T>) -> Vec<String> {
    let mut keys = map.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

fn resolve_server_cwd(workspace_root: &Path, cwd: Option<&str>) -> Option<PathBuf> {
    let cwd = cwd?.trim();
    if cwd.is_empty() {
        return None;
    }
    let path = PathBuf::from(cwd);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(workspace_root.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_server(command: &str, args: &[&str]) -> ExternalMcpServer {
        ExternalMcpServer {
            unprefixed_tools: false,
            command: Some(command.to_string()),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            env: HashMap::new(),
            cwd: None,
            url: None,
            headers: HashMap::new(),
            lifecycle: "lazy".to_string(),
            direct_tools: None,
            exclude_tools: Vec::new(),
        }
    }

    fn mock_server_script() -> &'static str {
        r#"
import json
import sys

TOOLS_PAGE_1 = [
    {
        "name": "echo",
        "description": "Echo a message",
        "inputSchema": {
            "type": "object",
            "properties": {"message": {"type": "string"}},
        },
    }
]
TOOLS_PAGE_2 = [
    {
        "name": "status",
        "description": "Read status",
        "inputSchema": {"type": "object", "properties": {}},
    }
]
RESOURCES_PAGE_1 = [
    {"uri": "mock://alpha", "name": "Alpha", "description": "Alpha resource"}
]
RESOURCES_PAGE_2 = [
    {"uri": "mock://beta", "name": "Beta", "description": "Beta resource"}
]

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")
    if request_id is None:
        continue
    params = message.get("params", {})
    if method == "initialize":
        result = {
            "protocolVersion": "2025-03-26",
            "capabilities": {"tools": {"listChanged": False}, "resources": {"listChanged": False}},
            "serverInfo": {"name": "mock", "version": "1.0.0"},
        }
    elif method == "tools/list":
        if params.get("cursor") == "tools-page-2":
            result = {"tools": TOOLS_PAGE_2}
        else:
            result = {"tools": TOOLS_PAGE_1, "nextCursor": "tools-page-2"}
    elif method == "tools/call":
        name = params.get("name")
        args = params.get("arguments", {})
        if name == "echo":
            result = {
                "content": [{"type": "text", "text": args.get("message", "")}],
                "isError": False,
            }
        elif name == "status":
            result = {
                "content": [{"type": "text", "text": "ok"}],
                "isError": False,
            }
        else:
            print(json.dumps({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32602, "message": f"unknown tool: {name}"},
            }), flush=True)
            continue
    elif method == "resources/list":
        if params.get("cursor") == "resources-page-2":
            result = {"resources": RESOURCES_PAGE_2}
        else:
            result = {"resources": RESOURCES_PAGE_1, "nextCursor": "resources-page-2"}
    elif method == "resources/read":
        uri = params.get("uri")
        result = {
            "contents": [{
                "uri": uri,
                "mimeType": "text/plain",
                "text": f"content for {uri}",
            }]
        }
    else:
        print(json.dumps({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": -32601, "message": f"unknown method: {method}"},
        }), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
"#
    }

    fn unique_workspace(prefix: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}"))
    }

    fn write_mock_server(workspace_root: &Path) -> PathBuf {
        std::fs::create_dir_all(workspace_root).expect("create workspace");
        let server_path = workspace_root.join("mock_mcp_server.py");
        std::fs::write(&server_path, mock_server_script()).expect("write mock server");
        server_path
    }

    fn mock_manager_with_server(
        server: ExternalMcpServer,
        workspace_root: &Path,
    ) -> ExternalMcpManager {
        let mut servers = HashMap::new();
        servers.insert("mock".to_string(), server);
        ExternalMcpManager::with_workspace(
            ExternalMcpConfig {
                mcp_servers: servers,
                ..ExternalMcpConfig::default()
            },
            workspace_root.to_path_buf(),
        )
    }

    fn mock_stdio_server(server_path: &Path) -> ExternalMcpServer {
        ExternalMcpServer {
            command: Some("python3".to_string()),
            args: vec!["-u".to_string(), server_path.to_string_lossy().into_owned()],
            ..ExternalMcpServer::default()
        }
    }

    #[test]
    fn sanitize_exposed_tool_names_prefixes_server_and_tool() {
        assert_eq!(
            sanitize_exposed_tool_name("Chrome DevTools", "take-screenshot"),
            "chrome_devtools_take_screenshot"
        );
        assert_eq!(
            sanitize_exposed_tool_name("serena", "read_file"),
            "serena_read_file"
        );
    }

    #[test]
    fn manager_status_reports_configured_servers() {
        let mut servers = HashMap::new();
        servers.insert(
            "serena".to_string(),
            test_server("serena-mcp-server", &["--project", "."]),
        );
        let config = ExternalMcpConfig {
            mcp_servers: servers,
            ..ExternalMcpConfig::default()
        };
        let mut manager = ExternalMcpManager::new(config);
        let status = manager.status_payload();

        assert_eq!(status.get("serverCount").and_then(Value::as_u64), Some(1));
        assert_eq!(
            status
                .get("servers")
                .and_then(Value::as_array)
                .and_then(|servers| servers.first())
                .and_then(|server| server.get("name"))
                .and_then(Value::as_str),
            Some("serena")
        );
    }

    #[test]
    fn parse_proxy_args_accepts_json_object_string() {
        let args =
            parse_proxy_args(&json!({"args":"{\"path\":\"src/main.rs\"}"})).expect("parse args");
        assert_eq!(
            args.get("path").and_then(Value::as_str),
            Some("src/main.rs")
        );
    }

    #[test]
    fn parse_proxy_args_accepts_json_object() {
        let args =
            parse_proxy_args(&json!({"args":{"path":"src/main.rs"}})).expect("parse object args");
        assert_eq!(
            args.get("path").and_then(Value::as_str),
            Some("src/main.rs")
        );
    }

    #[test]
    fn parse_proxy_args_rejects_json_arrays() {
        let error = parse_proxy_args(&json!({"args":"[]"})).expect_err("array should fail");
        assert!(error.contains("JSON object"));
    }

    #[test]
    fn proxy_action_rejects_multiple_actions() {
        let error = ProxyAction::from_arguments(&json!({
            "tool": "mock_echo",
            "search": "echo",
            "args": "{}"
        }))
        .expect_err("multiple actions should fail");
        assert!(error.contains("exactly one action"));
    }

    #[test]
    fn proxy_action_rejects_args_without_tool() {
        let error = ProxyAction::from_arguments(&json!({
            "search": "echo",
            "args": "{}"
        }))
        .expect_err("args without tool should fail");
        assert!(error.contains("args can only be used with tool"));
    }

    #[test]
    fn proxy_action_rejects_invalid_args_json() {
        let error = ProxyAction::from_arguments(&json!({
            "tool": "mock_echo",
            "args": "{not json}"
        }))
        .expect_err("invalid JSON should fail");
        assert!(error.contains("invalid args JSON"));
    }

    #[test]
    fn parses_sse_json_rpc_http_response_body() {
        let payload =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let parsed = parse_http_response_body(payload, Some("text/event-stream"))
            .expect("parse SSE JSON-RPC body");
        assert_eq!(
            parsed
                .get("result")
                .and_then(|result| result.get("ok"))
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn parses_sse_json_rpc_response_after_notification_event() {
        let payload = concat!(
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\n\n",
        );
        let parsed = parse_http_response_body(payload, Some("text/event-stream"))
            .expect("parse SSE response after notification");
        assert_eq!(
            parsed
                .get("result")
                .and_then(|result| result.get("ok"))
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[tokio::test]
    async fn direct_tool_descriptors_read_only_filters_annotations() {
        let mut servers = HashMap::new();
        servers.insert(
            "mock".to_string(),
            ExternalMcpServer {
                command: Some("mock".to_string()),
                direct_tools: Some(DirectToolsConfig::Enabled(true)),
                ..ExternalMcpServer::default()
            },
        );
        let mut manager = ExternalMcpManager::new(ExternalMcpConfig {
            mcp_servers: servers,
            ..ExternalMcpConfig::default()
        });
        manager.set_cached_tools_for_test(
            "mock",
            vec![
                json!({
                    "name": "safe",
                    "description": "Safe read-only tool",
                    "inputSchema": {"type":"object", "properties": {}},
                    "annotations": {"readOnlyHint": true, "openWorldHint": false, "destructiveHint": false}
                }),
                json!({
                    "name": "unsafe",
                    "description": "Unsafe tool",
                    "inputSchema": {"type":"object", "properties": {}},
                    "annotations": {"readOnlyHint": false, "destructiveHint": true}
                }),
                json!({
                    "name": "unknown",
                    "description": "Missing annotations",
                    "inputSchema": {"type":"object", "properties": {}}
                }),
            ],
        );

        let read_only = manager
            .direct_tool_descriptors(true)
            .await
            .expect("read-only descriptors");
        let names = read_only
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["mock_safe"]);
        assert_eq!(
            read_only
                .first()
                .and_then(|tool| tool.get("annotations"))
                .and_then(|annotations| annotations.get("readOnlyHint"))
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn tool_metadata_skips_excluded_tools() {
        let mut server = test_server("mock", &[]);
        server.exclude_tools = vec!["danger".to_string()];
        let danger = tool_meta_from_json(
            "mock",
            &server,
            json!({"name":"danger", "inputSchema": {"type":"object"}}),
        );
        let safe = tool_meta_from_json(
            "mock",
            &server,
            json!({"name":"read", "description":"Read something"}),
        );
        assert!(danger.is_none());
        assert_eq!(safe.expect("safe tool").exposed_name, "mock_read");
    }

    #[tokio::test]
    async fn manager_connects_and_calls_stdio_server() {
        let workspace_root = unique_workspace("catdesk-external-mcp-call");
        let server_path = write_mock_server(&workspace_root);
        let mut manager =
            mock_manager_with_server(mock_stdio_server(&server_path), &workspace_root);

        let status = manager.connect("mock").await.expect("connect mock server");
        assert_eq!(status.get("toolCount").and_then(Value::as_u64), Some(2));

        let call = manager
            .call_tool("mock_echo", json!({"message":"hello"}), None)
            .await
            .expect("call downstream tool");
        assert_eq!(call.server_name, "mock");
        assert_eq!(call.original_name, "echo");
        assert_eq!(call.exposed_name, "mock_echo");
        assert_eq!(
            call.result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|content| content.first())
                .and_then(|entry| entry.get("text"))
                .and_then(Value::as_str),
            Some("hello")
        );

        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn proxy_search_returns_partial_results_when_one_server_fails() {
        let workspace_root = unique_workspace("catdesk-external-mcp-partial-search");
        let server_path = write_mock_server(&workspace_root);
        let mut servers = HashMap::new();
        servers.insert("mock".to_string(), mock_stdio_server(&server_path));
        servers.insert(
            "broken".to_string(),
            test_server("catdesk-missing-mcp-command-for-test", &[]),
        );
        let mut manager = ExternalMcpManager::with_workspace(
            ExternalMcpConfig {
                mcp_servers: servers,
                ..ExternalMcpConfig::default()
            },
            workspace_root.clone(),
        );

        let output = manager
            .proxy(&json!({"search": "echo"}))
            .await
            .expect("partial search should succeed");

        assert!(output.text.contains("partial metadata refresh"));
        assert_eq!(
            output.structured.get("partial").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            output
                .structured
                .get("matches")
                .and_then(Value::as_array)
                .and_then(|matches| matches.first())
                .and_then(|tool| tool.get("name"))
                .and_then(Value::as_str),
            Some("mock_echo")
        );
        let refresh_failures = output
            .structured
            .get("refreshFailures")
            .and_then(Value::as_array)
            .expect("refresh failures should be present");
        assert_eq!(refresh_failures.len(), 1);
        assert!(
            refresh_failures
                .first()
                .and_then(Value::as_str)
                .is_some_and(|failure| failure.contains("broken:"))
        );

        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn proxy_search_with_server_hint_keeps_refresh_failure_strict() {
        let workspace_root = unique_workspace("catdesk-external-mcp-strict-search");
        std::fs::create_dir_all(&workspace_root).expect("create workspace");
        let mut servers = HashMap::new();
        servers.insert(
            "broken".to_string(),
            test_server("catdesk-missing-mcp-command-for-test", &[]),
        );
        let mut manager = ExternalMcpManager::with_workspace(
            ExternalMcpConfig {
                mcp_servers: servers,
                ..ExternalMcpConfig::default()
            },
            workspace_root.clone(),
        );

        let error = match manager
            .proxy(&json!({"search": "echo", "server": "broken"}))
            .await
        {
            Ok(_) => panic!("server-scoped search should fail for the target server"),
            Err(error) => error,
        };

        assert!(error.contains("spawn catdesk-missing-mcp-command-for-test"));

        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn manager_lists_and_reads_downstream_resources() {
        let workspace_root = unique_workspace("catdesk-external-mcp-resources");
        let server_path = write_mock_server(&workspace_root);
        let mut manager =
            mock_manager_with_server(mock_stdio_server(&server_path), &workspace_root);

        let resources = manager
            .list_resources(Some("mock"))
            .await
            .expect("list resources");
        assert_eq!(resources.len(), 2);
        assert_eq!(
            resources
                .iter()
                .find(|resource| resource.get("uri").and_then(Value::as_str) == Some("mock://beta"))
                .and_then(|resource| resource.get("server"))
                .and_then(Value::as_str),
            Some("mock")
        );

        let read = manager
            .read_resource("mock://alpha", Some("mock"))
            .await
            .expect("read resource");
        assert_eq!(read.server_name, "mock");
        assert_eq!(read.uri, "mock://alpha");
        assert_eq!(
            read.result
                .get("contents")
                .and_then(Value::as_array)
                .and_then(|contents| contents.first())
                .and_then(|entry| entry.get("text"))
                .and_then(Value::as_str),
            Some("content for mock://alpha")
        );

        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn proxy_lists_and_reads_resources() {
        let workspace_root = unique_workspace("catdesk-external-mcp-proxy-resources");
        let server_path = write_mock_server(&workspace_root);
        let mut manager =
            mock_manager_with_server(mock_stdio_server(&server_path), &workspace_root);

        let list = manager
            .proxy(&json!({"resources": true, "server": "mock"}))
            .await
            .expect("proxy list resources");
        assert_eq!(
            list.structured.get("action").and_then(Value::as_str),
            Some("resources")
        );
        assert_eq!(
            list.structured
                .get("resources")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );

        let read = manager
            .proxy(&json!({"resource": "mock://beta", "server": "mock"}))
            .await
            .expect("proxy read resource");
        assert_eq!(
            read.structured.get("action").and_then(Value::as_str),
            Some("readResource")
        );
        assert_eq!(
            read.structured
                .get("result")
                .and_then(|result| result.get("contents"))
                .and_then(Value::as_array)
                .and_then(|contents| contents.first())
                .and_then(|entry| entry.get("text"))
                .and_then(Value::as_str),
            Some("content for mock://beta")
        );

        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn tool_metadata_can_expose_unprefixed_tools_for_browser_gateway() {
        let server = ExternalMcpServer {
            command: Some("npx".to_string()),
            unprefixed_tools: true,
            direct_tools: Some(DirectToolsConfig::Enabled(true)),
            ..ExternalMcpServer::default()
        };
        let meta = tool_meta_from_json(
            "browser",
            &server,
            json!({"name":"take-screenshot", "inputSchema": {"type":"object"}}),
        )
        .expect("tool metadata");
        assert_eq!(meta.exposed_name, "take_screenshot");
    }

    #[test]
    fn tui_status_snapshot_reports_gateway_counts() {
        let mut servers = HashMap::new();
        servers.insert(
            "mock".to_string(),
            ExternalMcpServer {
                command: Some("mock".to_string()),
                ..ExternalMcpServer::default()
            },
        );
        let mut manager = ExternalMcpManager::new(ExternalMcpConfig {
            mcp_servers: servers,
            ..ExternalMcpConfig::default()
        });
        manager.set_cached_tools_for_test(
            "mock",
            vec![json!({"name":"read", "inputSchema": {"type":"object"}})],
        );
        let snapshot = manager.tui_status_snapshot(2, true);
        assert_eq!(snapshot.configured_server_count, 1);
        assert_eq!(snapshot.connected_server_count, 0);
        assert_eq!(snapshot.failed_server_count, 2);
        assert_eq!(snapshot.tool_count, 1);
        assert!(snapshot.browser_gateway_enabled);
    }

    #[test]
    fn direct_tool_name_candidate_matches_global_and_server_forms() {
        let mut servers = HashMap::new();
        servers.insert(
            "global".to_string(),
            ExternalMcpServer {
                command: Some("mock".to_string()),
                ..ExternalMcpServer::default()
            },
        );
        servers.insert(
            "server".to_string(),
            ExternalMcpServer {
                command: Some("mock".to_string()),
                direct_tools: Some(DirectToolsConfig::Enabled(true)),
                ..ExternalMcpServer::default()
            },
        );
        servers.insert(
            "allow".to_string(),
            ExternalMcpServer {
                command: Some("mock".to_string()),
                direct_tools: Some(DirectToolsConfig::Names(vec!["echo".to_string()])),
                ..ExternalMcpServer::default()
            },
        );
        let manager = ExternalMcpManager::new(ExternalMcpConfig {
            settings: crate::state::ExternalMcpSettings {
                direct_tools: true,
                ..Default::default()
            },
            mcp_servers: servers,
        });

        assert!(manager.direct_tool_name_candidate("global_echo"));
        assert!(manager.direct_tool_name_candidate("server_status"));
        assert!(manager.direct_tool_name_candidate("allow_echo"));
        assert!(!manager.direct_tool_name_candidate("allow_status"));
    }

    #[tokio::test]
    async fn direct_tool_descriptors_keep_duplicate_names_unambiguous() {
        let mut servers = HashMap::new();
        servers.insert(
            "alpha".to_string(),
            ExternalMcpServer {
                command: Some("mock".to_string()),
                direct_tools: Some(DirectToolsConfig::Enabled(true)),
                ..ExternalMcpServer::default()
            },
        );
        servers.insert(
            "beta".to_string(),
            ExternalMcpServer {
                command: Some("mock".to_string()),
                direct_tools: Some(DirectToolsConfig::Enabled(true)),
                ..ExternalMcpServer::default()
            },
        );
        let mut manager = ExternalMcpManager::new(ExternalMcpConfig {
            mcp_servers: servers,
            ..ExternalMcpConfig::default()
        });
        let echo_tool = json!({
            "name": "echo",
            "description": "Echo a message",
            "inputSchema": {"type": "object", "properties": {}}
        });
        manager.set_cached_tools_for_test("alpha", vec![echo_tool.clone()]);
        manager.set_cached_tools_for_test("beta", vec![echo_tool]);

        let descriptors = manager
            .direct_tool_descriptors(false)
            .await
            .expect("direct descriptors");
        let names = descriptors
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["alpha_echo", "beta_echo"]);
    }

    #[tokio::test]
    async fn original_tool_name_requires_server_when_ambiguous() {
        let mut servers = HashMap::new();
        servers.insert(
            "alpha".to_string(),
            ExternalMcpServer {
                command: Some("mock".to_string()),
                ..ExternalMcpServer::default()
            },
        );
        servers.insert(
            "beta".to_string(),
            ExternalMcpServer {
                command: Some("mock".to_string()),
                ..ExternalMcpServer::default()
            },
        );
        let mut manager = ExternalMcpManager::new(ExternalMcpConfig {
            mcp_servers: servers,
            ..ExternalMcpConfig::default()
        });
        let echo_tool = json!({
            "name": "echo",
            "description": "Echo a message",
            "inputSchema": {"type": "object", "properties": {}}
        });
        manager.set_cached_tools_for_test("alpha", vec![echo_tool.clone()]);
        manager.set_cached_tools_for_test("beta", vec![echo_tool]);

        let error = manager
            .call_tool("echo", json!({}), None)
            .await
            .expect_err("ambiguous original tool name should fail");
        assert!(error.contains("ambiguous downstream MCP tool `echo`"));
        assert!(error.contains("alpha:echo"));
        assert!(error.contains("beta:echo"));
    }

    #[tokio::test]
    async fn manager_connects_and_calls_http_server_with_headers_and_session() {
        use axum::extract::State;
        use axum::http::{HeaderMap as AxumHeaderMap, HeaderValue as AxumHeaderValue};
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::{Json, Router};

        async fn handle_http_mcp(
            State(requests): State<Arc<Mutex<Vec<Value>>>>,
            headers: AxumHeaderMap,
            Json(message): Json<Value>,
        ) -> impl IntoResponse {
            let method = message
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            requests.lock().await.push(json!({
                "method": method,
                "authorization": headers.get("authorization").and_then(|value| value.to_str().ok()),
                "xApiKey": headers.get("x-api-key").and_then(|value| value.to_str().ok()),
                "session": headers.get("mcp-session-id").and_then(|value| value.to_str().ok()),
                "accept": headers.get("accept").and_then(|value| value.to_str().ok()),
                "protocol": headers.get("mcp-protocol-version").and_then(|value| value.to_str().ok()),
            }));
            let id = message.get("id").cloned().unwrap_or(Value::Null);
            let result = match method.as_str() {
                "initialize" => json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "http-mock", "version": "1.0.0"},
                }),
                "tools/list" => json!({
                    "tools": [{
                        "name": "echo",
                        "description": "Echo over HTTP",
                        "inputSchema": {"type":"object", "properties": {"message": {"type":"string"}}},
                    }]
                }),
                "tools/call" => {
                    let text = message
                        .get("params")
                        .and_then(|params| params.get("arguments"))
                        .and_then(|args| args.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    json!({"content": [{"type":"text", "text": text}], "isError": false})
                }
                _ => json!({}),
            };
            let mut response =
                Json(json!({"jsonrpc":"2.0", "id": id, "result": result})).into_response();
            response
                .headers_mut()
                .insert("mcp-session-id", AxumHeaderValue::from_static("session-1"));
            response
        }

        let requests = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/mcp", post(handle_http_mcp))
            .with_state(requests.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock HTTP server");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer test-token".to_string());
        headers.insert("X-Api-Key".to_string(), "${PATH}".to_string());
        let mut servers = HashMap::new();
        servers.insert(
            "http".to_string(),
            ExternalMcpServer {
                url: Some(format!("http://{addr}/mcp")),
                headers,
                ..ExternalMcpServer::default()
            },
        );
        let mut manager = ExternalMcpManager::new(ExternalMcpConfig {
            mcp_servers: servers,
            ..ExternalMcpConfig::default()
        });

        let status = manager.connect("http").await.expect("connect HTTP server");
        assert_eq!(
            status.get("transport").and_then(Value::as_str),
            Some("http")
        );
        assert_eq!(status.get("toolCount").and_then(Value::as_u64), Some(1));
        let call = manager
            .call_tool("http_echo", json!({"message":"hello http"}), None)
            .await
            .expect("call HTTP downstream tool");
        assert_eq!(
            call.result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|content| content.first())
                .and_then(|entry| entry.get("text"))
                .and_then(Value::as_str),
            Some("hello http")
        );

        let requests = requests.lock().await;
        assert!(requests.iter().any(|request| {
            request.get("authorization").and_then(Value::as_str) == Some("Bearer test-token")
                && request.get("xApiKey").and_then(Value::as_str)
                    == std::env::var("PATH").ok().as_deref()
        }));
        assert!(requests.iter().any(|request| {
            request.get("method").and_then(Value::as_str) == Some("notifications/initialized")
                && request.get("session").and_then(Value::as_str) == Some("session-1")
        }));
        assert!(requests.iter().any(|request| {
            request.get("method").and_then(Value::as_str) == Some("tools/list")
                && request.get("session").and_then(Value::as_str) == Some("session-1")
        }));
        assert!(requests.iter().any(|request| {
            request.get("accept").and_then(Value::as_str)
                == Some("application/json, text/event-stream")
                && request.get("protocol").and_then(Value::as_str) == Some(PROTOCOL_VERSION)
        }));
    }

    #[tokio::test]
    async fn http_transport_accepts_sse_json_rpc_responses() {
        use axum::extract::State;
        use axum::http::HeaderValue as AxumHeaderValue;
        use axum::response::{IntoResponse, Response};
        use axum::routing::post;
        use axum::{Json, Router};

        async fn handle_sse_http_mcp(
            State(requests): State<Arc<Mutex<Vec<Value>>>>,
            Json(message): Json<Value>,
        ) -> Response {
            let method = message
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            requests.lock().await.push(json!({"method": method}));
            let id = message.get("id").cloned().unwrap_or(Value::Null);
            let result = match method.as_str() {
                "initialize" => json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "sse-mock", "version": "1.0.0"},
                }),
                "tools/list" => json!({
                    "tools": [{
                        "name": "echo",
                        "description": "Echo through SSE",
                        "inputSchema": {"type":"object", "properties": {"message": {"type":"string"}}},
                    }]
                }),
                "tools/call" => {
                    json!({"content": [{"type":"text", "text": "sse-ok"}], "isError": false})
                }
                _ => json!({}),
            };
            let body = format!(
                "event: message\ndata: {}\n\n",
                json!({"jsonrpc":"2.0", "id": id, "result": result})
            );
            let mut response = body.into_response();
            response.headers_mut().insert(
                "content-type",
                AxumHeaderValue::from_static("text/event-stream"),
            );
            response.headers_mut().insert(
                "mcp-session-id",
                AxumHeaderValue::from_static("sse-session"),
            );
            response
        }

        let requests = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/mcp", post(handle_sse_http_mcp))
            .with_state(requests.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind SSE HTTP server");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let mut servers = HashMap::new();
        servers.insert(
            "sse".to_string(),
            ExternalMcpServer {
                url: Some(format!("http://{addr}/mcp")),
                ..ExternalMcpServer::default()
            },
        );
        let mut manager = ExternalMcpManager::new(ExternalMcpConfig {
            mcp_servers: servers,
            ..ExternalMcpConfig::default()
        });
        manager
            .connect("sse")
            .await
            .expect("connect SSE HTTP server");
        let call = manager
            .call_tool("sse_echo", json!({"message":"ignored"}), None)
            .await
            .expect("call SSE HTTP tool");
        assert_eq!(
            call.result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|content| content.first())
                .and_then(|entry| entry.get("text"))
                .and_then(Value::as_str),
            Some("sse-ok")
        );
        let requests = requests.lock().await;
        assert!(
            requests
                .iter()
                .any(|request| request.get("method").and_then(Value::as_str)
                    == Some("notifications/initialized"))
        );
    }

    #[tokio::test]
    async fn http_transport_reports_json_rpc_and_http_errors() {
        use axum::extract::State;
        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::{Json, Router};

        async fn handle_http_error_mcp(
            State(mode): State<String>,
            Json(message): Json<Value>,
        ) -> (StatusCode, Json<Value>) {
            let id = message.get("id").cloned().unwrap_or(Value::Null);
            if mode == "http" {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"message":"boom"})),
                );
            }
            (
                StatusCode::OK,
                Json(json!({
                    "jsonrpc":"2.0",
                    "id": id,
                    "error": {"code": -32001, "message": "json rpc boom"},
                })),
            )
        }

        async fn spawn_error_server(mode: &str) -> String {
            let app = Router::new()
                .route("/mcp", post(handle_http_error_mcp))
                .with_state(mode.to_string());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind error HTTP server");
            let addr = listener.local_addr().expect("local addr");
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            format!("http://{addr}/mcp")
        }

        for (name, mode, expected) in [
            ("json", "json", "downstream MCP error"),
            ("http", "http", "downstream MCP HTTP error 500"),
        ] {
            let mut servers = HashMap::new();
            servers.insert(
                name.to_string(),
                ExternalMcpServer {
                    url: Some(spawn_error_server(mode).await),
                    ..ExternalMcpServer::default()
                },
            );
            let mut manager = ExternalMcpManager::new(ExternalMcpConfig {
                mcp_servers: servers,
                ..ExternalMcpConfig::default()
            });
            let error = manager
                .connect(name)
                .await
                .expect_err("HTTP error should fail");
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[test]
    fn http_header_interpolation_reports_missing_environment_variables() {
        let mut headers = HashMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer ${CATDESK_MISSING_ENV_FOR_TEST}".to_string(),
        );
        let error = resolve_http_headers(&headers).expect_err("missing env should fail");
        assert!(error.contains("CATDESK_MISSING_ENV_FOR_TEST"));
    }

    #[test]
    fn status_payload_redacts_http_headers() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer secret".to_string());
        let mut servers = HashMap::new();
        servers.insert(
            "http".to_string(),
            ExternalMcpServer {
                url: Some("http://127.0.0.1:3000/mcp".to_string()),
                headers,
                ..ExternalMcpServer::default()
            },
        );
        let mut manager = ExternalMcpManager::new(ExternalMcpConfig {
            mcp_servers: servers,
            ..ExternalMcpConfig::default()
        });
        let status = manager.status_payload();
        assert_eq!(
            status
                .get("servers")
                .and_then(Value::as_array)
                .and_then(|servers| servers.first())
                .and_then(|server| server.get("headers"))
                .and_then(|headers| headers.get("Authorization"))
                .and_then(Value::as_str),
            Some("<redacted>")
        );
    }

    #[test]
    fn status_reports_empty_gateway_message_and_lifecycle_fields() {
        let mut empty_manager = ExternalMcpManager::new(ExternalMcpConfig::default());
        let empty_status = empty_manager.status_payload();
        assert_eq!(
            empty_status.get("message").and_then(Value::as_str),
            Some(
                "No downstream MCP servers configured. Add [mcp.mcpServers.<name>] entries to ~/.catdesk/config.toml."
            )
        );

        let mut servers = HashMap::new();
        servers.insert(
            "keep".to_string(),
            ExternalMcpServer {
                command: Some("mock".to_string()),
                lifecycle: "keep_alive".to_string(),
                ..ExternalMcpServer::default()
            },
        );
        let mut manager = ExternalMcpManager::new(ExternalMcpConfig {
            mcp_servers: servers,
            ..ExternalMcpConfig::default()
        });
        let status = manager.status_payload();
        let server = status
            .get("servers")
            .and_then(Value::as_array)
            .and_then(|servers| servers.first())
            .expect("missing server status");
        assert_eq!(
            server.get("lifecycle").and_then(Value::as_str),
            Some("keep-alive")
        );
        assert_eq!(server.get("keepAlive").and_then(Value::as_bool), Some(true));
        assert_eq!(
            status.get("idleTimeoutMinutes").and_then(Value::as_u64),
            Some(10)
        );
    }

    #[tokio::test]
    async fn proxy_disconnect_removes_connected_server() {
        let workspace_root = unique_workspace("catdesk-external-mcp-disconnect");
        let server_path = write_mock_server(&workspace_root);
        let mut manager =
            mock_manager_with_server(mock_stdio_server(&server_path), &workspace_root);

        manager.connect("mock").await.expect("connect mock server");
        assert_eq!(manager.connected_server_count(), 1);
        let output = manager
            .proxy(&json!({"disconnect": "mock"}))
            .await
            .expect("disconnect through proxy");
        assert_eq!(
            output.structured.get("action").and_then(Value::as_str),
            Some("disconnect")
        );
        assert_eq!(
            output
                .structured
                .get("disconnected")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(manager.connected_server_count(), 0);

        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn idle_reaping_disconnects_lazy_servers_and_keeps_keep_alive_servers() {
        let workspace_root = unique_workspace("catdesk-external-mcp-idle");
        let server_path = write_mock_server(&workspace_root);
        let mut lazy_server = mock_stdio_server(&server_path);
        lazy_server.lifecycle = "lazy".to_string();
        let mut keep_server = mock_stdio_server(&server_path);
        keep_server.lifecycle = "keep-alive".to_string();

        let mut servers = HashMap::new();
        servers.insert("lazy".to_string(), lazy_server);
        servers.insert("keep".to_string(), keep_server);
        let mut manager = ExternalMcpManager::with_workspace(
            ExternalMcpConfig {
                mcp_servers: servers,
                ..ExternalMcpConfig::default()
            },
            workspace_root.clone(),
        );

        manager.connect("lazy").await.expect("connect lazy server");
        manager.connect("keep").await.expect("connect keep server");
        manager.mark_connection_idle_for_test("lazy", Duration::from_secs(11 * 60));
        manager.mark_connection_idle_for_test("keep", Duration::from_secs(11 * 60));
        let reaped = manager
            .reap_idle_connections()
            .expect("reap idle connections");

        assert_eq!(reaped, vec!["lazy".to_string()]);
        assert_eq!(manager.connected_server_count(), 1);
        assert!(
            manager
                .status_payload()
                .get("servers")
                .and_then(Value::as_array)
                .and_then(|servers| servers
                    .iter()
                    .find(|server| server.get("name").and_then(Value::as_str) == Some("keep")))
                .and_then(|server| server.get("connected"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        );

        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn shutdown_all_disconnects_all_servers() {
        let workspace_root = unique_workspace("catdesk-external-mcp-shutdown");
        let server_path = write_mock_server(&workspace_root);
        let mut manager =
            mock_manager_with_server(mock_stdio_server(&server_path), &workspace_root);

        manager.connect("mock").await.expect("connect mock server");
        assert_eq!(manager.connected_server_count(), 1);
        let output = manager.shutdown_all().await;
        assert_eq!(
            output.get("action").and_then(Value::as_str),
            Some("shutdown")
        );
        assert_eq!(
            output.get("connectedCount").and_then(Value::as_u64),
            Some(0)
        );
        assert_eq!(manager.connected_server_count(), 0);

        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn describe_output_includes_proxy_call_example() {
        let mut servers = HashMap::new();
        servers.insert(
            "mock".to_string(),
            ExternalMcpServer {
                command: Some("mock".to_string()),
                ..ExternalMcpServer::default()
            },
        );
        let mut manager = ExternalMcpManager::new(ExternalMcpConfig {
            mcp_servers: servers,
            ..ExternalMcpConfig::default()
        });
        manager.set_cached_tools_for_test(
            "mock",
            vec![json!({
                "name": "echo",
                "description": "Echo a message",
                "inputSchema": {"type": "object", "properties": {"message": {"type": "string"}}}
            })],
        );

        let output = manager
            .proxy(&json!({"describe": "mock_echo"}))
            .await
            .expect("describe direct tool");
        let call_example = output
            .structured
            .get("matches")
            .and_then(Value::as_array)
            .and_then(|matches| matches.first())
            .and_then(|tool| tool.get("callExample"))
            .expect("missing call example");
        assert_eq!(
            call_example.get("tool").and_then(Value::as_str),
            Some("mock_echo")
        );
        assert_eq!(
            call_example.get("server").and_then(Value::as_str),
            Some("mock")
        );
    }

    #[tokio::test]
    async fn downstream_json_rpc_error_is_reported() {
        let workspace_root = unique_workspace("catdesk-external-mcp-downstream-error");
        let server_path = write_mock_server(&workspace_root);
        let mut manager =
            mock_manager_with_server(mock_stdio_server(&server_path), &workspace_root);

        let error = manager
            .call_tool("missing", json!({}), Some("mock"))
            .await
            .expect_err("missing downstream tool should fail");
        assert!(
            error.contains("unknown downstream MCP tool: missing")
                || error.contains("downstream MCP error")
        );

        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn malformed_downstream_json_response_is_reported_as_timeout() {
        let workspace_root = unique_workspace("catdesk-external-mcp-malformed");
        std::fs::create_dir_all(&workspace_root).expect("create workspace");
        let server_path = workspace_root.join("malformed_mcp_server.py");
        std::fs::write(
            &server_path,
            r#"
import sys
for line in sys.stdin:
    print("not-json", flush=True)
    break
"#,
        )
        .expect("write malformed server");
        let mut manager =
            mock_manager_with_server(mock_stdio_server(&server_path), &workspace_root);

        let error = manager
            .connect("mock")
            .await
            .expect_err("malformed response should fail");
        assert!(
            error.contains("malformed JSON")
                || error.contains("timed out")
                || error.contains("closed")
        );

        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn downstream_process_exit_before_response_is_reported() {
        let workspace_root = unique_workspace("catdesk-external-mcp-exit");
        std::fs::create_dir_all(&workspace_root).expect("create workspace");
        let server_path = workspace_root.join("exit_mcp_server.py");
        std::fs::write(&server_path, "import sys\nsys.exit(0)\n").expect("write exit server");
        let mut manager =
            mock_manager_with_server(mock_stdio_server(&server_path), &workspace_root);

        let error = manager
            .connect("mock")
            .await
            .expect_err("early exit should fail");
        assert!(
            error.contains("closed")
                || error.contains("timed out")
                || error.contains("Broken pipe")
        );

        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn direct_tool_descriptors_obey_global_and_server_allowlists() {
        let workspace_root = unique_workspace("catdesk-external-mcp-direct");
        let server_path = write_mock_server(&workspace_root);
        let mut server = mock_stdio_server(&server_path);
        server.direct_tools = Some(DirectToolsConfig::Names(vec!["echo".to_string()]));
        let mut manager = mock_manager_with_server(server, &workspace_root);

        let descriptors = manager
            .direct_tool_descriptors(false)
            .await
            .expect("direct descriptors");
        let names = descriptors
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["mock_echo"]);

        let call = manager
            .call_direct_tool("mock_echo", json!({"message":"direct"}), false)
            .await
            .expect("direct call result")
            .expect("direct tool matched");
        assert_eq!(call.original_name, "echo");
        assert_eq!(
            call.result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|content| content.first())
                .and_then(|entry| entry.get("text"))
                .and_then(Value::as_str),
            Some("direct")
        );

        let no_match = manager
            .call_direct_tool("mock_status", json!({}), false)
            .await
            .expect("direct call lookup");
        assert!(no_match.is_none());

        let _ = std::fs::remove_dir_all(workspace_root);
    }
}
