mod command;
mod mcp;
mod ngrok;
mod server;
mod state;

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
        MouseEventKind,
    },
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use state::{AppState, SharedState};
use std::io::{stdout, Write};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

// ── Selection state ─────────────────────────────────────────

struct Selection {
    start: Option<(u16, u16)>,
    end: Option<(u16, u16)>,
    dragging: bool,
}

impl Selection {
    fn new() -> Self {
        Self { start: None, end: None, dragging: false }
    }

    fn clear(&mut self) {
        self.start = None;
        self.end = None;
        self.dragging = false;
    }

    /// Get normalized (top-left, bottom-right) range
    fn range(&self) -> Option<((u16, u16), (u16, u16))> {
        match (self.start, self.end) {
            (Some(s), Some(e)) => {
                let (r0, c0, r1, c1) = if (s.1, s.0) <= (e.1, e.0) {
                    (s.1, s.0, e.1, e.0)
                } else {
                    (e.1, e.0, s.1, s.0)
                };
                Some(((c0, r0), (c1, r1)))
            }
            _ => None,
        }
    }
}

/// Extract selected text from screen line snapshots.
fn extract_from_screen(
    lines: &[String],
    start: (u16, u16),
    end: (u16, u16),
) -> String {
    let (c0, r0) = start;
    let (c1, r1) = end;
    let mut result = String::new();

    for row in r0..=r1 {
        let row_idx = row as usize;
        if row_idx >= lines.len() {
            break;
        }
        let line: Vec<char> = lines[row_idx].chars().collect();
        let col_start = if row == r0 { c0 as usize } else { 0 };
        let col_end = if row == r1 { (c1 as usize).min(line.len().saturating_sub(1)) } else { line.len().saturating_sub(1) };
        for col in col_start..=col_end {
            if col < line.len() {
                result.push(line[col]);
            }
        }
        if row != r1 {
            result.push('\n');
        }
    }

    result
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Copy text to clipboard: OSC 52 + native fallback (xclip/xsel/wl-copy).
fn clipboard_copy(text: &str) {
    // OSC 52
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{encoded}\x07");
    let _ = stdout().write_all(seq.as_bytes());
    let _ = stdout().flush();

    // Native fallback (fire and forget)
    let text_owned = text.to_string();
    std::thread::spawn(move || {
        // Try wl-copy (Wayland)
        if let Ok(mut child) = std::process::Command::new("wl-copy")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(text_owned.as_bytes());
            }
            let _ = child.wait();
            return;
        }
        // Try xclip
        if let Ok(mut child) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(text_owned.as_bytes());
            }
            let _ = child.wait();
            return;
        }
        // Try xsel
        if let Ok(mut child) = std::process::Command::new("xsel")
            .args(["--clipboard", "--input"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(text_owned.as_bytes());
            }
            let _ = child.wait();
        }
    });
}

// ── Main ────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3200);

    let workspace_root = std::env::var("WORKSPACE_ROOT")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| "/tmp".into());

    let state: SharedState = Arc::new(Mutex::new(AppState::new(port, workspace_root)));

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run_tui(&mut terminal, state.clone()).await;

    stdout().execute(DisableMouseCapture)?;
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    {
        let mut app = state.lock().await;
        if let Some(ref mut child) = app.ngrok_child {
            let _ = child.kill().await;
        }
    }

    result
}

