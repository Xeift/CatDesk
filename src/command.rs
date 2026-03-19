use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::time::{Duration, timeout};

const MAX_BUFFER_BYTES: usize = 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 120_000;

#[derive(Debug)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

/// Clamp timeout to [1, MAX_TIMEOUT_MS].
pub fn clamp_timeout(t: Option<u64>) -> u64 {
    match t {
        Some(v) if v >= 1 => v.min(MAX_TIMEOUT_MS),
        _ => DEFAULT_TIMEOUT_MS,
    }
}

/// Resolve `input` relative to `workspace_root`, rejecting path traversal.
pub fn resolve_workspace_path(
    workspace_root: &str,
    input: Option<&str>,
) -> Result<PathBuf, String> {
    let root = Path::new(workspace_root)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let input = input.unwrap_or(".");

    let candidate = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        root.join(input)
    };

    let candidate = candidate.canonicalize().unwrap_or(candidate);
    if !candidate.starts_with(&root) {
        return Err(format!(
            "Path escapes workspace root: {}",
            candidate.display()
        ));
    }
    Ok(candidate)
}

/// Execute a shell command via `/bin/bash`.
pub async fn run_command(command: &str, cwd: &Path, timeout_ms: u64) -> CommandResult {
    let fut = Command::new("/bin/bash")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .output();

    match timeout(Duration::from_millis(timeout_ms), fut).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(
                &output.stdout[..output.stdout.len().min(MAX_BUFFER_BYTES)],
            )
            .to_string();
            let stderr = String::from_utf8_lossy(
                &output.stderr[..output.stderr.len().min(MAX_BUFFER_BYTES)],
            )
            .to_string();
            CommandResult {
                stdout,
                stderr,
                success: output.status.success(),
            }
        }
        Ok(Err(e)) => CommandResult {
            stdout: String::new(),
            stderr: format!("Failed to execute: {e}"),
            success: false,
        },
        Err(_) => CommandResult {
            stdout: String::new(),
            stderr: format!("Command timed out after {timeout_ms} ms"),
            success: false,
        },
    }
}

/// Format stdout+stderr into a single string.
pub fn format_result(r: &CommandResult) -> String {
    let mut out = String::new();
    if !r.stdout.is_empty() {
        out.push_str(&r.stdout);
    }
    if !r.stderr.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\nSTDERR:\n");
        } else {
            out.push_str("STDERR:\n");
        }
        out.push_str(&r.stderr);
    }
    if out.is_empty() {
        out.push_str("(no output)");
    }
    out
}
