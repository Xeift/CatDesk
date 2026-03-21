use std::fs::OpenOptions;
use std::io::Write;
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
    pub devtools_running: bool,
    pub port: u16,
    pub workspace_root: String,
    pub detected_browsers: Vec<DetectedBrowser>,
    pub selected_browser: Option<DetectedBrowser>,
    pub logs: Vec<LogEntry>,
    pub session_count: usize,
    pub request_count: u64,
    pub server_handle: Option<tokio::task::JoinHandle<()>>,
    pub ngrok_child: Option<tokio::process::Child>,
    pub remote_browser_child: Option<tokio::process::Child>,
    pub devtools_child: Option<tokio::process::Child>,
}

pub type SharedState = Arc<Mutex<AppState>>;

const LOG_FILE_PATH: &str = "log.txt";

fn append_file_log(time: &str, level: &str, message: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_FILE_PATH)
    {
        let _ = writeln!(file, "{time} {level} {message}");
    }
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
            devtools_running: false,
            port,
            workspace_root,
            detected_browsers: Vec::new(),
            selected_browser: None,
            logs: Vec::new(),
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
        let secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let h = (secs % 86400) / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        let now = format!("{h:02}:{m:02}:{s:02}");
        append_file_log(&now, level, &message);
        self.logs.push(LogEntry {
            time: now,
            level,
            message,
        });
        if self.logs.len() > 200 {
            self.logs.remove(0);
        }
    }
}
