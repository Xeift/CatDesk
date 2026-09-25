use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::state::user_home_dir;

pub const ROUTER_PORT: u16 = 3200;
pub const WORKER_PORT_START: u16 = 3201;
pub const WORKER_PORT_END: u16 = 3299;
const REGISTRY_FILE_NAME: &str = "workspace_id.toml";
const LOCK_RETRY_COUNT: usize = 100;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
const PORT_PROBE_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRegistration {
    pub workspace: String,
    pub port: u16,
    pub pid: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceRegistry {
    #[serde(default)]
    workspaces: Vec<WorkspaceRegistration>,
    #[serde(default)]
    sessions: BTreeMap<String, String>,
}

struct RegistryLock {
    path: PathBuf,
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn registry_path() -> io::Result<PathBuf> {
    Ok(user_home_dir()?.join(".catdesk").join(REGISTRY_FILE_NAME))
}

fn registry_lock_path(path: &Path) -> PathBuf {
    path.with_extension("lock")
}

fn acquire_registry_lock(path: &Path) -> io::Result<RegistryLock> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = registry_lock_path(path);
    for _ in 0..LOCK_RETRY_COUNT {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => return Ok(RegistryLock { path: lock_path }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let stale = fs::metadata(&lock_path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(5));
                if stale {
                    let _ = fs::remove_file(&lock_path);
                    continue;
                }
                thread::sleep(LOCK_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other(format!(
        "timed out waiting for workspace registry lock: {}",
        lock_path.display()
    )))
}

fn load_registry(path: &Path) -> io::Result<WorkspaceRegistry> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(WorkspaceRegistry::default()),
        Ok(text) => toml::from_str(&text).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(WorkspaceRegistry::default()),
        Err(error) => Err(error),
    }
}

fn save_registry(path: &Path, registry: &WorkspaceRegistry) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(registry).map_err(io::Error::other)?;
    fs::write(path, text)
}

fn canonical_workspace(workspace_root: &str) -> io::Result<String> {
    Ok(Path::new(workspace_root)
        .canonicalize()?
        .to_string_lossy()
        .into_owned())
}

fn port_is_open(port: u16) -> bool {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    TcpStream::connect_timeout(&address, PORT_PROBE_TIMEOUT).is_ok()
}

fn prune_stale(registry: &mut WorkspaceRegistry) {
    registry
        .workspaces
        .retain(|workspace| port_is_open(workspace.port));
    registry.sessions.retain(|_, workspace| {
        registry
            .workspaces
            .iter()
            .any(|registration| &registration.workspace == workspace)
    });
}

pub fn register_workspace(workspace_root: &str, port: u16) -> io::Result<WorkspaceRegistration> {
    let path = registry_path()?;
    let _lock = acquire_registry_lock(&path)?;
    let mut registry = load_registry(&path)?;
    prune_stale(&mut registry);

    let workspace = canonical_workspace(workspace_root)?;
    let registration = WorkspaceRegistration {
        workspace: workspace.clone(),
        port,
        pid: std::process::id(),
    };

    if let Some(existing) = registry
        .workspaces
        .iter_mut()
        .find(|entry| entry.workspace == workspace)
    {
        *existing = registration.clone();
    } else {
        registry.workspaces.push(registration.clone());
    }

    // Restarting CatDesk in a workspace explicitly makes that workspace available
    // for the next unbound ChatGPT conversation.
    registry.sessions.retain(|_, bound| bound != &workspace);
    save_registry(&path, &registry)?;
    Ok(registration)
}

