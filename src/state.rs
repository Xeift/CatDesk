use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;

/// Log entry displayed in the TUI.
#[derive(Clone)]
pub struct LogEntry {
    pub time: String,
    pub level: &'static str,
    pub message: String,
}

/// Shared application state across server, ngrok, and TUI.
pub struct AppState {
    pub server_running: bool,
    pub ngrok_running: bool,
    pub ngrok_url: Option<String>,
    pub remote_connected: bool,
    pub port: u16,
    pub workspace_root: String,
    pub logs: Vec<LogEntry>,
    pub session_count: usize,
    pub request_count: u64,
    pub server_handle: Option<tokio::task::JoinHandle<()>>,
    pub ngrok_child: Option<tokio::process::Child>,
}

pub type SharedState = Arc<Mutex<AppState>>;

impl AppState {
    pub fn new(port: u16, workspace_root: String) -> Self {
        Self {
            server_running: false,
            ngrok_running: false,
            ngrok_url: None,
            remote_connected: false,
            port,
            workspace_root,
            logs: Vec::new(),
            session_count: 0,
            request_count: 0,
            server_handle: None,
            ngrok_child: None,
        }
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
        self.logs.push(LogEntry {
            time: now,
            level,
            message,
        });
        // Keep last 200 entries
        if self.logs.len() > 200 {
            self.logs.remove(0);
        }
    }
}
