use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::browser::DetectedBrowser;
use crate::mascot::{self, MascotPack};
use crate::theme;

/// Log entry displayed in the TUI.
#[derive(Clone)]
pub struct LogEntry {
    pub time: String,
    pub level: &'static str,
    pub message: String,
}

/// Per-session MCP request flow rendered as a single timeline line.
#[derive(Clone)]
pub struct SessionFlow {
    pub session_id: String,
    pub short_id: String,
    pub events: Vec<String>,
    pub anim_queue: VecDeque<FlowAnimSegment>,
    pub last_direction: FlowDirection,
    pub closing_started_ms: Option<u128>,
    pub closing_step_ms: u64,
}

const APP_CONFIG_DIR_NAME: &str = ".catdesk";
const APP_CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub tool_call_count: u64,
}

impl UsageTotals {
    pub fn accumulate(&mut self, input_tokens: u64, output_tokens: u64, tool_call_count: u64) {
        self.input_tokens = self.input_tokens.saturating_add(input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(output_tokens);
        self.total_tokens = self.input_tokens.saturating_add(self.output_tokens);
        self.tool_call_count = self.tool_call_count.saturating_add(tool_call_count);
    }

    fn normalized(mut self) -> Self {
        self.total_tokens = self.input_tokens.saturating_add(self.output_tokens);
        self
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub ngrok_authtoken: Option<String>,
    pub theme: String,
    pub mode: Mode,
    pub tool_mode: ToolMode,
    pub usage_totals: UsageTotals,
    pub selected_browser: Option<DetectedBrowser>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            ngrok_authtoken: None,
            theme: theme::DEFAULT_THEME_ID.to_string(),
            mode: Mode::Both,
            tool_mode: ToolMode::OneTool,
            usage_totals: UsageTotals::default(),
            selected_browser: None,
        }
    }
}

impl AppConfig {
    fn normalized(mut self) -> Self {
        self.ngrok_authtoken = self
            .ngrok_authtoken
            .take()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.usage_totals = self.usage_totals.normalized();
        self
    }

    fn load_from_path(path: &Path) -> std::io::Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let config = toml::from_str::<Self>(&text).map_err(std::io::Error::other)?;
        Ok(config.normalized())
    }

    fn save_to_path(&self, path: &Path) -> std::io::Result<()> {
        let config = self.clone().normalized();
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::other("failed to resolve config directory for config.toml")
        })?;
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }

        let text = toml::to_string_pretty(&config).map_err(std::io::Error::other)?;
        let mut options = OpenOptions::new();
        options.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        use std::io::Write as _;
        file.write_all(text.as_bytes())?;
        file.flush()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

/// Direction for session flow animation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FlowDirection {
    Forward,  // request: Your computer -> ChatGPT Web
    Backward, // response: ChatGPT Web -> Your computer
}

/// Per-session queued animation segment.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FlowAnimKind {
    Move,
    Turn,
}

#[derive(Clone, Copy)]
pub struct FlowAnimSegment {
    pub kind: FlowAnimKind,
    pub direction: FlowDirection,
    pub started_ms: u128,
    pub ends_ms: u128,
    pub step_ms: u64,
}

/// Which MCP backends to enable.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    Computer, // run_command only
    Browser,  // chrome-devtools-mcp only
    Both,     // both
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Computer => "Computer",
            Mode::Browser => "Browser",
            Mode::Both => "Both",
        }
    }
    pub fn computer_enabled(self) -> bool {
        matches!(self, Mode::Computer | Mode::Both)
    }
    pub fn browser_enabled(self) -> bool {
        matches!(self, Mode::Browser | Mode::Both)
    }
}

/// Which local toolset to expose in MCP.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolMode {
    OneTool,    // only run_command
    MultiTools, // codex/claude-style workspace tools
    ReadOnly,   // read-only safe tools only
}

impl ToolMode {
    pub fn all() -> &'static [Self] {
        const TOOL_MODES: [ToolMode; 3] =
            [ToolMode::OneTool, ToolMode::MultiTools, ToolMode::ReadOnly];
        &TOOL_MODES
    }

    pub fn label(self) -> &'static str {
        match self {
            ToolMode::OneTool => "1-tool",
            ToolMode::MultiTools => "multi-tools",
            ToolMode::ReadOnly => "read-only",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ToolMode::OneTool => "Expose only the run_command tool.",
            ToolMode::MultiTools => "Expose workspace read/write tools plus run_command.",
            ToolMode::ReadOnly => "Expose safe read-only workspace tools only.",
        }
    }

    pub fn run_command_enabled(self) -> bool {
        matches!(self, ToolMode::OneTool | ToolMode::MultiTools)
    }

    pub fn read_tools_enabled(self) -> bool {
        matches!(self, ToolMode::MultiTools | ToolMode::ReadOnly)
    }

    pub fn write_tools_enabled(self) -> bool {
        matches!(self, ToolMode::MultiTools)
    }

    pub fn read_only(self) -> bool {
        matches!(self, ToolMode::ReadOnly)
    }
}