pub fn unregister_workspace(workspace_root: &str, port: u16) -> io::Result<()> {
    let path = registry_path()?;
    let _lock = acquire_registry_lock(&path)?;
    let mut registry = load_registry(&path)?;
    let workspace =
        canonical_workspace(workspace_root).unwrap_or_else(|_| workspace_root.to_string());

    registry
        .workspaces
        .retain(|entry| !(entry.workspace == workspace && entry.port == port));
    registry.sessions.retain(|_, bound| bound != &workspace);

    if registry.workspaces.is_empty() && registry.sessions.is_empty() {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    save_registry(&path, &registry)
}

pub fn resolve_session(session_id: &str) -> Result<WorkspaceRegistration, String> {
    let path = registry_path().map_err(|error| error.to_string())?;
    resolve_session_at(&path, session_id)
}

fn resolve_session_at(path: &Path, session_id: &str) -> Result<WorkspaceRegistration, String> {
    let _lock = acquire_registry_lock(path).map_err(|error| error.to_string())?;
    let mut registry = load_registry(path).map_err(|error| error.to_string())?;
    prune_stale(&mut registry);

    if let Some(workspace) = registry.sessions.get(session_id).cloned() {
        if let Some(registration) = registry
            .workspaces
            .iter()
            .find(|entry| entry.workspace == workspace)
            .cloned()
        {
            save_registry(path, &registry).map_err(|error| error.to_string())?;
            return Ok(registration);
        }
        registry.sessions.remove(session_id);
    }

    let next = registry
        .workspaces
        .iter()
        .find(|entry| {
            !registry
                .sessions
                .values()
                .any(|workspace| workspace == &entry.workspace)
        })
        .cloned()
        .ok_or_else(|| {
            "No unbound CatDesk workspace is available for this ChatGPT session. Start or restart CatDesk in the workspace you want to use, then retry the tool call.".to_string()
        })?;

    registry
        .sessions
        .insert(session_id.to_string(), next.workspace.clone());
    save_registry(path, &registry).map_err(|error| error.to_string())?;
    Ok(next)
}

pub fn registration_for_port(port: u16) -> Result<WorkspaceRegistration, String> {
    let path = registry_path().map_err(|error| error.to_string())?;
    let _lock = acquire_registry_lock(&path).map_err(|error| error.to_string())?;
    let mut registry = load_registry(&path).map_err(|error| error.to_string())?;
    prune_stale(&mut registry);
    let registration = registry
        .workspaces
        .iter()
        .find(|entry| entry.port == port)
        .cloned()
        .ok_or_else(|| format!("No live CatDesk workspace worker is registered on port {port}"))?;
    save_registry(&path, &registry).map_err(|error| error.to_string())?;
    Ok(registration)
}

pub fn session_id_from_request(body: &Value) -> Option<&str> {
    body.get("params")
        .and_then(Value::as_object)
        .and_then(|params| params.get("_meta"))
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("openai/session"))
        .and_then(Value::as_str)
        .filter(|session| !session.is_empty())
}

pub fn find_available_worker_port() -> io::Result<u16> {
    for port in WORKER_PORT_START..=WORKER_PORT_END {
        if TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
            return Ok(port);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        format!("no free CatDesk workspace worker port in {WORKER_PORT_START}..={WORKER_PORT_END}"),
    ))
}

pub async fn router_is_running(mcp_slug: &str) -> bool {
    if mcp_slug.trim().is_empty() {
        return false;
    }
    let url = format!("http://127.0.0.1:{ROUTER_PORT}/{mcp_slug}");
    let Ok(response) = reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_millis(500))
        .send()
        .await
    else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    response
        .json::<Value>()
        .await
        .ok()
        .and_then(|value| {
            value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .is_some_and(|name| name == "CatDesk")
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_registry_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "catdesk-workspace-router-{name}-{}.toml",
            Uuid::new_v4()
        ))
    }

    fn save_test_registry(path: &Path, registry: &WorkspaceRegistry) {
        save_registry(path, registry).expect("save registry");
    }

    #[test]
    fn session_id_reads_openai_session_metadata() {
        let request = serde_json::json!({
            "params": {
                "_meta": {
                    "openai/session": "conversation-123"
                }
            }
        });
        assert_eq!(session_id_from_request(&request), Some("conversation-123"));
    }

    #[test]
    fn sessions_bind_to_distinct_live_workspaces_and_stay_stable() {
        let path = test_registry_path("bindings");
        let listener_a = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind A");
        let listener_b = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind B");
        let port_a = listener_a.local_addr().expect("addr A").port();
        let port_b = listener_b.local_addr().expect("addr B").port();

        save_test_registry(
            &path,
            &WorkspaceRegistry {
                workspaces: vec![
                    WorkspaceRegistration {
                        workspace: "/tmp/project-a".into(),
                        port: port_a,
                        pid: 1,
                    },
                    WorkspaceRegistration {
                        workspace: "/tmp/project-b".into(),
                        port: port_b,
                        pid: 2,
                    },
                ],
                sessions: BTreeMap::new(),
            },
        );

        let first = resolve_session_at(&path, "session-a").expect("bind A");
        let second = resolve_session_at(&path, "session-b").expect("bind B");
        let first_again = resolve_session_at(&path, "session-a").expect("rebind A");

        assert_eq!(first.workspace, "/tmp/project-a");
        assert_eq!(second.workspace, "/tmp/project-b");
        assert_eq!(first_again, first);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn no_unbound_workspace_is_reported() {
        let path = test_registry_path("exhausted");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let mut sessions = BTreeMap::new();
        sessions.insert("existing".into(), "/tmp/project-a".into());
        save_test_registry(
            &path,
            &WorkspaceRegistry {
                workspaces: vec![WorkspaceRegistration {
                    workspace: "/tmp/project-a".into(),
                    port,
                    pid: 1,
                }],
                sessions,
            },
        );

        assert!(resolve_session_at(&path, "new-session").is_err());
        let _ = fs::remove_file(path);
    }
}