async fn run_tui(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: SharedState,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut log_scroll: usize = 0;
    let mut selection = Selection::new();
    // (message, position (col, row), created_at)
    let mut toast: Option<(&str, (u16, u16), Instant)> = None;
    #[allow(unused_assignments)]
    let mut screen_lines: Vec<String> = vec![];

    loop {
        {
            let app = state.lock().await;
            let toast_ref = toast
                .as_ref()
                .filter(|(_, _, t)| t.elapsed().as_secs() < 2)
                .map(|(m, pos, _)| (*m, *pos));
            let mut new_lines: Vec<String> = Vec::new();
            terminal.draw(|f| {
                draw_ui(f, &app, log_scroll, toast_ref);

                // Render selection highlight overlay
                if let Some(((c0, r0), (c1, r1))) = selection.range() {
                    let area = f.area();
                    for row in r0..=r1 {
                        if row >= area.height { break; }
                        let cs = if row == r0 { c0 } else { 0 };
                        let ce = if row == r1 { c1 } else { area.width.saturating_sub(1) };
                        for col in cs..=ce {
                            if col >= area.width { break; }
                            if let Some(cell) = f.buffer_mut().cell_mut((col, row)) {
                                cell.set_style(Style::default().bg(Color::DarkGray).fg(Color::White));
                            }
                        }
                    }
                }

                // Snapshot all screen text from the buffer
                let area = f.area();
                let buf = f.buffer_mut();
                for row in 0..area.height {
                    let mut line = String::new();
                    for col in 0..area.width {
                        let cell = &buf[(col, row)];
                        line.push_str(cell.symbol());
                    }
                    new_lines.push(line);
                }
            })?;
            screen_lines = new_lines;
        }

        // Clear expired toast
        if let Some((_, _, t)) = &toast {
            if t.elapsed().as_secs() >= 2 {
                toast = None;
            }
        }

        if event::poll(std::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    selection.clear();
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('s') => toggle_server(state.clone()).await,
                        KeyCode::Char('n') => toggle_ngrok(state.clone()).await,
                        KeyCode::Char('b') => start_both(state.clone()).await,
                        KeyCode::Char('x') => stop_both(state.clone()).await,
                        KeyCode::Up => log_scroll = log_scroll.saturating_sub(1),
                        KeyCode::Down => log_scroll = log_scroll.saturating_add(1),
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        selection.start = Some((mouse.column, mouse.row));
                        selection.end = Some((mouse.column, mouse.row));
                        selection.dragging = true;
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if selection.dragging {
                            selection.end = Some((mouse.column, mouse.row));
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        if selection.dragging {
                            selection.end = Some((mouse.column, mouse.row));
                            selection.dragging = false;

                            if let Some((start, end)) = selection.range() {
                                if start != end {
                                    let text = extract_from_screen(&screen_lines, start, end);
                                    if !text.is_empty() {
                                        clipboard_copy(&text);
                                        // Toast at mouse release position
                                        toast = Some(("Copied!", (mouse.column, mouse.row), Instant::now()));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    Ok(())
}

fn draw_ui(f: &mut Frame, app: &AppState, log_scroll: usize, toast: Option<(&str, (u16, u16))>) {
    let area = f.area();

    let both_running = app.server_running && app.ngrok_running;
    let has_url = app.ngrok_url.is_some();
    let show_guide = both_running && has_url && !app.remote_connected;
    let status_height = if show_guide { 16 } else { 10 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(status_height),
            Constraint::Length(3),
            Constraint::Min(5),
        ])
        .split(area);

    // ── Header ──
    let header = Paragraph::new("  MCP3000 - MCP Tools for ChatGPT to control your computer and browser")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
    f.render_widget(header, chunks[0]);

    // ── Status ──
    let server_status = if app.server_running {
        format!("RUNNING (port {})", app.port)
    } else {
        "STOPPED".into()
    };

    let ngrok_status: &str = if app.ngrok_running { "RUNNING" } else { "STOPPED" };

    let mcp_url: String = app.ngrok_url.as_ref()
        .map(|u| format!("{u}/mcp"))
        .unwrap_or_else(|| "--".into());

    let mut status_lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("  Server:           ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled(&server_status, Style::default().fg(if app.server_running { Color::Green } else { Color::Red })),
        ]),
        Line::from(vec![
            Span::styled("  ngrok:            ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled(ngrok_status, Style::default().fg(if app.ngrok_running { Color::Green } else { Color::Red })),
        ]),
        Line::from(vec![
            Span::styled("  MCP Server URL:   ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled(&mcp_url, Style::default().fg(if has_url { Color::Cyan } else { Color::DarkGray })),
        ]),
        {
            let mut spans = vec![Span::styled("  Remote connected: ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD))];
            if app.remote_connected {
                spans.push(Span::styled("V", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)));
            } else {
                spans.push(Span::styled("X", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)));
                if !both_running {
                    spans.push(Span::styled("  (press b to start server)", Style::default().fg(Color::DarkGray)));
                }
            }
            Line::from(spans)
        },
    ];

    if show_guide {
        status_lines.push(Line::from(""));
        status_lines.push(Line::from(Span::styled("  What to do next?", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))));
        status_lines.push(Line::from(vec![
            Span::styled("  1. ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("Open connector settings: ", Style::default().fg(Color::White)),
            Span::styled("https://chatgpt.com/apps#settings/Connectors", Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled("  2. ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("Click ", Style::default().fg(Color::White)),
            Span::styled("Create app", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled("  3. ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("Fill in the form:", Style::default().fg(Color::White)),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled("       Name:           ", Style::default().fg(Color::DarkGray)),
            Span::styled("MCP3000", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled("  (or any name you like)", Style::default().fg(Color::DarkGray)),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled("       MCP Server URL: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&mcp_url, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled("       Authentication: ", Style::default().fg(Color::DarkGray)),
            Span::styled("None", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled("  4. ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("Click ", Style::default().fg(Color::White)),
            Span::styled("\"I understand and want to continue\"", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled("  5. ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("Click ", Style::default().fg(Color::White)),
            Span::styled("Create", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ]));
    } else {
        status_lines.push(Line::from(""));
        status_lines.push(Line::from(vec![
            Span::styled("  Workspace: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&*app.workspace_root, Style::default().fg(Color::White)),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled("  Sessions:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(app.session_count.to_string(), Style::default().fg(Color::Yellow)),
            Span::styled("    Requests: ", Style::default().fg(Color::DarkGray)),
            Span::styled(app.request_count.to_string(), Style::default().fg(Color::Yellow)),
        ]));
    }

    let status = Paragraph::new(status_lines).block(
        Block::default().title(" Status ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(status, chunks[1]);

    // ── Keybindings ──
    let key_spans = vec![
        Span::styled("  [s]", Style::default().fg(Color::Cyan)),
        Span::raw(" Server  "),
        Span::styled("[n]", Style::default().fg(Color::Cyan)),
        Span::raw(" ngrok  "),
        Span::styled("[b]", Style::default().fg(Color::Cyan)),
        Span::raw(" Both  "),
        Span::styled("[x]", Style::default().fg(Color::Cyan)),
        Span::raw(" Stop all  "),
        Span::styled("[q]", Style::default().fg(Color::Red)),
        Span::raw(" Quit"),
    ];
    let keys = Paragraph::new(Line::from(key_spans)).block(
        Block::default().title(" Keys ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(keys, chunks[2]);

    // ── Logs ──
    let log_items: Vec<ListItem> = app.logs.iter().map(|entry| {
        let color = match entry.level {
            "ERROR" => Color::Red,
            "WARN" => Color::Yellow,
            _ => Color::Gray,
        };
        ListItem::new(Line::from(vec![
            Span::styled(format!(" {} ", entry.time), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:5} ", entry.level), Style::default().fg(color)),
            Span::styled(&*entry.message, Style::default().fg(Color::White)),
        ]))
    }).collect();

    let visible_height = chunks[3].height.saturating_sub(2) as usize;
    let total = log_items.len();
    let max_scroll = total.saturating_sub(visible_height);
    let effective_scroll = log_scroll.min(max_scroll);

    let visible_items: Vec<ListItem> = log_items.into_iter().skip(effective_scroll).take(visible_height).collect();
    let logs = List::new(visible_items).block(
        Block::default().title(" Logs ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(logs, chunks[3]);

    // ── Floating toast at cursor position ──
    if let Some((msg, (col, row))) = toast {
        let label = format!(" {msg} ");
        let w = label.len() as u16;
        // Position: prefer right of cursor, clamp to screen
        let x = col.saturating_add(1).min(area.width.saturating_sub(w));
        let y = if row > 0 { row - 1 } else { row + 1 }.min(area.height.saturating_sub(1));
        let toast_area = Rect::new(x, y, w, 1);
        let toast_widget = Paragraph::new(label)
            .style(Style::default().bg(Color::Green).fg(Color::Black).add_modifier(Modifier::BOLD));
        f.render_widget(toast_widget, toast_area);
    }
}

async fn toggle_server(state: SharedState) {
    let running = { state.lock().await.server_running };
    if running { stop_server(state).await; } else { start_server(state).await; }
}

async fn start_server(state: SharedState) {
    if state.lock().await.server_running { return; }
    let port = state.lock().await.port;

    let router = server::router(state.clone());
    let listener = match tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await {
        Ok(l) => l,
        Err(e) => {
            state.lock().await.log("ERROR", format!("Failed to bind port {port}: {e}"));
            return;
        }
    };

    let handle = tokio::spawn(async move { let _ = axum::serve(listener, router).await; });
    let mut app = state.lock().await;
    app.server_running = true;
    app.server_handle = Some(handle);
    app.log("INFO", format!("MCP Server started on port {port}"));
}

async fn stop_server(state: SharedState) {
    let mut app = state.lock().await;
    if let Some(handle) = app.server_handle.take() { handle.abort(); }
    app.server_running = false;
    app.log("INFO", "MCP Server stopped".into());
}

async fn toggle_ngrok(state: SharedState) {
    let running = { state.lock().await.ngrok_running };
    if running {
        let _ = ngrok::stop(state).await;
    } else if let Err(e) = ngrok::start(state.clone()).await {
        state.lock().await.log("ERROR", format!("ngrok: {e}"));
    }
}

async fn start_both(state: SharedState) {
    start_server(state.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    if let Err(e) = ngrok::start(state.clone()).await {
        state.lock().await.log("ERROR", format!("ngrok: {e}"));
    }
}

async fn stop_both(state: SharedState) {
    let _ = ngrok::stop(state.clone()).await;
    stop_server(state.clone()).await;
    state.lock().await.remote_connected = false;
}
