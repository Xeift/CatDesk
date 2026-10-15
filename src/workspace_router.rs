use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::state::user_home_dir;

pub const ROUTER_PORT: u16 = 3200;
pub const WORKER_PORT_START: u16 = 3201;
pub const WORKER_PORT_END: u16 = 3299;
const REGISTRY_FILE_NAME: &str = "workspace_id.toml";
const LOCK_RETRY_COUNT: usize = 100;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
const ROUTER_START_LOCK_RETRY_COUNT: usize = 3_000;
const PORT_PROBE_TIMEOUT: Duration = Duration::from_millis(100);
const ROUTER_CLIENT_TTL: Duration = Duration::from_secs(30);

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
    #[serde(default)]
    router_clients: Vec<RouterClient>,
    #[serde(default)]
    worker_port_reservations: Vec<WorkerPortReservation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RouterClient {
    workspace: String,
    pid: u32,
    last_seen_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerPortReservation {
    workspace: String,
    pid: u32,
    port: u16,
}

/// Keeps the shared router alive while this CatDesk process is starting or
/// running. Dropping the guard unregisters it on ordinary shutdown.
pub struct RouterClientGuard {
    workspace_root: String,
}

impl Drop for RouterClientGuard {
    fn drop(&mut self) {
        let _ = unregister_router_client(&self.workspace_root);
    }
}

/// Serializes first-router startup so simultaneously launched workspaces agree
/// on the persisted connector slug and only one of them spawns the router.
pub struct RouterStartupLock {
    path: PathBuf,
    token: String,
}

impl RouterStartupLock {
    fn owns_lock(&self) -> bool {
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| toml::from_str::<RouterStartLockOwner>(&text).ok())
            .is_some_and(|owner| owner.token == self.token)
    }
}

impl Drop for RouterStartupLock {
    fn drop(&mut self) {
        if self.owns_lock() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RouterStartLockOwner {
    pid: u32,
    token: String,
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

pub fn acquire_router_start_lock() -> io::Result<RouterStartupLock> {
    let path = registry_path()?.with_file_name("router_start.toml");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    for _ in 0..ROUTER_START_LOCK_RETRY_COUNT {
        let token = Uuid::new_v4().to_string();
        let owner = RouterStartLockOwner {
            pid: std::process::id(),
            token: token.clone(),
        };
        let temporary_path = path.with_extension(format!("{}.tmp", token));
        let write_owner = (|| -> io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)?;
            use std::io::Write as _;
            file.write_all(
                toml::to_string(&owner)
                    .map_err(io::Error::other)?
                    .as_bytes(),
            )?;
            file.flush()
        })();
        if let Err(error) = write_owner {
            let _ = fs::remove_file(&temporary_path);
            return Err(error);
        }
        match fs::hard_link(&temporary_path, &path) {
            Ok(()) => {
                let _ = fs::remove_file(&temporary_path);
                return Ok(RouterStartupLock { path, token });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temporary_path);
                let owner = fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| toml::from_str::<RouterStartLockOwner>(&text).ok());
                let invalid = owner.is_none();
                let stale_invalid = invalid
                    && fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(5));
                if owner.is_some_and(|owner| !process_is_alive(owner.pid)) || stale_invalid {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                thread::sleep(LOCK_RETRY_DELAY);
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary_path);
                return Err(error);
            }
        }
    }
    Err(io::Error::other(format!(
        "timed out waiting for CatDesk router startup lock: {}",
        path.display()
    )))
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle == 0 {
        return false;
    }
    unsafe {
        CloseHandle(handle);
    }
    true
}

fn acquire_registry_lock(path: &Path) -> io::Result<RegistryLock> {
    acquire_registry_lock_with_retries(path, LOCK_RETRY_COUNT)
}

