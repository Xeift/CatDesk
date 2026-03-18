use crate::state::SharedState;
use tokio::process::Command;

/// Start ngrok, parse public URL from the local API.
pub async fn start(state: SharedState) -> Result<(), String> {
    let port = {
        let app = state.lock().await;
        if app.ngrok_running {
            return Err("ngrok is already running".into());
        }
        app.port
    };

    // Kill any existing ngrok processes
    let _ = Command::new("pkill").arg("-f").arg("ngrok").output().await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Start ngrok in background
    let child = Command::new("ngrok")
        .arg("http")
        .arg(port.to_string())
        .arg("--log")
        .arg("stderr")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start ngrok: {e}"))?;

    {
        let mut app = state.lock().await;
        app.ngrok_child = Some(child);
        app.ngrok_running = true;
        app.log("INFO", "ngrok process started".into());
    }

    // Poll ngrok local API for the public URL
    let state_clone = state.clone();
    tokio::spawn(async move {
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if let Ok(url) = fetch_ngrok_url().await {
                let mut app = state_clone.lock().await;
                app.ngrok_url = Some(url.clone());
                app.log("INFO", format!("ngrok URL: {url}"));
                return;
            }
        }
        let mut app = state_clone.lock().await;
        app.log("WARN", "Could not get ngrok URL after 30s".into());
    });

    Ok(())
}

/// Stop ngrok.
pub async fn stop(state: SharedState) -> Result<(), String> {
    let mut app = state.lock().await;
    if let Some(ref mut child) = app.ngrok_child {
        let _ = child.kill().await;
    }
    app.ngrok_child = None;
    app.ngrok_running = false;
    app.ngrok_url = None;
    app.log("INFO", "ngrok stopped".into());
    Ok(())
}

/// Fetch public URL from ngrok's local API (http://127.0.0.1:4040/api/tunnels).
async fn fetch_ngrok_url() -> Result<String, String> {
    let resp = reqwest::get("http://127.0.0.1:4040/api/tunnels")
        .await
        .map_err(|e| e.to_string())?;
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let tunnels = body.get("tunnels").and_then(|t| t.as_array()).ok_or("No tunnels")?;
    for tunnel in tunnels {
        if let Some(url) = tunnel.get("public_url").and_then(|u| u.as_str()) {
            if url.starts_with("https://") {
                return Ok(url.to_string());
            }
        }
    }
    // Fallback to first tunnel
    tunnels
        .first()
        .and_then(|t| t.get("public_url"))
        .and_then(|u| u.as_str())
        .map(|s| s.to_string())
        .ok_or("No tunnel URL found".into())
}
