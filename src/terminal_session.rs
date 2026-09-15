use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use uuid::Uuid;

const DEFAULT_ROWS: u16 = 28;
const DEFAULT_COLS: u16 = 100;
const MIN_ROWS: u16 = 8;
const MAX_ROWS: u16 = 120;
const MIN_COLS: u16 = 20;
const MAX_COLS: u16 = 240;
const MAX_SESSIONS: usize = 4;
const SCROLLBACK_ROWS: usize = 500;
const IDLE_SESSION_TIMEOUT_MS: u64 = 10 * 60 * 1_000;

#[derive(Clone, Default)]
pub struct TerminalSessionManager {
    sessions: Arc<Mutex<HashMap<String, Arc<TerminalSession>>>>,
}

struct TerminalSession {
    id: String,
    workspace: String,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    screen: Arc<Mutex<ScreenState>>,
    alive: Arc<AtomicBool>,
    version: Arc<AtomicU64>,
    last_client_activity_ms: AtomicU64,
}

struct ScreenState {
    parser: vt100::Parser,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshot {
    pub session_id: String,
    pub version: u64,
    pub rows: u16,
    pub cols: u16,
    pub lines: Vec<String>,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_hidden: bool,
    pub alive: bool,
    pub workspace: String,
    pub error: Option<String>,
}

impl TerminalSessionManager {
    pub fn open(
        &self,
        workspace_root: &Path,
        rows: Option<u16>,
        cols: Option<u16>,
    ) -> Result<TerminalSnapshot, String> {
        let rows = clamp_rows(rows.unwrap_or(DEFAULT_ROWS));
        let cols = clamp_cols(cols.unwrap_or(DEFAULT_COLS));
        let workspace_root = workspace_root
            .canonicalize()
            .map_err(|error| format!("failed to resolve workspace: {error}"))?;
        if !workspace_root.is_dir() {
            return Err("workspace root is not a directory".to_string());
        }

        self.prune_dead();
        {
            let sessions = self
                .sessions
                .lock()
                .map_err(|_| "terminal session registry is unavailable".to_string())?;
            if sessions.len() >= MAX_SESSIONS {
                return Err(format!(
                    "too many interactive terminals are open (maximum {MAX_SESSIONS})"
                ));
            }
        }

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("failed to create PTY: {error}"))?;

        let mut command = CommandBuilder::new_default_prog();
        command.cwd(&workspace_root);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("CATDESK_INTERACTIVE_TERMINAL", "1");

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| format!("failed to launch shell: {error}"))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("failed to open PTY reader: {error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| format!("failed to open PTY writer: {error}"))?;

        let id = Uuid::new_v4().simple().to_string();
        let alive = Arc::new(AtomicBool::new(true));
        let version = Arc::new(AtomicU64::new(1));
        let screen = Arc::new(Mutex::new(ScreenState {
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_ROWS),
            error: None,
        }));

