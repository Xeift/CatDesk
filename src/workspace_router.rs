use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::state::user_home_dir;

pub const ROUTER_PORT: u16 = 3200;
pub const WORKER_PORT_START: u16 = 3201;
pub const WORKER_PORT_END: u16 = 3299;
const REGISTRY_FILE_NAME: &str = "workspace_id.toml";
const LOCK_RETRY_COUNT: usize = 100;
const ROUTER_START_LOCK_RETRY_COUNT: usize = 3_000;
const PORT_PROBE_TIMEOUT: Duration = Duration::from_millis(100);
const ROUTER_CLIENT_TTL: Duration = Duration::from_secs(30);
const SESSION_SELECTION_TIMEOUT: Duration = Duration::from_secs(60);
const SESSION_SELECTION_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRegistration {
    pub workspace: String,
    pub port: u16,
    pub pid: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSessionSelection {
    pub session_id: String,
    pub generation: u64,
    pub workspaces: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PendingSession {
    session_id: String,
    generation: u64,
    workspaces: Vec<String>,
    active_since_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceRegistry {
    #[serde(default)]
    workspaces: Vec<WorkspaceRegistration>,
    #[serde(default)]
    sessions: BTreeMap<String, String>,
    #[serde(default)]
    pending_sessions: Vec<PendingSession>,
    #[serde(default)]
    next_pending_generation: u64,
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
    _guard: crate::file_lock::OwnedFileLock,
}

fn registry_path() -> io::Result<PathBuf> {
    Ok(user_home_dir()?.join(".catdesk").join(REGISTRY_FILE_NAME))
}

fn registry_lock_path(path: &Path) -> PathBuf {
    path.with_extension("lock")
}

pub fn acquire_router_start_lock() -> io::Result<RouterStartupLock> {
    let path = registry_path()?.with_file_name("router_start.toml");
    let guard = crate::file_lock::acquire(&path, ROUTER_START_LOCK_RETRY_COUNT)?;
    Ok(RouterStartupLock { _guard: guard })
}

#[cfg(unix)]
pub(crate) fn process_is_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
pub(crate) fn process_is_alive(pid: u32) -> bool {
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

fn acquire_registry_lock(path: &Path) -> io::Result<crate::file_lock::OwnedFileLock> {
    acquire_registry_lock_with_retries(path, LOCK_RETRY_COUNT)
}

fn acquire_registry_lock_with_retries(
    path: &Path,
    retry_count: usize,
) -> io::Result<crate::file_lock::OwnedFileLock> {
    crate::file_lock::acquire(&registry_lock_path(path), retry_count)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegistrationStatus {
    Live,
    Uncertain,
    Stale,
}

/// Do not trust an open port on its own: it may belong to another process.
/// A dead PID or a positive identity mismatch is stale. Transport failures are
/// inconclusive while the PID is still alive, so keep the registration/session.
fn registration_status(registration: &WorkspaceRegistration) -> RegistrationStatus {
    if !process_is_alive(registration.pid) {
        return RegistrationStatus::Stale;
    }
    #[cfg(test)]
    let slug = "test".to_string();
    #[cfg(not(test))]
    let Some(slug) = crate::state::load_app_config()
        .ok()
        .and_then(|config| config.mcp_slug)
    else {
        return RegistrationStatus::Uncertain;
    };
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), registration.port);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, PORT_PROBE_TIMEOUT) else {
        return RegistrationStatus::Uncertain;
    };
    let _ = stream.set_read_timeout(Some(PORT_PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PORT_PROBE_TIMEOUT));
    if write!(
        stream,
        "GET /{slug} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .is_err()
    {
        return RegistrationStatus::Uncertain;
    }
    let mut response = Vec::new();
    if stream.take(64 * 1024).read_to_end(&mut response).is_err() {
        return RegistrationStatus::Uncertain;
    }
    let Some(body_offset) = response.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
        return RegistrationStatus::Stale;
    };
    let Ok(body) = serde_json::from_slice::<Value>(&response[body_offset + 4..]) else {
        return RegistrationStatus::Stale;
    };
    if body.get("name").and_then(Value::as_str) == Some("CatDesk")
        && body.get("pid").and_then(Value::as_u64) == Some(registration.pid as u64)
        && body.get("workspaceCanonical").and_then(Value::as_str)
            == Some(registration.workspace.as_str())
        && body.get("router").and_then(Value::as_bool) == Some(false)
    {
        RegistrationStatus::Live
    } else {
        RegistrationStatus::Stale
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn activate_front_pending(registry: &mut WorkspaceRegistry) {
    if let Some(front) = registry.pending_sessions.first_mut()
        && front.active_since_ms.is_none()
    {
        front.active_since_ms = Some(now_millis());
    }
}

fn remove_pending_session(registry: &mut WorkspaceRegistry, session_id: &str) {
    registry
        .pending_sessions
        .retain(|pending| pending.session_id != session_id);
    activate_front_pending(registry);
}

fn remove_pending_generation(registry: &mut WorkspaceRegistry, session_id: &str, generation: u64) {
    registry
        .pending_sessions
        .retain(|pending| pending.session_id != session_id || pending.generation != generation);
    activate_front_pending(registry);
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

fn prune_stale(registry: &mut WorkspaceRegistry) -> BTreeMap<String, RegistrationStatus> {
    let statuses = registry
        .workspaces
        .iter()
        .map(|registration| {
            (
                registration.workspace.clone(),
                registration_status(registration),
            )
        })
        .collect::<BTreeMap<_, _>>();
    registry.workspaces.retain(|registration| {
        statuses.get(&registration.workspace) != Some(&RegistrationStatus::Stale)
    });
    registry.sessions.retain(|_, workspace| {
        registry
            .workspaces
            .iter()
            .any(|registration| &registration.workspace == workspace)
    });
    for pending in &mut registry.pending_sessions {
        pending.workspaces.retain(|workspace| {
            registry
                .workspaces
                .iter()
                .any(|registration| &registration.workspace == workspace)
        });
    }
    registry
        .pending_sessions
        .retain(|pending| !pending.workspaces.is_empty());
    activate_front_pending(registry);
    statuses
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

    // Restarting CatDesk in a workspace releases its existing session bindings so
    // those conversations resolve the workspace again on their next tool call.
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
    for pending in &mut registry.pending_sessions {
        pending
            .workspaces
            .retain(|candidate| candidate != &workspace);
    }
    registry
        .pending_sessions
        .retain(|pending| !pending.workspaces.is_empty());
    activate_front_pending(&mut registry);

    if registry.workspaces.is_empty()
        && registry.sessions.is_empty()
        && registry.pending_sessions.is_empty()
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
        && registry.pending_sessions.is_empty()
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

/// Connector identity changes must ignore this worker's own keepalive.
pub fn has_other_live_router_clients(workspace: &str) -> io::Result<bool> {
    let path = registry_path()?;
    let _lock = acquire_registry_lock(&path)?;
    let mut registry = load_registry(&path)?;
    prune_stale_router_clients(&mut registry);
    let has_others = registry
        .router_clients
        .iter()
        .any(|client| client.pid != std::process::id() || client.workspace != workspace);
    save_registry(&path, &registry)?;
    Ok(has_others)
}

fn has_live_router_clients_at(path: &Path) -> io::Result<bool> {
    let _lock = acquire_registry_lock(path)?;
    let mut registry = load_registry(path)?;
    prune_stale_router_clients(&mut registry);
    let has_clients = !registry.router_clients.is_empty();
    if registry.workspaces.is_empty()
        && registry.sessions.is_empty()
        && registry.pending_sessions.is_empty()
        && registry.worker_port_reservations.is_empty()
        && !has_clients
    {
        let _ = fs::remove_file(path);
    } else {
        save_registry(path, &registry)?;
    }
    Ok(has_clients)
}

enum SessionResolution {
    Resolved(WorkspaceRegistration),
    AwaitSelection { generation: u64 },
}

/// Resolve a ChatGPT session to a workspace. A single live workspace binds
/// immediately; multiple live workspaces wait for the user to choose in TUI.
pub fn resolve_session(session_id: &str) -> Result<WorkspaceRegistration, String> {
    let path = registry_path().map_err(|error| error.to_string())?;
    resolve_session_at(&path, session_id)
}

fn resolve_session_at(path: &Path, session_id: &str) -> Result<WorkspaceRegistration, String> {
    match begin_session_resolution_at(path, session_id)? {
        SessionResolution::Resolved(registration) => Ok(registration),
        SessionResolution::AwaitSelection { generation } => {
            wait_for_session_selection_at(path, session_id, generation)
        }
    }
}

fn begin_session_resolution_at(path: &Path, session_id: &str) -> Result<SessionResolution, String> {
    let _lock = acquire_registry_lock(path).map_err(|error| error.to_string())?;
    let mut registry = load_registry(path).map_err(|error| error.to_string())?;
    let statuses = prune_stale(&mut registry);

    if let Some(workspace) = registry.sessions.get(session_id).cloned() {
        if let Some(registration) = registry
            .workspaces
            .iter()
            .find(|entry| entry.workspace == workspace)
            .cloned()
        {
            remove_pending_session(&mut registry, session_id);
            save_registry(path, &registry).map_err(|error| error.to_string())?;
            return Ok(SessionResolution::Resolved(registration));
        }
        registry.sessions.remove(session_id);
    }

    let live = registry
        .workspaces
        .iter()
        .filter(|entry| statuses.get(&entry.workspace) == Some(&RegistrationStatus::Live))
        .cloned()
        .collect::<Vec<_>>();

    match live.as_slice() {
        [] => {
            remove_pending_session(&mut registry, session_id);
            save_registry(path, &registry).map_err(|error| error.to_string())?;
            Err("No live CatDesk workspace is available for this ChatGPT session. Start CatDesk in the workspace you want to use, then retry the tool call.".to_string())
        }
        [only] => {
            registry
                .sessions
                .insert(session_id.to_string(), only.workspace.clone());
            remove_pending_session(&mut registry, session_id);
            save_registry(path, &registry).map_err(|error| error.to_string())?;
            Ok(SessionResolution::Resolved(only.clone()))
        }
        _ => {
            let workspaces = live
                .iter()
                .map(|entry| entry.workspace.clone())
                .collect::<Vec<_>>();
            let generation = if let Some(existing) = registry
                .pending_sessions
                .iter_mut()
                .find(|pending| pending.session_id == session_id)
            {
                existing.workspaces = workspaces;
                if existing.active_since_ms.is_some() {
                    existing.active_since_ms = Some(now_millis());
                }
                existing.generation
            } else {
                registry.next_pending_generation =
                    registry.next_pending_generation.saturating_add(1);
                let generation = registry.next_pending_generation;
                registry.pending_sessions.push(PendingSession {
                    session_id: session_id.to_string(),
                    generation,
                    workspaces,
                    active_since_ms: None,
                });
                activate_front_pending(&mut registry);
                generation
            };
            save_registry(path, &registry).map_err(|error| error.to_string())?;
            Ok(SessionResolution::AwaitSelection { generation })
        }
    }
}

fn wait_for_session_selection_at(
    path: &Path,
    session_id: &str,
    generation: u64,
) -> Result<WorkspaceRegistration, String> {
    let timeout_ms: u64 = SESSION_SELECTION_TIMEOUT
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    loop {
        std::thread::sleep(SESSION_SELECTION_POLL_INTERVAL);
        let _lock = acquire_registry_lock(path).map_err(|error| error.to_string())?;
        let mut registry = load_registry(path).map_err(|error| error.to_string())?;
        if let Some(workspace) = registry.sessions.get(session_id)
            && let Some(registration) = registry
                .workspaces
                .iter()
                .find(|entry| &entry.workspace == workspace)
                .cloned()
        {
            return Ok(registration);
        }
        let Some(pending) = registry
            .pending_sessions
            .iter()
            .find(|pending| pending.session_id == session_id && pending.generation == generation)
        else {
            return Err("The CatDesk workspace selection is no longer pending.".to_string());
        };
        let timed_out = pending
            .active_since_ms
            .is_some_and(|started| now_millis().saturating_sub(started) >= timeout_ms);
        if timed_out {
            remove_pending_generation(&mut registry, session_id, generation);
            save_registry(path, &registry).map_err(|error| error.to_string())?;
            return Err("Timed out waiting for a CatDesk workspace selection in TUI.".to_string());
        }
    }
}

pub fn pending_session_selection() -> Result<Option<PendingSessionSelection>, String> {
    let path = registry_path().map_err(|error| error.to_string())?;
    pending_session_selection_at(&path)
}

fn pending_session_selection_at(path: &Path) -> Result<Option<PendingSessionSelection>, String> {
    let _lock = acquire_registry_lock(path).map_err(|error| error.to_string())?;
    let registry = load_registry(path).map_err(|error| error.to_string())?;
    Ok(registry
        .pending_sessions
        .first()
        .map(|pending| PendingSessionSelection {
            session_id: pending.session_id.clone(),
            generation: pending.generation,
            workspaces: pending.workspaces.clone(),
        }))
}

pub fn bind_pending_session(
    session_id: &str,
    generation: u64,
    workspace: &str,
) -> Result<WorkspaceRegistration, String> {
    let path = registry_path().map_err(|error| error.to_string())?;
    bind_pending_session_at(&path, session_id, generation, workspace)
}

fn bind_pending_session_at(
    path: &Path,
    session_id: &str,
    generation: u64,
    workspace: &str,
) -> Result<WorkspaceRegistration, String> {
    let _lock = acquire_registry_lock(path).map_err(|error| error.to_string())?;
    let mut registry = load_registry(path).map_err(|error| error.to_string())?;

    if let Some(bound_workspace) = registry.sessions.get(session_id) {
        let registration = registry
            .workspaces
            .iter()
            .find(|entry| &entry.workspace == bound_workspace)
            .cloned()
            .ok_or_else(|| "The selected CatDesk workspace is no longer registered.".to_string())?;
        remove_pending_session(&mut registry, session_id);
        save_registry(path, &registry).map_err(|error| error.to_string())?;
        return Ok(registration);
    }

    let pending = registry
        .pending_sessions
        .iter()
        .find(|pending| pending.session_id == session_id && pending.generation == generation)
        .ok_or_else(|| {
            "This ChatGPT session is no longer waiting for this workspace selection.".to_string()
        })?;
    if !pending
        .workspaces
        .iter()
        .any(|candidate| candidate == workspace)
    {
        return Err(
            "The selected workspace is not available for this ChatGPT session.".to_string(),
        );
    }

    let registration = registry
        .workspaces
        .iter()
        .find(|entry| entry.workspace == workspace)
        .cloned()
        .ok_or_else(|| "The selected CatDesk workspace is no longer registered.".to_string())?;
    if registration_status(&registration) != RegistrationStatus::Live {
        return Err("The selected CatDesk workspace is not currently available.".to_string());
    }

    registry
        .sessions
        .insert(session_id.to_string(), workspace.to_string());
    remove_pending_generation(&mut registry, session_id, generation);
    save_registry(path, &registry).map_err(|error| error.to_string())?;
    Ok(registration)
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
    response.json::<Value>().await.ok().is_some_and(|value| {
        value.get("name").and_then(Value::as_str) == Some("CatDesk")
            && value.get("router").and_then(Value::as_bool) == Some(true)
            && value.get("ngrokRunning").and_then(Value::as_bool) == Some(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    struct MockHealthWorker {
        port: u16,
        active: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl MockHealthWorker {
        fn new(workspace: &str) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let active = Arc::new(AtomicBool::new(true));
            let worker_active = active.clone();
            let workspace = workspace.to_string();
            let thread = std::thread::spawn(move || {
                while worker_active.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                            let mut request = [0_u8; 1024];
                            let _ = stream.read(&mut request);
                            let body = serde_json::json!({
                                "name":"CatDesk", "pid":std::process::id(),
                                "router":false, "workspace":workspace,
                                "workspaceCanonical":workspace
                            })
                            .to_string();
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                body.len(),
                                body
                            );
                            let _ = stream.write_all(response.as_bytes());
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                port,
                active,
                thread: Some(thread),
            }
        }
    }
    impl Drop for MockHealthWorker {
        fn drop(&mut self) {
            self.active.store(false, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

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
    fn multiple_sessions_share_single_live_workspace() {
        let path = test_registry_path("shared-binding");
        let worker = MockHealthWorker::new("/tmp/project-a");
        save_test_registry(
            &path,
            &WorkspaceRegistry {
                workspaces: vec![WorkspaceRegistration {
                    workspace: "/tmp/project-a".into(),
                    port: worker.port,
                    pid: std::process::id(),
                }],
                ..WorkspaceRegistry::default()
            },
        );

        let first = resolve_session_at(&path, "session-a").expect("bind first session");
        let second = resolve_session_at(&path, "session-b").expect("bind second session");

        assert_eq!(first.workspace, "/tmp/project-a");
        assert_eq!(second, first);
        let registry = load_registry(&path).unwrap();
        assert_eq!(registry.sessions.len(), 2);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn multiple_live_workspaces_wait_for_explicit_selection() {
        let path = test_registry_path("workspace-selection");
        let worker_a = MockHealthWorker::new("/tmp/project-a");
        let worker_b = MockHealthWorker::new("/tmp/project-b");
        save_test_registry(
            &path,
            &WorkspaceRegistry {
                workspaces: vec![
                    WorkspaceRegistration {
                        workspace: "/tmp/project-a".into(),
                        port: worker_a.port,
                        pid: std::process::id(),
                    },
                    WorkspaceRegistration {
                        workspace: "/tmp/project-b".into(),
                        port: worker_b.port,
                        pid: std::process::id(),
                    },
                ],
                ..WorkspaceRegistry::default()
            },
        );

        let generation = match begin_session_resolution_at(&path, "session-a")
            .expect("start selection")
        {
            SessionResolution::AwaitSelection { generation } => generation,
            SessionResolution::Resolved(_) => panic!("multiple workspaces must require selection"),
        };
        let pending = pending_session_selection_at(&path)
            .expect("read pending selection")
            .expect("selection exists");
        assert_eq!(pending.generation, generation);
        assert_eq!(
            pending.workspaces,
            vec!["/tmp/project-a".to_string(), "/tmp/project-b".to_string()]
        );

        let selected = bind_pending_session_at(&path, "session-a", generation, "/tmp/project-b")
            .expect("bind selected workspace");
        assert_eq!(selected.workspace, "/tmp/project-b");
        let resolved = resolve_session_at(&path, "session-a").expect("reuse binding");
        assert_eq!(resolved.workspace, "/tmp/project-b");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn pending_sessions_are_fifo_and_timeout_starts_when_front_becomes_active() {
        let path = test_registry_path("workspace-selection-queue");
        let worker_a = MockHealthWorker::new("/tmp/project-a");
        let worker_b = MockHealthWorker::new("/tmp/project-b");
        save_test_registry(
            &path,
            &WorkspaceRegistry {
                workspaces: vec![
                    WorkspaceRegistration {
                        workspace: "/tmp/project-a".into(),
                        port: worker_a.port,
                        pid: std::process::id(),
                    },
                    WorkspaceRegistration {
                        workspace: "/tmp/project-b".into(),
                        port: worker_b.port,
                        pid: std::process::id(),
                    },
                ],
                ..WorkspaceRegistry::default()
            },
        );

        let first_generation = match begin_session_resolution_at(&path, "session-z")
            .expect("queue first session")
        {
            SessionResolution::AwaitSelection { generation } => generation,
            SessionResolution::Resolved(_) => panic!("multiple workspaces must require selection"),
        };
        let second_generation = match begin_session_resolution_at(&path, "session-a")
            .expect("queue second session")
        {
            SessionResolution::AwaitSelection { generation } => generation,
            SessionResolution::Resolved(_) => panic!("multiple workspaces must require selection"),
        };

        let queued = load_registry(&path).unwrap();
        assert_eq!(queued.pending_sessions.len(), 2);
        assert_eq!(queued.pending_sessions[0].session_id, "session-z");
        assert!(queued.pending_sessions[0].active_since_ms.is_some());
        assert_eq!(queued.pending_sessions[1].session_id, "session-a");
        assert!(queued.pending_sessions[1].active_since_ms.is_none());
        drop(queued);

        let retried_generation = match begin_session_resolution_at(&path, "session-a")
            .expect("retry queued session")
        {
            SessionResolution::AwaitSelection { generation } => generation,
            SessionResolution::Resolved(_) => panic!("multiple workspaces must require selection"),
        };
        assert_eq!(retried_generation, second_generation);
        let still_queued = load_registry(&path).unwrap();
        assert!(still_queued.pending_sessions[1].active_since_ms.is_none());
        drop(still_queued);

        let visible = pending_session_selection_at(&path)
            .expect("read first queued selection")
            .expect("selection exists");
        assert_eq!(visible.session_id, "session-z");

        bind_pending_session_at(&path, "session-z", first_generation, "/tmp/project-a")
            .expect("bind first queued session");

        let promoted = load_registry(&path).unwrap();
        assert_eq!(promoted.pending_sessions.len(), 1);
        assert_eq!(promoted.pending_sessions[0].session_id, "session-a");
        assert_eq!(promoted.pending_sessions[0].generation, second_generation);
        assert!(promoted.pending_sessions[0].active_since_ms.is_some());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn repeated_resolution_for_same_session_joins_existing_pending_generation() {
        let path = test_registry_path("workspace-selection-retry");
        let worker_a = MockHealthWorker::new("/tmp/project-a");
        let worker_b = MockHealthWorker::new("/tmp/project-b");
        save_test_registry(
            &path,
            &WorkspaceRegistry {
                workspaces: vec![
                    WorkspaceRegistration {
                        workspace: "/tmp/project-a".into(),
                        port: worker_a.port,
                        pid: std::process::id(),
                    },
                    WorkspaceRegistration {
                        workspace: "/tmp/project-b".into(),
                        port: worker_b.port,
                        pid: std::process::id(),
                    },
                ],
                ..WorkspaceRegistry::default()
            },
        );

        let first_generation = match begin_session_resolution_at(&path, "session-a")
            .expect("start first request")
        {
            SessionResolution::AwaitSelection { generation } => generation,
            SessionResolution::Resolved(_) => panic!("multiple workspaces must require selection"),
        };
        let old_active_since = now_millis().saturating_sub(10_000);
        let mut registry = load_registry(&path).unwrap();
        registry.pending_sessions[0].active_since_ms = Some(old_active_since);
        save_test_registry(&path, &registry);

        let second_generation = match begin_session_resolution_at(&path, "session-a")
            .expect("join second request")
        {
            SessionResolution::AwaitSelection { generation } => generation,
            SessionResolution::Resolved(_) => panic!("multiple workspaces must require selection"),
        };

        assert_eq!(second_generation, first_generation);
        let registry = load_registry(&path).unwrap();
        assert_eq!(registry.pending_sessions.len(), 1);
        assert_eq!(registry.pending_sessions[0].generation, first_generation);
        assert!(
            registry.pending_sessions[0]
                .active_since_ms
                .is_some_and(|refreshed| refreshed > old_active_since)
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn stale_waiter_generation_cannot_remove_new_pending_selection() {
        let path = test_registry_path("workspace-selection-generation");
        let mut registry = WorkspaceRegistry {
            next_pending_generation: 2,
            pending_sessions: vec![PendingSession {
                session_id: "session-a".into(),
                generation: 2,
                workspaces: vec!["/tmp/project-a".into(), "/tmp/project-b".into()],
                active_since_ms: Some(now_millis()),
            }],
            ..WorkspaceRegistry::default()
        };
        activate_front_pending(&mut registry);
        save_test_registry(&path, &registry);

        assert!(wait_for_session_selection_at(&path, "session-a", 1).is_err());
        let remaining = load_registry(&path).unwrap();
        assert_eq!(remaining.pending_sessions.len(), 1);
        assert_eq!(remaining.pending_sessions[0].generation, 2);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn live_pid_with_unreachable_worker_keeps_existing_session_binding() {
        let path = test_registry_path("unreachable-live-worker");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let mut sessions = BTreeMap::new();
        sessions.insert("existing-session".into(), "/tmp/project-a".into());
        save_test_registry(
            &path,
            &WorkspaceRegistry {
                workspaces: vec![WorkspaceRegistration {
                    workspace: "/tmp/project-a".into(),
                    port,
                    pid: std::process::id(),
                }],
                sessions,
                ..WorkspaceRegistry::default()
            },
        );

        let existing = resolve_session_at(&path, "existing-session")
            .expect("existing binding survives inconclusive health probe");
        assert_eq!(existing.workspace, "/tmp/project-a");
        assert!(resolve_session_at(&path, "new-session").is_err());

        let remaining = load_registry(&path).unwrap();
        assert_eq!(remaining.workspaces.len(), 1);
        assert_eq!(
            remaining
                .sessions
                .get("existing-session")
                .map(String::as_str),
            Some("/tmp/project-a")
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn foreign_service_with_non_catdesk_response_is_stale() {
        let path = test_registry_path("foreign-non-catdesk");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let body = "<html>not CatDesk</html>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
        });

        let mut sessions = BTreeMap::new();
        sessions.insert("existing-session".into(), "/tmp/project-a".into());
        save_test_registry(
            &path,
            &WorkspaceRegistry {
                workspaces: vec![WorkspaceRegistration {
                    workspace: "/tmp/project-a".into(),
                    port,
                    pid: std::process::id(),
                }],
                sessions,
                ..WorkspaceRegistry::default()
            },
        );

        assert!(resolve_session_at(&path, "existing-session").is_err());
        thread.join().unwrap();
        let remaining = load_registry(&path).unwrap();
        assert!(remaining.workspaces.is_empty());
        assert!(remaining.sessions.is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn foreign_process_on_registered_port_is_not_trusted() {
        let path = test_registry_path("foreign-worker");
        let foreign = MockHealthWorker::new("/tmp/different-workspace");
        save_test_registry(
            &path,
            &WorkspaceRegistry {
                workspaces: vec![WorkspaceRegistration {
                    workspace: "/tmp/expected-workspace".into(),
                    port: foreign.port,
                    pid: std::process::id(),
                }],
                ..WorkspaceRegistry::default()
            },
        );
        assert!(resolve_session_at(&path, "new-session").is_err());
        let remaining = load_registry(&path).unwrap();
        assert!(remaining.workspaces.is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn occupied_workspace_can_bind_another_session() {
        let path = test_registry_path("shared-workspace");
        let worker = MockHealthWorker::new("/tmp/project-a");
        let mut sessions = BTreeMap::new();
        sessions.insert("existing".into(), "/tmp/project-a".into());
        save_test_registry(
            &path,
            &WorkspaceRegistry {
                workspaces: vec![WorkspaceRegistration {
                    workspace: "/tmp/project-a".into(),
                    port: worker.port,
                    pid: std::process::id(),
                }],
                sessions,
                ..WorkspaceRegistry::default()
            },
        );

        let resolved = resolve_session_at(&path, "new-session").expect("share workspace");
        assert_eq!(resolved.workspace, "/tmp/project-a");
        let registry = load_registry(&path).unwrap();
        assert_eq!(registry.sessions.len(), 2);
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