/// Shared application state across server, ngrok, and TUI.
pub struct AppState {
    pub theme: String,
    pub mode: Mode,
    pub tool_mode: ToolMode,
    pub mcp_slug: String,
    pub server_running: bool,
    pub ngrok_running: bool,
    pub ngrok_url: Option<String>,
    pub remote_connected: bool,
    pub last_remote_activity_ms: Option<u128>,
    pub devtools_running: bool,
    pub port: u16,
    pub workspace_root: String,
    pub mascot: MascotPack,
    pub detected_browsers: Vec<DetectedBrowser>,
    pub selected_browser: Option<DetectedBrowser>,
    pub logs: Vec<LogEntry>,
    pub session_flows: Vec<SessionFlow>,
    pub session_count: usize,
    pub request_count: u64,
    pub usage_totals: UsageTotals,
    config_path: PathBuf,
    pub server_handle: Option<tokio::task::JoinHandle<()>>,
    pub ngrok_task: Option<tokio::task::JoinHandle<()>>,
    pub remote_browser_child: Option<tokio::process::Child>,
    pub devtools_child: Option<tokio::process::Child>,
}

pub type SharedState = Arc<Mutex<AppState>>;

pub const FLOW_ANIM_CELLS: usize = 32;
const FLOW_LINK_CELLS: u64 = FLOW_ANIM_CELLS as u64;
const FLOW_CHAIN_DELAY_CELLS: u64 = 3;
const FLOW_ANIMATION_DURATION_MS: u64 = 250;
const FLOW_STEP_FIXED_MS: u64 =
    (FLOW_ANIMATION_DURATION_MS + FLOW_LINK_CELLS - 1) / FLOW_LINK_CELLS;
const FLOW_TURN_TRANSITION_MS: u64 = 60;
const FLOW_CLOSE_PRUNE_MULTIPLIER: u64 = 3;

fn short_session_id(sid: &str) -> String {
    sid[..sid.len().min(8)].to_string()
}

pub fn app_config_path() -> std::io::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        std::io::Error::other("HOME is not set; cannot resolve ~/.catdesk/config.toml")
    })?;
    Ok(PathBuf::from(home)
        .join(APP_CONFIG_DIR_NAME)
        .join(APP_CONFIG_FILE_NAME))
}

pub fn load_app_config() -> std::io::Result<AppConfig> {
    AppConfig::load_from_path(&app_config_path()?)
}

pub fn load_ngrok_authtoken() -> std::io::Result<Option<String>> {
    Ok(load_app_config()?.ngrok_authtoken)
}

pub fn save_ngrok_authtoken(token: &str) -> std::io::Result<PathBuf> {
    let path = app_config_path()?;
    let mut config = AppConfig::load_from_path(&path)?;
    config.ngrok_authtoken = Some(token.to_string());
    config.save_to_path(&path)?;
    Ok(path)
}

fn now_hms() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn now_unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn derive_flow_step_ms() -> u64 {
    FLOW_STEP_FIXED_MS
}

fn prune_finished_segments(queue: &mut VecDeque<FlowAnimSegment>, now_ms: u128) {
    while let Some(seg) = queue.front() {
        if seg.ends_ms <= now_ms {
            queue.pop_front();
        } else {
            break;
        }
    }
}

fn move_segment_duration_ms(step_ms: u64) -> u128 {
    (FLOW_LINK_CELLS + FLOW_CHAIN_DELAY_CELLS) as u128 * step_ms as u128
}

fn enqueue_flow_segment(
    queue: &mut VecDeque<FlowAnimSegment>,
    direction: FlowDirection,
    now_ms: u128,
    step_ms: u64,
) {
    prune_finished_segments(queue, now_ms);

    let mut start_ms = now_ms;
    if let Some(tail) = queue.back() {
        start_ms = start_ms.max(tail.ends_ms);
    }

    let last_move_direction = queue.iter().rev().find_map(|seg| {
        if seg.kind == FlowAnimKind::Move {
            Some(seg.direction)
        } else {
            None
        }
    });
    if let Some(last_dir) = last_move_direction {
        if last_dir != direction {
            let turn_start = start_ms;
            let turn_end = turn_start + FLOW_TURN_TRANSITION_MS as u128;
            queue.push_back(FlowAnimSegment {
                kind: FlowAnimKind::Turn,
                direction: last_dir,
                started_ms: turn_start,
                ends_ms: turn_end,
                step_ms: 0,
            });
            start_ms = turn_end;
        }
    }

    let move_end = start_ms + move_segment_duration_ms(step_ms);
    queue.push_back(FlowAnimSegment {
        kind: FlowAnimKind::Move,
        direction,
        started_ms: start_ms,
        ends_ms: move_end,
        step_ms,
    });
}

