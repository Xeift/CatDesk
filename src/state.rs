use std::collections::VecDeque;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;

use crate::browser::DetectedBrowser;
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
#[derive(Clone, Copy, PartialEq, Eq)]
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToolMode {
    OneTool,    // only run_command
    MultiTools, // codex/claude-style workspace tools
    ReadOnly,   // read-only safe tools only
}

impl ToolMode {
    pub fn label(self) -> &'static str {
        match self {
            ToolMode::OneTool => "1-tool",
            ToolMode::MultiTools => "multi-tools",
            ToolMode::ReadOnly => "read-only",
        }
    }

    pub fn next(self) -> Self {
        match self {
            ToolMode::OneTool => ToolMode::MultiTools,
            ToolMode::MultiTools => ToolMode::ReadOnly,
            ToolMode::ReadOnly => ToolMode::OneTool,
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
    pub server_running: bool,
    pub ngrok_running: bool,
    pub ngrok_url: Option<String>,
    pub remote_connected: bool,
    pub last_remote_activity_ms: Option<u128>,
    pub devtools_running: bool,
    pub port: u16,
    pub workspace_root: String,
    pub detected_browsers: Vec<DetectedBrowser>,
    pub selected_browser: Option<DetectedBrowser>,
    pub logs: Vec<LogEntry>,
    pub session_flows: Vec<SessionFlow>,
    pub session_count: usize,
    pub request_count: u64,
    pub server_handle: Option<tokio::task::JoinHandle<()>>,
    pub ngrok_child: Option<tokio::process::Child>,
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
    pub fn new(port: u16, workspace_root: String) -> Self {
        Self {
            theme: theme::DEFAULT_THEME_ID.to_string(),
            mode: Mode::Both,
            tool_mode: ToolMode::OneTool,
            server_running: false,
            ngrok_running: false,
            ngrok_url: None,
            remote_connected: false,
            last_remote_activity_ms: None,
            devtools_running: false,
            port,
            workspace_root,
            detected_browsers: Vec::new(),
            selected_browser: None,
            logs: Vec::new(),
            session_flows: Vec::new(),
            session_count: 0,
            request_count: 0,
            server_handle: None,
            ngrok_child: None,
            remote_browser_child: None,
            devtools_child: None,
        }
    }

    pub fn current_theme(&self) -> &'static theme::ThemeDef {
        theme::resolve(&self.theme)
    }

    pub fn log(&mut self, level: &'static str, message: String) {
        let now = now_hms();
        self.logs.push(LogEntry {
            time: now,
            level,
            message,
        });
        if self.logs.len() > 200 {
            self.logs.remove(0);
        }
    }

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
