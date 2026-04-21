use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
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

/// MCP request flow rendered as a single timeline line.
#[derive(Clone)]
pub struct FlowLane {
    pub flow_id: String,
    pub short_id: String,
    pub events: Vec<String>,
    pub bootstrap_status_active: bool,
    pub bootstrap_completed_steps: usize,
    pub bootstrap_pending_steps: VecDeque<usize>,
    pub bootstrap_status_close_deadline_ms: Option<u128>,
    pub anim_queue: VecDeque<FlowAnimSegment>,
    pub last_direction: FlowDirection,
    pub closing_started_ms: Option<u128>,
    pub closing_step_ms: u64,
}

#[derive(Clone, Default)]
pub struct FlowBootstrapProgress {
    pub completed_steps: usize,
    pub pending_steps: VecDeque<usize>,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentsPathMode {
    #[default]
    Default,
    Workspace,
    Catdesk,
    Codex,
    Disabled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenStatsLayout {
    Disable,
    #[default]
    Right,
    Bottom,
}

impl TokenStatsLayout {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Right => "right",
            Self::Bottom => "bottom",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShowDetailMode {
    Disable,
    #[default]
    Expanded,
    Collapsed,
}

impl ShowDetailMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Expanded => "expanded",
            Self::Collapsed => "collapsed",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub ngrok_authtoken: Option<String>,
    #[serde(default)]
    pub agents_path_mode: AgentsPathMode,
    #[serde(default)]
    pub token_stats_layout: TokenStatsLayout,
    #[serde(default)]
    pub show_detail_mode: ShowDetailMode,
    #[serde(default)]
    pub partner_binagotchy_seed: Option<String>,
    #[serde(default)]
    pub set_catdesk_as_co_author: bool,
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
            agents_path_mode: AgentsPathMode::Default,
            token_stats_layout: TokenStatsLayout::Right,
            show_detail_mode: ShowDetailMode::Expanded,
            partner_binagotchy_seed: None,
            set_catdesk_as_co_author: false,
            theme: theme::DEFAULT_THEME_ID.to_string(),
            mode: Mode::Both,
            tool_mode: ToolMode::MultiTools,
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
        self.partner_binagotchy_seed = self
            .partner_binagotchy_seed
            .take()
            .map(|value| value.trim().to_ascii_lowercase())
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

/// Direction for flow animation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FlowDirection {
    Forward,  // request: Your computer -> ChatGPT Web
    Backward, // response: ChatGPT Web -> Your computer
}

pub enum ServerUiEvent {
    IncrementRequestCount,
    SetRemoteConnected(bool),
    RecordFlow {
        flow_id: String,
        events: Vec<String>,
        direction: FlowDirection,
    },
    BeginFlowClose {
        flow_id: String,
    },
    Log {
        level: &'static str,
        message: String,
    },
}

/// Per-flow queued animation segment.
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
    pub start_cells: usize,
    pub end_cells: usize,
}

#[derive(Clone, Copy)]
pub struct FlowBootstrapStep {
    pub event: &'static str,
    pub label: &'static str,
}

#[derive(Clone, Copy)]
pub struct FlowBootstrapPhase {
    pub title: &'static str,
    pub steps: &'static [FlowBootstrapStep],
}

const FLOW_BOOTSTRAP_PHASE_1_STEPS: &[FlowBootstrapStep] = &[
    FlowBootstrapStep {
        event: "initialize",
        label: "initialize#1",
    },
    FlowBootstrapStep {
        event: "initialize",
        label: "initialize#2",
    },
    FlowBootstrapStep {
        event: "notifications/initialized",
        label: "initialized",
    },
    FlowBootstrapStep {
        event: "tools/list",
        label: "tools/list",
    },
];

const FLOW_BOOTSTRAP_PHASE_2_STEPS: &[FlowBootstrapStep] = &[
    FlowBootstrapStep {
        event: "initialize",
        label: "initialize#1",
    },
    FlowBootstrapStep {
        event: "initialize",
        label: "initialize#2",
    },
    FlowBootstrapStep {
        event: "notifications/initialized",
        label: "initialized",
    },
    FlowBootstrapStep {
        event: "resources/list",
        label: "resources/list",
    },
];

const FLOW_BOOTSTRAP_PHASE_3_STEPS: &[FlowBootstrapStep] = &[
    FlowBootstrapStep {
        event: "initialize",
        label: "initialize#1",
    },
    FlowBootstrapStep {
        event: "initialize",
        label: "initialize#2",
    },
    FlowBootstrapStep {
        event: "notifications/initialized",
        label: "initialized",
    },
    FlowBootstrapStep {
        event: "resources/read",
        label: "dashboard.html",
    },
];

const FLOW_BOOTSTRAP_PHASE_4_STEPS: &[FlowBootstrapStep] = &[
    FlowBootstrapStep {
        event: "initialize",
        label: "initialize#1",
    },
    FlowBootstrapStep {
        event: "initialize",
        label: "initialize#2",
    },
    FlowBootstrapStep {
        event: "notifications/initialized",
        label: "initialized",
    },
    FlowBootstrapStep {
        event: "tools/list",
        label: "tools/list",
    },
    FlowBootstrapStep {
        event: "resources/read",
        label: "dashboard.html",
    },
];

const FLOW_BOOTSTRAP_PHASE_5_STEPS: &[FlowBootstrapStep] = &[
    FlowBootstrapStep {
        event: "initialize",
        label: "initialize#1",
    },
    FlowBootstrapStep {
        event: "initialize",
        label: "initialize#2",
    },
    FlowBootstrapStep {
        event: "notifications/initialized",
        label: "initialized",
    },
    FlowBootstrapStep {
        event: "resources/list",
        label: "resources/list",
    },
];

pub const FLOW_BOOTSTRAP_PHASES: &[FlowBootstrapPhase] = &[
    FlowBootstrapPhase {
        title: "Checking tools",
        steps: FLOW_BOOTSTRAP_PHASE_1_STEPS,
    },
    FlowBootstrapPhase {
        title: "Checking resources",
        steps: FLOW_BOOTSTRAP_PHASE_2_STEPS,
    },
    FlowBootstrapPhase {
        title: "Loading widgets",
        steps: FLOW_BOOTSTRAP_PHASE_3_STEPS,
    },
    FlowBootstrapPhase {
        title: "Refreshing widgets",
        steps: FLOW_BOOTSTRAP_PHASE_4_STEPS,
    },
    FlowBootstrapPhase {
        title: "Final resource check",
        steps: FLOW_BOOTSTRAP_PHASE_5_STEPS,
    },
];

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
    MultiTools, // codex/claude-style workspace tools
    ReadOnly,   // read-only safe tools only
}