impl AppState {
    pub fn new(port: u16, workspace_root: String) -> std::io::Result<Self> {
        let config_path = app_config_path()?;
        Self::from_config_path(port, workspace_root, config_path)
    }

    fn from_config_path(
        port: u16,
        workspace_root: String,
        config_path: PathBuf,
    ) -> std::io::Result<Self> {
        let config = AppConfig::load_from_path(&config_path)?;
        Ok(Self {
            theme: config.theme,
            mode: config.mode,
            tool_mode: config.tool_mode,
            mcp_slug: generate_mcp_slug(),
            server_running: false,
            ngrok_running: false,
            ngrok_url: None,
            remote_connected: false,
            last_remote_activity_ms: None,
            devtools_running: false,
            port,
            mascot: mascot::build_workspace_mascot(&workspace_root),
            workspace_root,
            detected_browsers: Vec::new(),
            selected_browser: config.selected_browser,
            logs: Vec::new(),
            session_flows: Vec::new(),
            session_count: 0,
            request_count: 0,
            usage_totals: config.usage_totals,
            config_path,
            server_handle: None,
            ngrok_task: None,
            remote_browser_child: None,
            devtools_child: None,
        })
    }

    pub fn current_theme(&self) -> &'static theme::ThemeDef {
        theme::resolve(&self.theme)
    }

    pub fn mcp_path(&self) -> String {
        format!("/{}/mcp", self.mcp_slug)
    }

    pub fn public_mcp_url(&self) -> Option<String> {
        self.ngrok_url
            .as_ref()
            .map(|url| format!("{url}{}", self.mcp_path()))
    }

    pub fn log(&mut self, level: &'static str, message: String) {
        let now = now_hms();
        self.logs.push(LogEntry {
            time: now,
            level,
            message,
        });
        if self.logs.len() > 500 {
            self.logs.remove(0);
        }
    }

    fn app_config(&self) -> std::io::Result<AppConfig> {
        let mut config = AppConfig::load_from_path(&self.config_path)?;
        config.theme = self.theme.clone();
        config.mode = self.mode;
        config.tool_mode = self.tool_mode;
        config.usage_totals = self.usage_totals.clone().normalized();
        config.selected_browser = self.selected_browser.clone();
        Ok(config.normalized())
    }

    pub fn persist_state(&self) -> std::io::Result<()> {
        self.app_config()?.save_to_path(&self.config_path)
    }

    pub fn persist_state_with_log(&mut self) {
        if let Err(e) = self.persist_state() {
            self.log("WARN", format!("Failed to persist app state: {e}"));
        }
    }
}

impl AppState {
    pub fn record_session_flow(&mut self, sid: &str, events: &[String], direction: FlowDirection) {
        if events.is_empty() {
            return;
        }
        let now_ms = now_unix_millis();
        self.last_remote_activity_ms = Some(now_ms);
        let step_ms = derive_flow_step_ms();

        if let Some(idx) = self
            .session_flows
            .iter()
            .position(|flow| flow.session_id == sid)
        {
            let mut flow = self.session_flows.remove(idx);
            flow.events.extend(events.iter().cloned());
            if flow.events.len() > 12 {
                let drop_n = flow.events.len() - 12;
                flow.events.drain(0..drop_n);
            }
            flow.closing_started_ms = None;
            flow.closing_step_ms = 0;
            flow.last_direction = direction;
            enqueue_flow_segment(&mut flow.anim_queue, direction, now_ms, step_ms);
            self.session_flows.insert(0, flow);
            return;
        }

        let mut trimmed = events.to_vec();
        if trimmed.len() > 12 {
            trimmed = trimmed[trimmed.len() - 12..].to_vec();
        }
        self.session_flows.insert(
            0,
            SessionFlow {
                session_id: sid.to_string(),
                short_id: short_session_id(sid),
                events: trimmed,
                anim_queue: VecDeque::new(),
                last_direction: direction,
                closing_started_ms: None,
                closing_step_ms: 0,
            },
        );
        if let Some(flow) = self.session_flows.first_mut() {
            enqueue_flow_segment(&mut flow.anim_queue, direction, now_ms, step_ms);
        }
    }

