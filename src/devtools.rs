use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::browser::DetectedBrowser;

/// A running chrome-devtools-mcp child process with stdin/stdout JSON-RPC bridge.
pub struct DevtoolsBridge {
    #[allow(dead_code)]
    child: Child,
    stdin: tokio::io::BufWriter<tokio::process::ChildStdin>,
    pending: Arc<Mutex<std::collections::HashMap<Value, tokio::sync::oneshot::Sender<Value>>>>,
}

impl DevtoolsBridge {
    /// Spawn `npx chrome-devtools-mcp@latest` and set up stdio bridge.
    pub async fn start(
        selected_browser: Option<&DetectedBrowser>,
    ) -> Result<Arc<Mutex<Self>>, String> {
        let mut command = Command::new("npx");
        command.args(["-y", "chrome-devtools-mcp@latest"]);

        if let Some(browser) = selected_browser {
            if browser.remote_debug_active {
                if let Some(target) = browser.remote_debug_target.as_deref() {
                    if target == "pipe" {
                        command.args(["--executablePath", &browser.path]);
                    } else {
                        command.args(["--browserUrl", &format!("http://{target}")]);
                    }
                } else {
                    command.args(["--executablePath", &browser.path]);
                }
            } else {
                command.args(["--executablePath", &browser.path]);
            }
        }

        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to spawn chrome-devtools-mcp: {e}"))?;

        let child_stdin = child.stdin.take().ok_or("No stdin")?;
        let child_stdout = child.stdout.take().ok_or("No stdout")?;

        let stdin = tokio::io::BufWriter::new(child_stdin);
        let pending: Arc<
            Mutex<std::collections::HashMap<Value, tokio::sync::oneshot::Sender<Value>>>,
        > = Arc::new(Mutex::new(std::collections::HashMap::new()));

        let bridge = Arc::new(Mutex::new(Self {
            child,
            stdin,
            pending: pending.clone(),
        }));

        // Spawn stdout reader task
        let pending_clone = pending;
        tokio::spawn(async move {
            let mut reader = BufReader::new(child_stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Ok(msg) = serde_json::from_str::<Value>(trimmed) {
                            // Match response to pending request by id
                            if let Some(id) = msg.get("id").cloned() {
                                let mut map = pending_clone.lock().await;
                                if let Some(tx) = map.remove(&id) {
                                    let _ = tx.send(msg);
                                }
                            }
                            // Notifications from devtools (no id) are ignored for now
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(bridge)
    }

    /// Send a JSON-RPC request and wait for the response.
    pub async fn request(&mut self, req: &Value) -> Result<Value, String> {
        // Write JSON line to stdin
        let line = serde_json::to_string(req).map_err(|e| e.to_string())?;
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("stdin write: {e}"))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| format!("stdin write newline: {e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("stdin flush: {e}"))?;

        // If request has id, wait for response
        if let Some(id) = req.get("id").cloned() {
            let (tx, rx) = tokio::sync::oneshot::channel();
            {
                let mut map = self.pending.lock().await;
                map.insert(id, tx);
            }
            // Timeout 120s
            match tokio::time::timeout(std::time::Duration::from_secs(120), rx).await {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(_)) => Err("Response channel closed".into()),
                Err(_) => Err("Request timed out (120s)".into()),
            }
        } else {
            // Notification — no response expected
            Ok(Value::Null)
        }
    }

    /// Send a notification (no id, no response expected).
    pub async fn notify(&mut self, req: &Value) -> Result<(), String> {
        let line = serde_json::to_string(req).map_err(|e| e.to_string())?;
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("stdin write: {e}"))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| format!("stdin write: {e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("stdin flush: {e}"))?;
        Ok(())
    }

    /// Kill the child process.
    #[allow(dead_code)]
    pub async fn stop(&mut self) {
        let _ = self.child.kill().await;
    }
}