impl ToolMode {
    pub fn all() -> &'static [Self] {
        const TOOL_MODES: [ToolMode; 2] = [ToolMode::MultiTools, ToolMode::ReadOnly];
        &TOOL_MODES
    }

    pub fn label(self) -> &'static str {
        match self {
            ToolMode::MultiTools => "multi-tools",
            ToolMode::ReadOnly => "read-only",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ToolMode::MultiTools => "Expose workspace read/write tools plus run_command.",
            ToolMode::ReadOnly => "Expose safe read-only workspace tools only.",
        }
    }

    pub fn run_command_enabled(self) -> bool {
        matches!(self, ToolMode::MultiTools)
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
    pub mascot_seed: u64,
    pub partner_binagotchy_seed: Option<String>,
    pub set_catdesk_as_co_author: bool,
    pub mascot: MascotPack,
    pub detected_browsers: Vec<DetectedBrowser>,
    pub selected_browser: Option<DetectedBrowser>,
    pub logs: Vec<LogEntry>,
    pub flows: Vec<FlowLane>,
    pub flow_bootstrap_progress: HashMap<String, FlowBootstrapProgress>,
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
const FLOW_CHAIN_DELAY_CELLS: u64 = 0;
const FLOW_FORWARD_ANIMATION_DURATION_MS: u64 = 125;
const FLOW_BACKWARD_ANIMATION_DURATION_MS: u64 = 125;
const FLOW_STEP_FIXED_MS: u64 =
    (FLOW_FORWARD_ANIMATION_DURATION_MS + FLOW_LINK_CELLS - 1) / FLOW_LINK_CELLS;
const FLOW_TURN_TRANSITION_MS: u64 = 24;
const FLOW_CLOSE_PRUNE_MULTIPLIER: u64 = 3;
const FLOW_BOOTSTRAP_STATUS_CLOSE_DELAY_MS: u128 = 3_000;

fn short_flow_id(flow_id: &str) -> String {
    flow_id[..flow_id.len().min(8)].to_string()
}

pub fn user_home_dir() -> std::io::Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home));
    }

    #[cfg(windows)]
    {
        if let Some(user_profile) =
            std::env::var_os("USERPROFILE").filter(|value| !value.is_empty())
        {
            return Ok(PathBuf::from(user_profile));
        }

        let home_drive = std::env::var_os("HOMEDRIVE").filter(|value| !value.is_empty());
        let home_path = std::env::var_os("HOMEPATH").filter(|value| !value.is_empty());
        if let (Some(home_drive), Some(home_path)) = (home_drive, home_path) {
            let mut path = PathBuf::from(home_drive);
            path.push(home_path);
            return Ok(path);
        }
    }

    Err(std::io::Error::other(
        "could not resolve the user home directory from HOME, USERPROFILE, or HOMEDRIVE/HOMEPATH",
    ))
}