        let reader_screen = screen.clone();
        let reader_alive = alive.clone();
        let reader_version = version.clone();
        let thread_name = format!("catdesk-terminal-{}", &id[..8]);
        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let mut buffer = [0u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            if let Ok(mut state) = reader_screen.lock() {
                                state.parser.process(&buffer[..read]);
                            } else {
                                break;
                            }
                            reader_version.fetch_add(1, Ordering::Release);
                        }
                        Err(error) => {
                            if let Ok(mut state) = reader_screen.lock() {
                                state.error = Some(format!("PTY read failed: {error}"));
                            }
                            break;
                        }
                    }
                }
                reader_alive.store(false, Ordering::Release);
                reader_version.fetch_add(1, Ordering::Release);
            })
            .map_err(|error| format!("failed to start PTY reader: {error}"))?;

        let session = Arc::new(TerminalSession {
            id: id.clone(),
            workspace: workspace_root.to_string_lossy().into_owned(),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            screen,
            alive,
            version,
            last_client_activity_ms: AtomicU64::new(now_unix_millis()),
        });
        spawn_idle_watchdog(&session)?;
        let snapshot = session.snapshot()?;
        self.sessions
            .lock()
            .map_err(|_| "terminal session registry is unavailable".to_string())?
            .insert(id, session);
        Ok(snapshot)
    }

    pub fn snapshot(&self, session_id: &str) -> Result<TerminalSnapshot, String> {
        let session = self.session(session_id)?;
        session.touch();
        session.snapshot()
    }

    pub fn write_input(&self, session_id: &str, data: &str) -> Result<TerminalSnapshot, String> {
        let session = self.session(session_id)?;
        session.touch();
        if !session.alive.load(Ordering::Acquire) {
            return Err("terminal session has exited".to_string());
        }
        {
            let mut writer = session
                .writer
                .lock()
                .map_err(|_| "terminal input stream is unavailable".to_string())?;
            writer
                .write_all(data.as_bytes())
                .map_err(|error| format!("failed to write terminal input: {error}"))?;
            writer
                .flush()
                .map_err(|error| format!("failed to flush terminal input: {error}"))?;
        }
        session.snapshot()
    }

    pub fn resize(
        &self,
        session_id: &str,
        rows: u16,
        cols: u16,
    ) -> Result<TerminalSnapshot, String> {
        let session = self.session(session_id)?;
        session.touch();
        let rows = clamp_rows(rows);
        let cols = clamp_cols(cols);
        session
            .master
            .lock()
            .map_err(|_| "terminal PTY is unavailable".to_string())?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("failed to resize terminal: {error}"))?;
        {
            let mut state = session
                .screen
                .lock()
                .map_err(|_| "terminal screen is unavailable".to_string())?;
            state.parser.set_size(rows, cols);
        }
        session.version.fetch_add(1, Ordering::Release);
        session.snapshot()
    }

    pub fn close(&self, session_id: &str) -> Result<(), String> {
        let session = self
            .sessions
            .lock()
            .map_err(|_| "terminal session registry is unavailable".to_string())?
            .remove(session_id)
            .ok_or_else(|| "terminal session not found".to_string())?;
        session.alive.store(false, Ordering::Release);
        session.version.fetch_add(1, Ordering::Release);
        session
            .child
            .lock()
            .map_err(|_| "terminal process handle is unavailable".to_string())?
            .kill()
            .map_err(|error| format!("failed to terminate terminal shell: {error}"))?;
        Ok(())
    }

    pub fn close_all(&self) {
        let sessions = match self.sessions.lock() {
            Ok(mut sessions) => sessions
                .drain()
                .map(|(_, session)| session)
                .collect::<Vec<_>>(),
            Err(_) => return,
        };
        for session in sessions {
            session.alive.store(false, Ordering::Release);
            if let Ok(mut child) = session.child.lock() {
                let _ = child.kill();
            }
        }
    }

    fn session(&self, session_id: &str) -> Result<Arc<TerminalSession>, String> {
        self.sessions
            .lock()
            .map_err(|_| "terminal session registry is unavailable".to_string())?
            .get(session_id)
            .cloned()
            .ok_or_else(|| "terminal session not found".to_string())
    }

    fn prune_dead(&self) {
        let now = now_unix_millis();
        let stale = match self.sessions.lock() {
            Ok(mut sessions) => {
                let stale_ids = sessions
                    .iter()
                    .filter_map(|(id, session)| {
                        let alive = session.alive.load(Ordering::Acquire);
                        let last_activity = session.last_client_activity_ms.load(Ordering::Acquire);
                        let idle = now.saturating_sub(last_activity) >= IDLE_SESSION_TIMEOUT_MS;
                        (!alive || idle).then_some(id.clone())
                    })
                    .collect::<Vec<_>>();
                stale_ids
                    .into_iter()
                    .filter_map(|id| sessions.remove(&id))
                    .collect::<Vec<_>>()
            }
            Err(_) => return,
        };
        for session in stale {
            session.alive.store(false, Ordering::Release);
            if let Ok(mut child) = session.child.lock() {
                let _ = child.kill();
            }
        }
    }
}

impl TerminalSession {
    fn touch(&self) {
        self.last_client_activity_ms
            .store(now_unix_millis(), Ordering::Release);
    }