    pub fn begin_session_flow_close(&mut self, sid: &str) {
        let now_ms = now_unix_millis();
        if let Some(flow) = self
            .session_flows
            .iter_mut()
            .find(|flow| flow.session_id == sid)
        {
            if flow.closing_started_ms.is_none() {
                flow.closing_started_ms = Some(now_ms);
                flow.closing_step_ms = flow
                    .anim_queue
                    .back()
                    .map(|seg| seg.step_ms.max(1))
                    .unwrap_or_else(derive_flow_step_ms);
                flow.anim_queue.clear();
            }
        }
    }

    pub fn prune_closed_session_flows(&mut self) {
        let now_ms = now_unix_millis();
        for flow in &mut self.session_flows {
            prune_finished_segments(&mut flow.anim_queue, now_ms);
        }
        self.session_flows.retain(|flow| {
            let Some(closing_started_ms) = flow.closing_started_ms else {
                return true;
            };
            let step_ms = flow.closing_step_ms.max(1) as u128;
            let ttl_ms = (FLOW_LINK_CELLS * FLOW_CLOSE_PRUNE_MULTIPLIER) as u128 * step_ms;
            now_ms.saturating_sub(closing_started_ms) < ttl_ms
        });
    }
}

fn generate_mcp_slug() -> String {
    let random = Uuid::new_v4();
    URL_SAFE_NO_PAD.encode(&random.as_bytes()[..12])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_state_loads_persisted_config_file() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("catdesk-config-load-{unique}"));
        std::fs::create_dir_all(&workspace).expect("create temp workspace");
        let config_path = workspace.join(APP_CONFIG_FILE_NAME);
        std::fs::write(
            &config_path,
            r#"theme = "neon"
mode = "browser"
toolMode = "multiTools"

[usageTotals]
inputTokens = 120
outputTokens = 34
totalTokens = 154
toolCallCount = 7
"#,
        )
        .expect("write config file");

        let app = AppState::from_config_path(
            8787,
            workspace.to_string_lossy().into_owned(),
            config_path.clone(),
        )
        .expect("load app state");

        assert_eq!(app.theme, "neon");
        assert!(matches!(app.mode, Mode::Browser));
        assert!(matches!(app.tool_mode, ToolMode::MultiTools));
        assert_eq!(app.usage_totals.input_tokens, 120);
        assert_eq!(app.usage_totals.output_tokens, 34);
        assert_eq!(app.usage_totals.total_tokens, 154);
        assert_eq!(app.usage_totals.tool_call_count, 7);

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir(workspace);
    }

    #[test]
    fn persist_state_writes_single_config_file() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("catdesk-config-save-{unique}"));
        std::fs::create_dir_all(&workspace).expect("create temp workspace");
        let config_path = workspace.join(APP_CONFIG_FILE_NAME);

        let mut app = AppState::from_config_path(
            8787,
            workspace.to_string_lossy().into_owned(),
            config_path.clone(),
        )
        .expect("create app state");
        app.theme = "neon".into();
        app.mode = Mode::Computer;
        app.tool_mode = ToolMode::ReadOnly;
        app.usage_totals.accumulate(12, 8, 3);
        app.persist_state().expect("persist state");

        let saved = AppConfig::load_from_path(&config_path).expect("load config file");
        assert_eq!(saved.theme, "neon");
        assert!(matches!(saved.mode, Mode::Computer));
        assert!(matches!(saved.tool_mode, ToolMode::ReadOnly));
        assert_eq!(saved.usage_totals.input_tokens, 12);
        assert_eq!(saved.usage_totals.output_tokens, 8);
        assert_eq!(saved.usage_totals.total_tokens, 20);
        assert_eq!(saved.usage_totals.tool_call_count, 3);

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir(workspace);
    }

    #[test]
    fn app_config_round_trips_ngrok_authtoken() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("catdesk-config-token-{unique}"));
        std::fs::create_dir_all(&workspace).expect("create temp config dir");
        let config_path = workspace.join(APP_CONFIG_FILE_NAME);

        let config = AppConfig {
            ngrok_authtoken: Some("test-token-123".into()),
            ..AppConfig::default()
        };
        config.save_to_path(&config_path).expect("save config");

        let saved = AppConfig::load_from_path(&config_path).expect("load config");
        assert_eq!(saved.ngrok_authtoken.as_deref(), Some("test-token-123"));

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir(workspace);
    }
}