pub fn app_config_path() -> std::io::Result<PathBuf> {
    Ok(user_home_dir()?
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

pub fn save_agents_path_mode(mode: AgentsPathMode) -> std::io::Result<PathBuf> {
    let path = app_config_path()?;
    let mut config = AppConfig::load_from_path(&path)?;
    config.agents_path_mode = mode;
    config.save_to_path(&path)?;
    Ok(path)
}

pub fn save_token_stats_layout(layout: TokenStatsLayout) -> std::io::Result<PathBuf> {
    let path = app_config_path()?;
    let mut config = AppConfig::load_from_path(&path)?;
    config.token_stats_layout = layout;
    config.save_to_path(&path)?;
    Ok(path)
}

pub fn save_show_detail_mode(mode: ShowDetailMode) -> std::io::Result<PathBuf> {
    let path = app_config_path()?;
    let mut config = AppConfig::load_from_path(&path)?;
    config.show_detail_mode = mode;
    config.save_to_path(&path)?;
    Ok(path)
}

pub(crate) fn parse_seed_hex(seed: &str) -> std::io::Result<u64> {
    u64::from_str_radix(seed, 16).map_err(|error| {
        std::io::Error::other(format!("invalid partner Binagotchy seed `{seed}`: {error}"))
    })
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

fn current_queue_segment(
    queue: &VecDeque<FlowAnimSegment>,
    now_ms: u128,
) -> Option<FlowAnimSegment> {
    if let Some(seg) = queue
        .iter()
        .find(|seg| seg.started_ms <= now_ms && now_ms < seg.ends_ms)
    {
        return Some(*seg);
    }
    queue.front().copied()
}

pub(crate) fn flow_anim_lit_count(seg: FlowAnimSegment, now_ms: u128) -> usize {
    if seg.started_ms >= seg.ends_ms {
        return seg.end_cells;
    }
    if now_ms <= seg.started_ms {
        return seg.start_cells;
    }
    if now_ms >= seg.ends_ms {
        return seg.end_cells;
    }

    let duration_ms = seg.ends_ms.saturating_sub(seg.started_ms);
    if duration_ms == 0 {
        return seg.end_cells;
    }

    let elapsed_ms = now_ms.saturating_sub(seg.started_ms);
    let distance = seg.end_cells.abs_diff(seg.start_cells) as u128;
    let progressed = ((distance * elapsed_ms) / duration_ms) as usize;

    if seg.end_cells >= seg.start_cells {
        (seg.start_cells + progressed).min(seg.end_cells)
    } else {
        seg.start_cells
            .saturating_sub(progressed.min(seg.start_cells - seg.end_cells))
    }
}

fn move_segment_duration_ms(
    direction: FlowDirection,
    _step_ms: u64,
    start_cells: usize,
    end_cells: usize,
) -> u128 {
    let cells_to_travel = end_cells.abs_diff(start_cells) as u128;
    if cells_to_travel == 0 {
        return 0;
    }
    let base_duration_ms = match direction {
        FlowDirection::Forward => FLOW_FORWARD_ANIMATION_DURATION_MS as u128,
        FlowDirection::Backward => FLOW_BACKWARD_ANIMATION_DURATION_MS as u128,
    };
    ((cells_to_travel + FLOW_CHAIN_DELAY_CELLS as u128) * base_duration_ms)
        .div_ceil(FLOW_LINK_CELLS as u128)
}

fn enqueue_flow_segment(
    queue: &mut VecDeque<FlowAnimSegment>,
    direction: FlowDirection,
    now_ms: u128,
    step_ms: u64,
) {
    prune_finished_segments(queue, now_ms);

    let current_seg = current_queue_segment(queue, now_ms);
    let current_direction = current_seg
        .map(|seg| seg.direction)
        .or_else(|| queue.back().map(|seg| seg.direction));
    let current_cells = current_seg
        .map(|seg| flow_anim_lit_count(seg, now_ms))
        .or_else(|| queue.back().map(|seg| seg.end_cells))
        .unwrap_or(0)
        .min(FLOW_ANIM_CELLS);

    queue.clear();

    let mut start_ms = now_ms;
    let mut move_start_cells = 0usize;

    if let Some(current_direction) = current_direction {
        if current_direction == direction {
            move_start_cells = current_cells;
        } else if current_cells > 0 {
            let turn_end = start_ms + FLOW_TURN_TRANSITION_MS as u128;
            queue.push_back(FlowAnimSegment {
                kind: FlowAnimKind::Turn,
                direction: current_direction,
                started_ms: start_ms,
                ends_ms: turn_end,
                step_ms,
                start_cells: current_cells,
                end_cells: 0,
            });
            start_ms = turn_end;
        }
    }

    let move_end =
        start_ms + move_segment_duration_ms(direction, step_ms, move_start_cells, FLOW_ANIM_CELLS);
    if move_end > start_ms {
        queue.push_back(FlowAnimSegment {
            kind: FlowAnimKind::Move,
            direction,
            started_ms: start_ms,
            ends_ms: move_end,
            step_ms,
            start_cells: move_start_cells,
            end_cells: FLOW_ANIM_CELLS,
        });
    }
}

fn flow_bootstrap_step(index: usize) -> Option<&'static FlowBootstrapStep> {
    let mut offset = 0;
    for phase in FLOW_BOOTSTRAP_PHASES {
        let end = offset + phase.steps.len();
        if index < end {
            return phase.steps.get(index - offset);
        }
        offset = end;
    }
    None
}

fn flow_bootstrap_steps_total() -> usize {
    FLOW_BOOTSTRAP_PHASES
        .iter()
        .map(|phase| phase.steps.len())
        .sum()
}

fn events_start_bootstrap_status(events: &[String]) -> bool {
    events.iter().any(|event| event == "initialize")
}

fn is_bootstrap_status_event(event: &str) -> bool {
    FLOW_BOOTSTRAP_PHASES
        .iter()
        .flat_map(|phase| phase.steps)
        .any(|step| step.event == event)
}

fn events_are_bootstrap_status_events(events: &[String]) -> bool {
    events.iter().all(|event| is_bootstrap_status_event(event))
}

fn advance_bootstrap_progress(
    completed_steps: &mut usize,
    pending_steps: &mut VecDeque<usize>,
    events: &[String],
    direction: FlowDirection,
) {
    match direction {
        FlowDirection::Forward => {
            for event in events {
                let next_index = completed_steps.saturating_add(pending_steps.len());
                let Some(step) = flow_bootstrap_step(next_index) else {
                    break;
                };
                if step.event != event {
                    continue;
                }
                if step.event == "notifications/initialized" {
                    *completed_steps = next_index + 1;
                    continue;
                }
                pending_steps.push_back(next_index);
            }
        }
        FlowDirection::Backward => {
            for event in events {
                let Some(pending_index) = pending_steps.front().copied() else {
                    break;
                };
                let Some(step) = flow_bootstrap_step(pending_index) else {
                    pending_steps.clear();
                    break;
                };
                if step.event == event {
                    pending_steps.pop_front();
                    *completed_steps = pending_index + 1;
                }
            }
        }
    }
}

impl AppState {
    pub fn new(port: u16, workspace_root: String) -> std::io::Result<Self> {
        let config_path = app_config_path()?;
        Self::from_config_path(port, workspace_root, config_path)
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        port: u16,
        workspace_root: String,
        config_path: PathBuf,
    ) -> std::io::Result<Self> {
        Self::from_config_path(port, workspace_root, config_path)
    }

    fn from_config_path(
        port: u16,
        workspace_root: String,
        config_path: PathBuf,
    ) -> std::io::Result<Self> {
        let config = AppConfig::load_from_path(&config_path)?;
        let partner_binagotchy_seed = config.partner_binagotchy_seed.clone();
        let mascot_seed = if let Some(seed) = partner_binagotchy_seed.as_deref() {
            parse_seed_hex(seed)?
        } else {
            rand::random::<u64>()
        };
        let mascot = mascot::build_workspace_mascot(mascot_seed);
        #[cfg(not(test))]
        if partner_binagotchy_seed.is_none() {
            mascot::archive_startup_mascot(mascot_seed)?;
        }
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
            mascot_seed,
            partner_binagotchy_seed,
            set_catdesk_as_co_author: config.set_catdesk_as_co_author,
            mascot,
            workspace_root,
            detected_browsers: Vec::new(),
            selected_browser: config.selected_browser,
            logs: Vec::new(),
            flows: Vec::new(),
            flow_bootstrap_progress: HashMap::new(),
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
        config.partner_binagotchy_seed = self.partner_binagotchy_seed.clone();
        config.set_catdesk_as_co_author = self.set_catdesk_as_co_author;
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

    pub fn apply_server_ui_event(&mut self, event: ServerUiEvent) {
        match event {
            ServerUiEvent::IncrementRequestCount => {
                self.request_count = self.request_count.saturating_add(1);
            }
            ServerUiEvent::SetRemoteConnected(connected) => {
                self.remote_connected = connected;
                if connected {
                    self.last_remote_activity_ms = Some(now_unix_millis());
                } else {
                    self.last_remote_activity_ms = None;
                }
            }
            ServerUiEvent::RecordFlow {
                flow_id,
                events,
                direction,
            } => {
                self.record_flow(&flow_id, &events, direction);
            }
            ServerUiEvent::BeginFlowClose { flow_id } => {
                self.begin_flow_close(&flow_id);
            }
            ServerUiEvent::Log { level, message } => {
                self.log(level, message);
            }
        }
    }
}

impl AppState {
    pub fn record_flow(&mut self, flow_id: &str, events: &[String], direction: FlowDirection) {
        if events.is_empty() {
            return;
        }
        let now_ms = now_unix_millis();
        self.last_remote_activity_ms = Some(now_ms);
        self.remote_connected = true;
        let step_ms = derive_flow_step_ms();
        let mut bootstrap = self
            .flow_bootstrap_progress
            .get(flow_id)
            .cloned()
            .unwrap_or_default();
        let starts_bootstrap_status = events_start_bootstrap_status(events);
        let only_bootstrap_status_events = events_are_bootstrap_status_events(events);

        if let Some(idx) = self.flows.iter().position(|flow| flow.flow_id == flow_id) {
            let mut flow = self.flows.remove(idx);
            if starts_bootstrap_status {
                flow.bootstrap_status_active = true;
            } else if flow.bootstrap_status_active && !only_bootstrap_status_events {
                flow.bootstrap_status_active = false;
                flow.bootstrap_status_close_deadline_ms = None;
            }
            flow.events.extend(events.iter().cloned());
            if flow.events.len() > 12 {
                let drop_n = flow.events.len() - 12;
                flow.events.drain(0..drop_n);
            }
            flow.bootstrap_completed_steps = bootstrap.completed_steps;
            flow.bootstrap_pending_steps = bootstrap.pending_steps.clone();
            advance_bootstrap_progress(
                &mut flow.bootstrap_completed_steps,
                &mut flow.bootstrap_pending_steps,
                events,
                direction,
            );
            bootstrap.completed_steps = flow.bootstrap_completed_steps;
            bootstrap.pending_steps = flow.bootstrap_pending_steps.clone();
            self.flow_bootstrap_progress
                .insert(flow_id.to_string(), bootstrap);
            flow.closing_started_ms = None;
            flow.closing_step_ms = 0;
            flow.bootstrap_status_close_deadline_ms = None;
            flow.last_direction = direction;
            enqueue_flow_segment(&mut flow.anim_queue, direction, now_ms, step_ms);
            self.flows.insert(0, flow);
            return;
        }

        let mut trimmed = events.to_vec();
        if trimmed.len() > 12 {
            trimmed = trimmed[trimmed.len() - 12..].to_vec();
        }
        self.flows.insert(
            0,
            FlowLane {
                flow_id: flow_id.to_string(),
                short_id: short_flow_id(flow_id),
                events: trimmed,
                bootstrap_status_active: starts_bootstrap_status,
                bootstrap_completed_steps: bootstrap.completed_steps,
                bootstrap_pending_steps: bootstrap.pending_steps.clone(),
                bootstrap_status_close_deadline_ms: None,
                anim_queue: VecDeque::new(),
                last_direction: direction,
                closing_started_ms: None,
                closing_step_ms: 0,
            },
        );
        if let Some(flow) = self.flows.first_mut() {
            advance_bootstrap_progress(
                &mut flow.bootstrap_completed_steps,
                &mut flow.bootstrap_pending_steps,
                events,
                direction,
            );
            bootstrap.completed_steps = flow.bootstrap_completed_steps;
            bootstrap.pending_steps = flow.bootstrap_pending_steps.clone();
            self.flow_bootstrap_progress
                .insert(flow_id.to_string(), bootstrap);
            enqueue_flow_segment(&mut flow.anim_queue, direction, now_ms, step_ms);
        }
    }

    pub fn begin_flow_close(&mut self, flow_id: &str) {
        let now_ms = now_unix_millis();
        self.flow_bootstrap_progress.remove(flow_id);
        if let Some(flow) = self.flows.iter_mut().find(|flow| flow.flow_id == flow_id) {
            if flow.closing_started_ms.is_none() {
                flow.closing_started_ms = Some(now_ms);
                flow.closing_step_ms = flow
                    .anim_queue
                    .back()
                    .map(|seg| seg.step_ms.max(1))
                    .unwrap_or_else(derive_flow_step_ms);
                flow.anim_queue.clear();
                flow.bootstrap_status_active = false;
                flow.bootstrap_status_close_deadline_ms = None;
            }
        }
    }

    pub fn prune_closed_flows(&mut self) {
        let now_ms = now_unix_millis();
        let bootstrap_steps_total = flow_bootstrap_steps_total();

        for flow in &mut self.flows {
            prune_finished_segments(&mut flow.anim_queue, now_ms);
            if !flow.bootstrap_status_active {
                flow.bootstrap_status_close_deadline_ms = None;
                continue;
            }
            let bootstrap_complete = flow.bootstrap_completed_steps >= bootstrap_steps_total
                && flow.bootstrap_pending_steps.is_empty();
            if flow.closing_started_ms.is_none() && bootstrap_complete {
                if flow.anim_queue.is_empty() {
                    match flow.bootstrap_status_close_deadline_ms {
                        Some(deadline) if now_ms >= deadline => {
                            flow.bootstrap_status_active = false;
                            flow.bootstrap_status_close_deadline_ms = None;
                        }
                        Some(_) => {}
                        None => {
                            flow.bootstrap_status_close_deadline_ms =
                                Some(now_ms + FLOW_BOOTSTRAP_STATUS_CLOSE_DELAY_MS);
                        }
                    }
                } else {
                    flow.bootstrap_status_close_deadline_ms = None;
                }
            } else {
                flow.bootstrap_status_close_deadline_ms = None;
            }
        }
        self.flows.retain(|flow| {
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

    fn test_app(name: &str) -> (AppState, PathBuf, PathBuf) {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("{name}-{unique}"));
        std::fs::create_dir_all(&workspace).expect("create temp workspace");
        let config_path = workspace.join(APP_CONFIG_FILE_NAME);
        let app = AppState::from_config_path(
            8787,
            workspace.to_string_lossy().into_owned(),
            config_path.clone(),
        )
        .expect("create app state");
        (app, workspace, config_path)
    }

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

    #[test]
    fn app_config_round_trips_agents_path_mode() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("catdesk-config-agents-mode-{unique}"));
        std::fs::create_dir_all(&workspace).expect("create temp config dir");
        let config_path = workspace.join(APP_CONFIG_FILE_NAME);

        let config = AppConfig {
            agents_path_mode: AgentsPathMode::Codex,
            ..AppConfig::default()
        };
        config.save_to_path(&config_path).expect("save config");

        let saved = AppConfig::load_from_path(&config_path).expect("load config");
        assert!(matches!(saved.agents_path_mode, AgentsPathMode::Codex));

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir(workspace);
    }

    #[test]
    fn app_config_round_trips_token_stats_layout() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("catdesk-config-token-layout-{unique}"));
        std::fs::create_dir_all(&workspace).expect("create temp config dir");
        let config_path = workspace.join(APP_CONFIG_FILE_NAME);

        let config = AppConfig {
            token_stats_layout: TokenStatsLayout::Bottom,
            ..AppConfig::default()
        };
        config.save_to_path(&config_path).expect("save config");

        let saved = AppConfig::load_from_path(&config_path).expect("load config");
        assert!(matches!(saved.token_stats_layout, TokenStatsLayout::Bottom));

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir(workspace);
    }

    #[test]
    fn app_config_round_trips_show_detail_mode() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("catdesk-config-show-detail-{unique}"));
        std::fs::create_dir_all(&workspace).expect("create temp config dir");
        let config_path = workspace.join(APP_CONFIG_FILE_NAME);

        let config = AppConfig {
            show_detail_mode: ShowDetailMode::Collapsed,
            ..AppConfig::default()
        };
        config.save_to_path(&config_path).expect("save config");

        let saved = AppConfig::load_from_path(&config_path).expect("load config");
        assert!(matches!(saved.show_detail_mode, ShowDetailMode::Collapsed));

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir(workspace);
    }

    #[test]
    fn app_state_loads_partner_binagotchy_seed() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("catdesk-config-partner-{unique}"));
        std::fs::create_dir_all(&workspace).expect("create temp workspace");
        let config_path = workspace.join(APP_CONFIG_FILE_NAME);

        std::fs::write(
            &config_path,
            r#"
theme = "concise"
mode = "both"
toolMode = "multiTools"
partnerBinagotchySeed = "00000000000000ff"

[usageTotals]
inputTokens = 0
outputTokens = 0
totalTokens = 0
toolCallCount = 0
"#,
        )
        .expect("write config file");

        let app = AppState::from_config_path(
            8787,
            workspace.to_string_lossy().into_owned(),
            config_path.clone(),
        )
        .expect("load app state");

        assert_eq!(
            app.partner_binagotchy_seed.as_deref(),
            Some("00000000000000ff")
        );
        assert_eq!(app.mascot_seed, 0xff);

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir(workspace);
    }

    #[test]
    fn flow_anim_lit_count_interpolates_between_endpoints() {
        let duration_ms = move_segment_duration_ms(
            FlowDirection::Forward,
            derive_flow_step_ms(),
            0,
            FLOW_ANIM_CELLS,
        );
        let seg = FlowAnimSegment {
            kind: FlowAnimKind::Move,
            direction: FlowDirection::Forward,
            started_ms: 100,
            ends_ms: 100 + duration_ms,
            step_ms: derive_flow_step_ms(),
            start_cells: 0,
            end_cells: FLOW_ANIM_CELLS,
        };

        assert_eq!(flow_anim_lit_count(seg, 100), 0);
        assert!(flow_anim_lit_count(seg, 100 + duration_ms / 2) > 0);
        assert!(flow_anim_lit_count(seg, 100 + duration_ms / 2) < FLOW_ANIM_CELLS);
        assert_eq!(flow_anim_lit_count(seg, 100 + duration_ms), FLOW_ANIM_CELLS);
    }

    #[test]
    fn backward_move_uses_longer_duration() {
        let forward = move_segment_duration_ms(
            FlowDirection::Forward,
            derive_flow_step_ms(),
            0,
            FLOW_ANIM_CELLS,
        );
        let backward = move_segment_duration_ms(
            FlowDirection::Backward,
            derive_flow_step_ms(),
            0,
            FLOW_ANIM_CELLS,
        );

        assert_eq!(forward, FLOW_FORWARD_ANIMATION_DURATION_MS as u128);
        assert_eq!(backward, FLOW_BACKWARD_ANIMATION_DURATION_MS as u128);
    }

    #[test]
    fn enqueue_flow_segment_preempts_inflight_move() {
        let mut queue = VecDeque::new();
        let step_ms = derive_flow_step_ms();
        enqueue_flow_segment(&mut queue, FlowDirection::Forward, 0, step_ms);
        assert_eq!(queue.len(), 1);

        enqueue_flow_segment(&mut queue, FlowDirection::Backward, 40, step_ms);
        assert_eq!(queue.len(), 2);
        assert!(matches!(queue[0].kind, FlowAnimKind::Turn));
        assert!(queue[0].direction == FlowDirection::Forward);
        assert!(queue[0].start_cells > 0);
        assert_eq!(queue[0].end_cells, 0);
        assert!(matches!(queue[1].kind, FlowAnimKind::Move));
        assert!(queue[1].direction == FlowDirection::Backward);
        assert_eq!(queue[1].start_cells, 0);
        assert_eq!(queue[1].end_cells, FLOW_ANIM_CELLS);
    }

    #[test]
    fn record_flow_tool_call_does_not_activate_bootstrap_status() {
        let (mut app, workspace, config_path) = test_app("catdesk-flow-tool-call");

        app.record_flow(
            "stateless",
            &["tools/call:run_command".to_string()],
            FlowDirection::Forward,
        );

        let flow = app.flows.first().expect("missing flow");
        assert!(!flow.bootstrap_status_active);
        assert_eq!(flow.bootstrap_completed_steps, 0);
        assert!(flow.bootstrap_pending_steps.is_empty());

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn record_flow_initialize_activates_bootstrap_status() {
        let (mut app, workspace, config_path) = test_app("catdesk-flow-initialize");

        app.record_flow(
            "stateless",
            &["initialize".to_string()],
            FlowDirection::Forward,
        );

        let flow = app.flows.first().expect("missing flow");
        assert!(flow.bootstrap_status_active);
        assert_eq!(flow.bootstrap_completed_steps, 0);
        assert_eq!(flow.bootstrap_pending_steps.front(), Some(&0));

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn record_flow_bootstrap_event_keeps_bootstrap_status_active() {
        let (mut app, workspace, config_path) = test_app("catdesk-flow-bootstrap-event");

        app.record_flow(
            "stateless",
            &["initialize".to_string()],
            FlowDirection::Forward,
        );
        app.record_flow(
            "stateless",
            &["tools/list".to_string()],
            FlowDirection::Forward,
        );

        let flow = app.flows.first().expect("missing flow");
        assert!(flow.bootstrap_status_active);

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn record_flow_tool_call_after_initialize_deactivates_bootstrap_status() {
        let (mut app, workspace, config_path) = test_app("catdesk-flow-tool-after-initialize");

        app.record_flow(
            "stateless",
            &["initialize".to_string()],
            FlowDirection::Forward,
        );
        app.record_flow(
            "stateless",
            &["tools/call:catdesk_instruction".to_string()],
            FlowDirection::Forward,
        );

        let flow = app.flows.first().expect("missing flow");
        assert!(!flow.bootstrap_status_active);
        assert!(flow.bootstrap_status_close_deadline_ms.is_none());

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn record_flow_tool_call_after_close_does_not_reactivate_bootstrap_status() {
        let (mut app, workspace, config_path) = test_app("catdesk-flow-after-close");

        app.record_flow(
            "stateless",
            &["initialize".to_string()],
            FlowDirection::Forward,
        );
        app.begin_flow_close("stateless");
        app.record_flow(
            "stateless",
            &["tools/call:run_command".to_string()],
            FlowDirection::Forward,
        );

        let flow = app.flows.first().expect("missing flow");
        assert!(!flow.bootstrap_status_active);

        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(workspace);
    }
}