fn acquire_registry_lock_with_retries(path: &Path, retry_count: usize) -> io::Result<RegistryLock> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = registry_lock_path(path);
    for _ in 0..retry_count {
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

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn prune_stale_router_clients(registry: &mut WorkspaceRegistry) {
    let ttl_ms: u64 = ROUTER_CLIENT_TTL.as_millis().try_into().unwrap_or(u64::MAX);
    let oldest_live = now_millis().saturating_sub(ttl_ms);
    registry
        .router_clients
        .retain(|client| client.last_seen_ms >= oldest_live);
    registry.worker_port_reservations.retain(|reservation| {
        registry.router_clients.iter().any(|client| {
            client.workspace == reservation.workspace && client.pid == reservation.pid
        })
    });
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
    prune_stale_router_clients(&mut registry);

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
    registry.worker_port_reservations.retain(|reservation| {
        !(reservation.workspace == workspace
            && reservation.pid == std::process::id()
            && reservation.port == port)
    });
    save_registry(&path, &registry)?;
    Ok(registration)
}

pub fn reserve_worker_port(workspace_root: &str) -> io::Result<u16> {
    let path = registry_path()?;
    reserve_worker_port_at(&path, workspace_root)
}

fn reserve_worker_port_at(path: &Path, workspace_root: &str) -> io::Result<u16> {
    reserve_worker_port_at_for(path, workspace_root, std::process::id())
}

fn reserve_worker_port_at_for(path: &Path, workspace_root: &str, pid: u32) -> io::Result<u16> {
    let _lock = acquire_registry_lock(path)?;
    let mut registry = load_registry(path)?;
    prune_stale(&mut registry);
    prune_stale_router_clients(&mut registry);
    let workspace =
        canonical_workspace(workspace_root).unwrap_or_else(|_| workspace_root.to_string());
    if let Some(existing) = registry
        .worker_port_reservations
        .iter()
        .find(|reservation| reservation.workspace == workspace && reservation.pid == pid)
    {
        return Ok(existing.port);
    }

    for port in WORKER_PORT_START..=WORKER_PORT_END {
        let already_used = registry.workspaces.iter().any(|entry| entry.port == port)
            || registry
                .worker_port_reservations
                .iter()
                .any(|reservation| reservation.port == port);
        if !already_used && TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
            registry
                .worker_port_reservations
                .push(WorkerPortReservation {
                    workspace,
                    pid,
                    port,
                });
            save_registry(path, &registry)?;
            return Ok(port);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        format!("no free CatDesk workspace worker port in {WORKER_PORT_START}..={WORKER_PORT_END}"),
    ))
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

    if registry.workspaces.is_empty()
        && registry.sessions.is_empty()
        && registry.router_clients.is_empty()
        && registry.worker_port_reservations.is_empty()
    {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    save_registry(&path, &registry)
}

pub fn register_router_client(workspace_root: &str) -> io::Result<RouterClientGuard> {
    touch_router_client(workspace_root)?;
    Ok(RouterClientGuard {
        workspace_root: workspace_root.to_string(),
    })
}

pub fn touch_router_client(workspace_root: &str) -> io::Result<()> {
    let path = registry_path()?;
    let _lock = acquire_registry_lock(&path)?;
    let mut registry = load_registry(&path)?;
    prune_stale_router_clients(&mut registry);
    let workspace =
        canonical_workspace(workspace_root).unwrap_or_else(|_| workspace_root.to_string());
    let pid = std::process::id();
    let client = RouterClient {
        workspace: workspace.clone(),
        pid,
        last_seen_ms: now_millis(),
    };
    if let Some(existing) = registry
        .router_clients
        .iter_mut()
        .find(|existing| existing.workspace == workspace && existing.pid == pid)
    {
        *existing = client;
    } else {
        registry.router_clients.push(client);
    }
    save_registry(&path, &registry)
}

pub fn unregister_router_client(workspace_root: &str) -> io::Result<()> {
    let path = registry_path()?;
    let _lock = acquire_registry_lock(&path)?;
    let mut registry = load_registry(&path)?;
    let workspace =
        canonical_workspace(workspace_root).unwrap_or_else(|_| workspace_root.to_string());
    let pid = std::process::id();
    registry
        .router_clients
        .retain(|client| !(client.workspace == workspace && client.pid == pid));
    registry
        .worker_port_reservations
        .retain(|reservation| !(reservation.workspace == workspace && reservation.pid == pid));
    if registry.workspaces.is_empty()
        && registry.sessions.is_empty()
        && registry.router_clients.is_empty()
        && registry.worker_port_reservations.is_empty()
    {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    save_registry(&path, &registry)
}

pub fn has_live_router_clients() -> io::Result<bool> {
    let path = registry_path()?;
    has_live_router_clients_at(&path)
}

fn has_live_router_clients_at(path: &Path) -> io::Result<bool> {
    let _lock = acquire_registry_lock(path)?;
    let mut registry = load_registry(path)?;
    prune_stale_router_clients(&mut registry);
    let has_clients = !registry.router_clients.is_empty();
    if registry.workspaces.is_empty()
        && registry.sessions.is_empty()
        && registry.worker_port_reservations.is_empty()
        && !has_clients
    {
        let _ = fs::remove_file(path);
    } else {
        save_registry(path, &registry)?;
    }
    Ok(has_clients)
}

/// Return live workspace endpoints for session-independent tool discovery.
pub fn live_workspaces() -> Result<Vec<WorkspaceRegistration>, String> {
    let path = registry_path().map_err(|error| error.to_string())?;
    let _lock = acquire_registry_lock(&path).map_err(|error| error.to_string())?;
    let mut registry = load_registry(&path).map_err(|error| error.to_string())?;
    prune_stale(&mut registry);
    save_registry(&path, &registry).map_err(|error| error.to_string())?;
    Ok(registry.workspaces)
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

pub fn bound_registration(session_id: &str) -> Result<Option<WorkspaceRegistration>, String> {
    let path = registry_path().map_err(|error| error.to_string())?;
    bound_registration_at(&path, session_id)
}

fn bound_registration_at(
    path: &Path,
    session_id: &str,
) -> Result<Option<WorkspaceRegistration>, String> {
    let _lock = acquire_registry_lock(path).map_err(|error| error.to_string())?;
    let mut registry = load_registry(path).map_err(|error| error.to_string())?;
    prune_stale(&mut registry);
    let registration = registry
        .sessions
        .get(session_id)
        .and_then(|workspace| {
            registry
                .workspaces
                .iter()
                .find(|entry| &entry.workspace == workspace)
        })
        .cloned();
    save_registry(path, &registry).map_err(|error| error.to_string())?;
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
                ..WorkspaceRegistry::default()
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
                ..WorkspaceRegistry::default()
            },
        );

        assert!(resolve_session_at(&path, "new-session").is_err());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn router_liveness_keeps_fresh_clients_and_prunes_expired_clients() {
        let fresh_path = test_registry_path("fresh-router-client");
        save_test_registry(
            &fresh_path,
            &WorkspaceRegistry {
                router_clients: vec![RouterClient {
                    workspace: "/tmp/project-a".into(),
                    pid: 1,
                    last_seen_ms: now_millis(),
                }],
                ..WorkspaceRegistry::default()
            },
        );
        assert!(has_live_router_clients_at(&fresh_path).expect("fresh client is live"));

        let expired_path = test_registry_path("expired-router-client");
        save_test_registry(
            &expired_path,
            &WorkspaceRegistry {
                router_clients: vec![RouterClient {
                    workspace: "/tmp/project-a".into(),
                    pid: 1,
                    last_seen_ms: now_millis()
                        .saturating_sub(ROUTER_CLIENT_TTL.as_millis().try_into().unwrap())
                        .saturating_sub(1),
                }],
                ..WorkspaceRegistry::default()
            },
        );
        assert!(!has_live_router_clients_at(&expired_path).expect("expired client is pruned"));
        assert!(!expired_path.exists());

        let _ = fs::remove_file(fresh_path);
    }

    #[test]
    fn worker_port_reservations_are_distinct_for_concurrent_clients() {
        let path = test_registry_path("worker-port-reservations");
        save_test_registry(
            &path,
            &WorkspaceRegistry {
                router_clients: vec![
                    RouterClient {
                        workspace: "/tmp/project-a".into(),
                        pid: 1,
                        last_seen_ms: now_millis(),
                    },
                    RouterClient {
                        workspace: "/tmp/project-b".into(),
                        pid: 2,
                        last_seen_ms: now_millis(),
                    },
                ],
                ..WorkspaceRegistry::default()
            },
        );

        let first = reserve_worker_port_at_for(&path, "/tmp/project-a", 1)
            .expect("reserve first worker port");
        let second = reserve_worker_port_at_for(&path, "/tmp/project-b", 2)
            .expect("reserve second worker port");
        assert_ne!(first, second);

        let _ = fs::remove_file(path);
    }
}
