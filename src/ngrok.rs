use crate::state::{SharedState, load_ngrok_authtoken};
use ngrok::prelude::*;
use reqwest::Url;

/// Start an ngrok HTTP tunnel using the embedded Rust SDK.
pub async fn start(state: SharedState) -> Result<(), String> {
    let (port, mcp_path) = {
        let app = state.lock().await;
        if app.ngrok_running {
            return Err("ngrok is already running".into());
        }
        (app.port, app.mcp_path())
    };
    let authtoken = load_ngrok_authtoken()
        .map_err(|e| format!("Failed to read ~/.catdesk/config.toml: {e}"))?
        .ok_or_else(|| "ngrok authtoken is not configured".to_string())?;
    let forwards_to: Url = format!("http://127.0.0.1:{port}")
        .parse()
        .map_err(|e| format!("Invalid forward URL: {e}"))?;

    let mut forwarder = ngrok::Session::builder()
        .authtoken(authtoken)
        .connect()
        .await
        .map_err(|e| format!("Failed to connect ngrok session: {e}"))?
        .http_endpoint()
        .listen_and_forward(forwards_to)
        .await
        .map_err(|e| format!("Failed to open ngrok tunnel: {e}"))?;
    let url = forwarder.url().to_string();

    let state_clone = state.clone();
    let watcher = tokio::spawn(async move {
        let result = forwarder.join().await;
        let mut app = state_clone.lock().await;
        match result {
            Ok(Ok(())) => app.log("WARN", "ngrok tunnel exited".into()),
            Ok(Err(e)) => app.log("ERROR", format!("ngrok tunnel failed: {e}")),
            Err(e) if e.is_cancelled() => return,
            Err(e) => app.log("ERROR", format!("ngrok tunnel join failed: {e}")),
        }
        app.ngrok_running = false;
        app.ngrok_url = None;
        app.remote_connected = false;
        app.last_remote_activity_ms = None;
    });

    {
        let mut app = state.lock().await;
        app.ngrok_task = Some(watcher);
        app.ngrok_running = true;
        app.ngrok_url = Some(url.clone());
        app.log("INFO", "ngrok SDK tunnel started".into());
        app.log("INFO", format!("ngrok URL: {url}"));
        app.log("INFO", format!("MCP Server URL: {url}{mcp_path}"));
    }

    Ok(())
}

/// Stop ngrok.
pub async fn stop(state: SharedState) -> Result<(), String> {
    let mut app = state.lock().await;
    if let Some(handle) = app.ngrok_task.take() {
        handle.abort();
    }
    app.ngrok_running = false;
    app.ngrok_url = None;
    app.log("INFO", "ngrok stopped".into());
    Ok(())
}