    fn snapshot(&self) -> Result<TerminalSnapshot, String> {
        let state = self
            .screen
            .lock()
            .map_err(|_| "terminal screen is unavailable".to_string())?;
        let screen = state.parser.screen();
        let (rows, cols) = screen.size();
        let (cursor_row, cursor_col) = screen.cursor_position();
        let lines = screen.rows(0, cols).collect::<Vec<_>>();
        Ok(TerminalSnapshot {
            session_id: self.id.clone(),
            version: self.version.load(Ordering::Acquire),
            rows,
            cols,
            lines,
            cursor_row,
            cursor_col,
            cursor_hidden: screen.hide_cursor(),
            alive: self.alive.load(Ordering::Acquire),
            workspace: self.workspace.clone(),
            error: state.error.clone(),
        })
    }
}

impl Drop for TerminalSessionManager {
    fn drop(&mut self) {
        if Arc::strong_count(&self.sessions) == 1 {
            self.close_all();
        }
    }
}

fn spawn_idle_watchdog(session: &Arc<TerminalSession>) -> Result<(), String> {
    let weak_session = Arc::downgrade(session);
    let thread_name = format!("catdesk-terminal-idle-{}", &session.id[..8]);
    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(30));
                let Some(session) = weak_session.upgrade() else {
                    return;
                };
                if !session.alive.load(Ordering::Acquire) {
                    return;
                }
                let last_activity = session.last_client_activity_ms.load(Ordering::Acquire);
                if now_unix_millis().saturating_sub(last_activity) < IDLE_SESSION_TIMEOUT_MS {
                    continue;
                }
                session.alive.store(false, Ordering::Release);
                session.version.fetch_add(1, Ordering::Release);
                if let Ok(mut child) = session.child.lock() {
                    let _ = child.kill();
                }
                return;
            }
        })
        .map_err(|error| format!("failed to start terminal idle watchdog: {error}"))?;
    Ok(())
}

fn now_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn clamp_rows(rows: u16) -> u16 {
    rows.clamp(MIN_ROWS, MAX_ROWS)
}

fn clamp_cols(cols: u16) -> u16 {
    cols.clamp(MIN_COLS, MAX_COLS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn terminal_dimensions_are_bounded() {
        assert_eq!(clamp_rows(1), MIN_ROWS);
        assert_eq!(clamp_rows(500), MAX_ROWS);
        assert_eq!(clamp_cols(1), MIN_COLS);
        assert_eq!(clamp_cols(500), MAX_COLS);
    }

    #[cfg(unix)]
    #[test]
    fn inactive_terminal_sessions_are_pruned() {
        let root = std::env::temp_dir().join(format!("catdesk-terminal-idle-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create terminal test workspace");
        let manager = TerminalSessionManager::default();
        let opened = manager
            .open(&root, Some(20), Some(80))
            .expect("open terminal session");
        let session = manager
            .session(&opened.session_id)
            .expect("find terminal session");
        session.last_client_activity_ms.store(
            now_unix_millis().saturating_sub(IDLE_SESSION_TIMEOUT_MS + 1),
            Ordering::Release,
        );

        manager.prune_dead();

        assert!(manager.session(&opened.session_id).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_session_accepts_input_and_renders_screen() {
        let root = std::env::temp_dir().join(format!("catdesk-terminal-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create terminal test workspace");
        let manager = TerminalSessionManager::default();
        let opened = manager
            .open(&root, Some(20), Some(80))
            .expect("open terminal session");

        manager
            .write_input(&opened.session_id, "printf 'catdesk-pty-ok\\n'\r")
            .expect("write terminal command");

        let started = Instant::now();
        let snapshot = loop {
            let snapshot = manager
                .snapshot(&opened.session_id)
                .expect("read terminal snapshot");
            if snapshot
                .lines
                .iter()
                .any(|line| line.contains("catdesk-pty-ok"))
            {
                break snapshot;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "PTY output timeout"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(snapshot.rows, 20);
        assert_eq!(snapshot.cols, 80);

        let resized = manager
            .resize(&opened.session_id, 30, 120)
            .expect("resize terminal");
        assert_eq!((resized.rows, resized.cols), (30, 120));

        manager.close(&opened.session_id).expect("close terminal");
        assert!(manager.snapshot(&opened.session_id).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
