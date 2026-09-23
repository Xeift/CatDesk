use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt};
use std::path::{Path, PathBuf};
use std::process::Command;

fn canonical_existing(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to canonicalize {}: {error}", path.display()),
        )
    })
}

fn insert_existing(paths: &mut BTreeSet<PathBuf>, path: impl AsRef<Path>) {
    let path = path.as_ref();
    if let Ok(canonical) = path.canonicalize() {
        paths.insert(canonical);
    }
}

fn insert_env_path_list(paths: &mut BTreeSet<PathBuf>, variable: &str) {
    let Some(value) = std::env::var_os(variable) else {
        return;
    };
    for path in std::env::split_paths(&value) {
        insert_existing(paths, path);
    }
}

fn insert_env_path(paths: &mut BTreeSet<PathBuf>, variable: &str) {
    if let Some(path) = std::env::var_os(variable) {
        insert_existing(paths, PathBuf::from(path));
    }
}

fn insert_nvm_lib_path(paths: &mut BTreeSet<PathBuf>, nvm_bin: &Path, nvm_dir: &Path) {
    let Ok(nvm_bin) = nvm_bin.canonicalize() else {
        return;
    };
    let Ok(nvm_dir) = nvm_dir.canonicalize() else {
        return;
    };
    if nvm_bin.file_name() != Some(std::ffi::OsStr::new("bin")) {
        return;
    }
    let Some(version_root) = nvm_bin.parent() else {
        return;
    };
    let Ok(node_versions) = nvm_dir.join("versions/node").canonicalize() else {
        return;
    };
    if version_root.parent() != Some(node_versions.as_path()) {
        return;
    }
    let Some(lib) = real_dir(&version_root.join("lib")) else {
        return;
    };
    if lib.parent() != Some(version_root) {
        return;
    }

    paths.insert(lib);
}

fn insert_nvm_read_paths(paths: &mut BTreeSet<PathBuf>) {
    let (Some(nvm_bin), Some(nvm_dir)) = (std::env::var_os("NVM_BIN"), std::env::var_os("NVM_DIR"))
    else {
        return;
    };
    insert_nvm_lib_path(paths, Path::new(&nvm_bin), Path::new(&nvm_dir));
}

fn insert_ssh_read_paths(paths: &mut BTreeSet<PathBuf>, home: &Path) {
    let ssh_dir = home.join(".ssh");
    for name in ["config", "known_hosts", "known_hosts2"] {
        insert_existing(paths, ssh_dir.join(name));
    }
    if let Ok(entries) = std::fs::read_dir(&ssh_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "pub") {
                insert_existing(paths, path);
            }
        }
    }
}

fn existing_unix_socket(path: &Path) -> Option<PathBuf> {
    let canonical = path.canonicalize().ok()?;
    std::fs::metadata(&canonical)
        .ok()?
        .file_type()
        .is_socket()
        .then_some(canonical)
}

fn ssh_agent_socket() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("SSH_AUTH_SOCK")?);
    existing_unix_socket(&path)
}

fn runtime_read_paths() -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();

    for path in ["/bin", "/sbin", "/usr", "/lib", "/lib64", "/etc", "/sys"] {
        insert_existing(&mut paths, path);
    }

    insert_existing(&mut paths, "/etc/resolv.conf");

    // Executables installed outside the standard system prefixes must remain
    // executable when their directory is explicitly present in PATH.
    insert_env_path_list(&mut paths, "PATH");

    // NVM's npm/npx launchers in NVM_BIN are symlinks into the sibling lib
    // directory. Expose only that current version's lib tree, read-only.
    insert_nvm_read_paths(&mut paths);

    // Rust toolchains are commonly installed under the user's home directory.
    // Expose only executable/cache trees from Cargo so registry credentials
    // remain outside the sandbox. Rustup does not store registry credentials.
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME") {
        let cargo_home = PathBuf::from(cargo_home);
        insert_existing(&mut paths, cargo_home.join("bin"));
        insert_existing(&mut paths, cargo_home.join("registry"));
        insert_existing(&mut paths, cargo_home.join("git"));
    }
    insert_env_path(&mut paths, "RUSTUP_HOME");
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        let cargo_home = home.join(".cargo");
        insert_existing(&mut paths, cargo_home.join("bin"));
        insert_existing(&mut paths, cargo_home.join("registry"));
        insert_existing(&mut paths, cargo_home.join("git"));
        insert_existing(&mut paths, home.join(".rustup"));

        // Git treats an unreadable global config as fatal. Grant only the
        // configuration files, keeping credential stores and the rest of HOME
        // inaccessible.
        insert_existing(&mut paths, home.join(".gitconfig"));
        insert_existing(&mut paths, home.join(".config/git/config"));
        insert_ssh_read_paths(&mut paths, &home);
    }

    paths
}

fn real_dir(path: &Path) -> Option<PathBuf> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    path.canonicalize().ok()
}

fn git_config_value(config: &Path, key: &str, workspace: &Path) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    let git = executable_in_paths(std::env::split_paths(&path), workspace, "git")?;
    let output = Command::new(git)
        .arg("config")
        .arg("--file")
        .arg(config)
        .arg("--no-includes")
        .arg("--get")
        .arg(key)
        .current_dir("/")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn resolve_from(base: &Path, value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    if path.is_absolute() {
        path.canonicalize().ok()
    } else {
        base.join(path).canonicalize().ok()
    }
}

fn append_git_suffix(path: &Path) -> Option<PathBuf> {
    let mut path = path.to_path_buf();
    let mut name = OsString::from(path.file_name()?);
    name.push(".git");
    path.set_file_name(name);
    Some(path)
}

fn raw_common_git_dir(raw_git_dir: &Path) -> Option<&Path> {
    raw_git_dir
        .ancestors()
        .find(|path| path.file_name().and_then(|name| name.to_str()) == Some(".git"))
}

fn has_real_ancestor_git(workspace: &Path, common: &Path) -> bool {
    workspace.ancestors().skip(1).any(|ancestor| {
        real_dir(&ancestor.join(".git"))
            .as_deref()
            .is_some_and(|git| git == common)
    })
}

fn insert_validated_worktree_paths(
    paths: &mut BTreeSet<PathBuf>,
    workspace: &Path,
    dot_git: &Path,
    raw_git_dir: &Path,
    git_dir: &Path,
) {
    let Ok(dot_git) = dot_git.canonicalize() else {
        return;
    };
    let Ok(backpointer) = std::fs::read_to_string(git_dir.join("gitdir")) else {
        return;
    };
    let Some(backpointer) = resolve_from(git_dir, backpointer.trim()) else {
        return;
    };
    if backpointer != dot_git {
        return;
    }
    let Ok(common) = std::fs::read_to_string(git_dir.join("commondir")) else {
        return;
    };
    let common_path = git_dir.join(common.trim());
    if real_dir(&raw_git_dir.join(common.trim())).as_deref() != real_dir(&common_path).as_deref() {
        return;
    }
    let Some(common) = real_dir(&common_path) else {
        return;
    };
    if raw_common_git_dir(raw_git_dir)
        .and_then(real_dir)
        .as_deref()
        != Some(common.as_path())
    {
        return;
    }
    let Some(workspace_parent) = dot_git.parent().and_then(Path::parent) else {
        return;
    };
    if common.file_name().and_then(|name| name.to_str()) != Some(".git") {
        return;
    }
    if common.parent().and_then(Path::parent) != Some(workspace_parent)
        && !has_real_ancestor_git(workspace, &common)
    {
        return;
    }
    if !git_dir.starts_with(common.join("worktrees")) {
        return;
    }
    paths.insert(git_dir.to_path_buf());
    paths.insert(common);
}

fn insert_validated_submodule_paths(
    paths: &mut BTreeSet<PathBuf>,
    workspace: &Path,
    git_dir: &Path,
) {
    for ancestor in workspace.ancestors().skip(1) {
        let Some(super_git) = real_dir(&ancestor.join(".git")) else {
            continue;
        };
        let modules = super_git.join("modules");
        if !git_dir.starts_with(&modules) {
            continue;
        }
        if git_config_value(&git_dir.join("config"), "core.worktree", workspace)
            .and_then(|worktree| resolve_from(git_dir, &worktree))
            .as_deref()
            == Some(workspace)
        {
            paths.insert(git_dir.to_path_buf());
            return;
        }
        let Ok(name) = git_dir.strip_prefix(&modules) else {
            continue;
        };
        let key = format!("submodule.{}.path", name.to_string_lossy());
        if git_config_value(&ancestor.join(".gitmodules"), &key, workspace)
            .and_then(|path| resolve_from(ancestor, &path))
            .as_deref()
            == Some(workspace)
        {
            paths.insert(git_dir.to_path_buf());
            return;
        }
    }
}

fn insert_repo_symlink_targets(paths: &mut BTreeSet<PathBuf>, git_dir: &Path, repo_root: &Path) {
    let Some(project_objects) = real_dir(&repo_root.join("project-objects")) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(git_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_symlink()) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !["objects", "hooks", "rr-cache"].contains(&name) {
            continue;
        }
        let Ok(target) = path.canonicalize() else {
            continue;
        };
        if target.starts_with(&project_objects)
            && target
                .file_name()
                .and_then(|target_name| target_name.to_str())
                == Some(name)
        {
            paths.insert(target);
        }
    }
}

fn insert_validated_repo_paths(paths: &mut BTreeSet<PathBuf>, workspace: &Path, git_dir: &Path) {
    for ancestor in workspace.ancestors().skip(1) {
        let Some(repo_root) = real_dir(&ancestor.join(".repo")) else {
            continue;
        };
        let Some(projects_root) = real_dir(&repo_root.join("projects")) else {
            continue;
        };
        let Ok(relative) = workspace.strip_prefix(ancestor) else {
            continue;
        };
        let Some(project_git) = append_git_suffix(&repo_root.join("projects").join(relative))
        else {
            continue;
        };
        let Some(project_git) = real_dir(&project_git) else {
            continue;
        };
        if project_git != git_dir || !project_git.starts_with(&projects_root) {
            continue;
        }
        paths.insert(git_dir.to_path_buf());
        insert_repo_symlink_targets(paths, git_dir, &repo_root);
        return;
    }
}

/// Paths holding the workspace's git metadata when it lives outside the
/// workspace itself.
///
/// A plain checkout keeps `.git` inside the workspace, which is already
/// writable, so this returns nothing. Three common layouts put it elsewhere:
///
///   * `repo` checkouts symlink `.git` into `.repo/projects/<name>.git`, whose
///     `objects`, `hooks` and `rr-cache` are themselves symlinks into a shared
///     `.repo/project-objects` tree.
///   * git worktrees and submodules replace `.git` with a file containing a
///     `gitdir:` line. Worktrees have a `gitdir` backlink plus `commondir`;
///     submodules keep `core.worktree` in their git config.
///
/// Without these, every git command inside the sandbox fails with "not a git
/// repository", because the target is simply absent. They are writable rather
/// than read-only for parity with a plain checkout, where `.git` sits in the
/// writable workspace and commands like `git commit` work.
///
/// Every external path must match the specific workspace layout before being
/// granted; a pointer at an ancestor's `.git` or `.repo` is not enough by
/// itself.
fn workspace_git_paths(workspace: &Path) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();

    let dot_git = workspace.join(".git");
    let metadata = match std::fs::symlink_metadata(&dot_git) {
        Ok(metadata) => metadata,
        Err(_) => return paths,
    };

    // A real directory already sits inside the writable workspace.
    if metadata.is_dir() {
        return paths;
    }

    let (raw_git_dir, git_dir) = if metadata.is_file() {
        // "gitdir: <path>", possibly relative to the workspace.
        let contents = match std::fs::read_to_string(&dot_git) {
            Ok(contents) => contents,
            Err(_) => return paths,
        };
        let Some(target) = contents
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))
            .map(str::trim)
        else {
            return paths;
        };
        let raw = workspace.join(target);
        match raw.canonicalize() {
            Ok(path) => (raw, path),
            Err(_) => return paths,
        }
    } else {
        match dot_git.canonicalize() {
            Ok(path) => (dot_git.clone(), path),
            Err(_) => return paths,
        }
    };

    insert_validated_worktree_paths(&mut paths, workspace, &dot_git, &raw_git_dir, &git_dir);
    insert_validated_submodule_paths(&mut paths, workspace, &git_dir);
    insert_validated_repo_paths(&mut paths, workspace, &git_dir);
    paths
}

/// Locate an executable `bwrap` on PATH. Bubblewrap confines through mount
/// namespaces rather than an LSM, so it works on kernels far older than
/// Landlock's 5.13 baseline -- RHEL 8 / Rocky 8 (4.18), Ubuntu 20.04 (5.4) and
/// Debian 11 (5.10) included.
///
/// A non-executable file named `bwrap` earlier in PATH must not shadow a real
/// one later, so the execute bit is checked rather than just the file type.
fn executable_in_paths(
    paths: impl IntoIterator<Item = PathBuf>,
    workspace: &Path,
    executable: &str,
) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let workspace = workspace.canonicalize().ok();
    paths
        .into_iter()
        .map(|dir| dir.join(executable))
        .find_map(|candidate| {
            let metadata = std::fs::metadata(&candidate).ok()?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return None;
            }
            let canonical = candidate.canonicalize().ok()?;
            if workspace
                .as_ref()
                .is_some_and(|root| canonical.starts_with(root))
            {
                None
            } else {
                Some(canonical)
            }
        })
}

fn bubblewrap_executable_in_paths(
    paths: impl IntoIterator<Item = PathBuf>,
    workspace: &Path,
) -> Option<PathBuf> {
    executable_in_paths(paths, workspace, "bwrap")
}

fn bubblewrap_executable(workspace: &Path) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    bubblewrap_executable_in_paths(std::env::split_paths(&path), workspace)
}

/// Build a bubblewrap invocation that confines `command` to `workspace` plus its
/// private `scratch` directory.
///
/// The namespace contains only what [`runtime_read_paths`] returns plus the
/// workspace, scratch and any external git metadata directories, so an unbound
/// path is simply absent rather than merely denied. `--dev /dev` supplies a
/// minimal set of device nodes (`/dev/null`, `/dev/zero`, `/dev/random`,
/// `/dev/tty` and the like), `--tmpfs /tmp` keeps the host's `/tmp` out of
/// reach, and `--unshare-pid` hides host processes. `--new-session` gives the
/// sandboxed command its own session.
fn bubblewrap_command(
    bwrap: &Path,
    command: &str,
    workspace: &Path,
    cwd: &Path,
    scratch: &Path,
) -> io::Result<Command> {
    let workspace = canonical_existing(workspace)?;
    let cwd = canonical_existing(cwd)?;
    let scratch = canonical_existing(scratch)?;

    let mut bwrap_command = Command::new(bwrap);
    bwrap_command
        .arg("--unshare-user")
        .arg("--unshare-pid")
        .arg("--unshare-ipc")
        .arg("--unshare-uts")
        .arg("--new-session")
        .arg("--die-with-parent")
        .arg("--proc")
        .arg("/proc")
        .arg("--dev")
        .arg("/dev")
        .arg("--tmpfs")
        .arg("/tmp");

    for path in runtime_read_paths() {
        bwrap_command.arg("--ro-bind-try").arg(&path).arg(&path);
    }

    // Root-owned SSH client config appears as uid 65534 inside the unprivileged
    // user namespace, which OpenSSH rejects before authentication. Hide the
    // system config and let OpenSSH use the read-only user config/defaults.
    if Path::new("/etc/ssh").is_dir() {
        bwrap_command.arg("--tmpfs").arg("/etc/ssh");
    }

    let ssh_agent_socket = ssh_agent_socket();
    if let Some(socket) = &ssh_agent_socket {
        // Forward only the agent socket. Private key files remain outside the
        // sandbox while Git/SSH can authenticate and perform SSH signing.
        bwrap_command.arg("--bind").arg(socket).arg(socket);
    }

    // Replicate merged-/usr symlinks. runtime_read_paths canonicalises, so on
    // distributions where /bin, /sbin, /lib and /lib64 are symlinks into /usr
    // it yields only the /usr targets. Bubblewrap builds a fresh namespace:
    // without these links /bin/bash does not exist and every sandboxed command
    // fails with "execvp /bin/bash: No such file or directory".
    for link in ["/bin", "/sbin", "/lib", "/lib64"] {
        let link = Path::new(link);
        if let Ok(target) = std::fs::read_link(link) {
            bwrap_command.arg("--symlink").arg(target).arg(link);
        }
    }

    // Git metadata that lives outside the workspace (repo checkouts, worktrees,
    // submodules). Empty for a plain checkout.
    for path in workspace_git_paths(&workspace) {
        bwrap_command.arg("--bind-try").arg(&path).arg(&path);
    }

    for path in [&workspace, &scratch] {
        bwrap_command.arg("--bind").arg(path).arg(path);
    }

    bwrap_command.arg("--chdir").arg(&cwd);
    if let Some(socket) = &ssh_agent_socket {
        bwrap_command
            .arg("--setenv")
            .arg("SSH_AUTH_SOCK")
            .arg(socket);
    }
    bwrap_command
        .arg("--setenv")
        .arg("TMPDIR")
        .arg(&scratch)
        .arg("--setenv")
        .arg("TMP")
        .arg(&scratch)
        .arg("--setenv")
        .arg("TEMP")
        .arg(&scratch)
        .arg("/bin/bash")
        .arg("-c")
        .arg(command);

    Ok(bwrap_command)
}

/// Build the command that runs `command` confined to `workspace`, together with
/// the private scratch directory created for it.
///
/// Confinement is through bubblewrap, which builds a fresh mount namespace
/// containing only the allowlisted paths. When `bwrap` is not on `PATH` the
/// error says so, since the caller cannot run anything unconfined.
///
/// The scratch directory is removed again if the command could not be prepared.
pub fn helper_command(
    command: &str,
    workspace: &Path,
    cwd: &Path,
) -> io::Result<(Command, PathBuf)> {
    let scratch_dir =
        std::env::temp_dir().join(format!("catdesk-sandbox-{}", uuid::Uuid::new_v4()));
    let mut dir_builder = std::fs::DirBuilder::new();
    dir_builder
        .mode(0o700)
        .create(&scratch_dir)
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to create sandbox scratch directory {}: {error}",
                    scratch_dir.display()
                ),
            )
        })?;

    let prepared = match bubblewrap_executable(workspace) {
        Some(bwrap) => bubblewrap_command(&bwrap, command, workspace, cwd, &scratch_dir),
        None => Err(io::Error::other(
            "no usable sandbox: bwrap was not found on PATH outside the workspace. Install \
             bubblewrap to run commands confined.",
        )),
    };

    match prepared {
        Ok(prepared) => Ok((prepared, scratch_dir)),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&scratch_dir);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn nvm_read_paths_include_only_current_version_lib() {
        let tree = TempTree::new();
        let nvm_dir = tree.path().join(".nvm");
        let version_root = nvm_dir.join("versions/node/v26.8.1");
        let bin = version_root.join("bin");
        let lib = version_root.join("lib");
        std::fs::create_dir_all(&bin).expect("create NVM bin");
        std::fs::create_dir_all(&lib).expect("create NVM lib");

        let mut paths = BTreeSet::new();
        insert_nvm_lib_path(&mut paths, &bin, &nvm_dir);

        assert!(paths.contains(&lib.canonicalize().expect("canonical NVM lib")));
        assert!(!paths.contains(&version_root.canonicalize().expect("canonical version root")));
        assert!(!paths.contains(&nvm_dir.canonicalize().expect("canonical NVM dir")));
    }

    #[test]
    fn nvm_read_paths_reject_symlinked_lib_escaping_version_root() {
        use std::os::unix::fs::symlink;

        let tree = TempTree::new();
        let nvm_dir = tree.path().join(".nvm");
        let version_root = nvm_dir.join("versions/node/v26.8.1");
        let bin = version_root.join("bin");
        let outside = tree.path().join("outside");
        std::fs::create_dir_all(&bin).expect("create NVM bin");
        std::fs::create_dir_all(&outside).expect("create outside dir");
        symlink(&outside, version_root.join("lib")).expect("symlink NVM lib");

        let mut paths = BTreeSet::new();
        insert_nvm_lib_path(&mut paths, &bin, &nvm_dir);

        assert!(paths.is_empty());
    }

    #[test]
    fn nvm_read_paths_reject_nonstandard_path_inside_nvm_dir() {
        let tree = TempTree::new();
        let nvm_dir = tree.path().join(".nvm");
        let version_root = nvm_dir.join("foo");
        let bin = version_root.join("bin");
        let lib = version_root.join("lib");
        std::fs::create_dir_all(nvm_dir.join("versions/node")).expect("create NVM versions");
        std::fs::create_dir_all(&bin).expect("create nonstandard bin");
        std::fs::create_dir_all(&lib).expect("create nonstandard lib");

        let mut paths = BTreeSet::new();
        insert_nvm_lib_path(&mut paths, &bin, &nvm_dir);

        assert!(paths.is_empty());
    }

    #[test]
    fn nvm_read_paths_reject_bin_outside_nvm_dir() {
        let tree = TempTree::new();
        let nvm_dir = tree.path().join(".nvm");
        let outside_root = tree.path().join("outside/node");
        let bin = outside_root.join("bin");
        let lib = outside_root.join("lib");
        std::fs::create_dir_all(&nvm_dir).expect("create NVM dir");
        std::fs::create_dir_all(&bin).expect("create outside bin");
        std::fs::create_dir_all(&lib).expect("create outside lib");

        let mut paths = BTreeSet::new();
        insert_nvm_lib_path(&mut paths, &bin, &nvm_dir);

        assert!(paths.is_empty());
    }

    #[test]
    fn runtime_read_paths_include_resolv_conf_target() {
        let resolv_conf = Path::new("/etc/resolv.conf")
            .canonicalize()
            .expect("canonical /etc/resolv.conf");
        assert!(runtime_read_paths().contains(&resolv_conf));
    }

    #[test]
    fn runtime_read_paths_include_ssh_known_hosts_target() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let known_hosts = PathBuf::from(home).join(".ssh/known_hosts");
        let Ok(known_hosts) = known_hosts.canonicalize() else {
            return;
        };
        assert!(runtime_read_paths().contains(&known_hosts));
    }

    #[test]
    fn ssh_read_paths_include_config_known_hosts_and_public_keys_only() {
        let tree = TempTree::new();
        let ssh_dir = tree.path().join(".ssh");
        std::fs::create_dir_all(&ssh_dir).expect("create .ssh");
        let config = ssh_dir.join("config");
        let known_hosts = ssh_dir.join("known_hosts");
        let public_key = ssh_dir.join("id_ed25519.pub");
        let private_key = ssh_dir.join("id_ed25519");
        for path in [&config, &known_hosts, &public_key, &private_key] {
            std::fs::write(path, b"test\n").expect("write ssh fixture");
        }

        let mut paths = BTreeSet::new();
        insert_ssh_read_paths(&mut paths, tree.path());

        assert!(paths.contains(&config.canonicalize().expect("canonical config")));
        assert!(paths.contains(&known_hosts.canonicalize().expect("canonical known_hosts")));
        assert!(paths.contains(&public_key.canonicalize().expect("canonical public key")));
        assert!(!paths.contains(&private_key.canonicalize().expect("canonical private key")));
    }

    #[test]
    fn existing_unix_socket_accepts_socket_and_rejects_regular_file() {
        use std::os::unix::net::UnixListener;

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let socket = PathBuf::from(format!("/tmp/cd-{}.sock", &suffix[..8]));
        let regular = PathBuf::from(format!("/tmp/cd-{}.file", &suffix[..8]));
        let _listener = UnixListener::bind(&socket).expect("bind unix socket");
        std::fs::write(&regular, b"not a socket").expect("write regular file");

        assert_eq!(
            existing_unix_socket(&socket),
            Some(socket.canonicalize().expect("canonical socket"))
        );
        assert_eq!(existing_unix_socket(&regular), None);

        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_file(&regular);
    }

    #[test]
    fn runtime_read_paths_do_not_grant_the_home_directory_itself() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let home = PathBuf::from(home).canonicalize().expect("canonical HOME");
        assert!(!runtime_read_paths().contains(&home));
    }

    #[test]
    fn helper_command_creates_private_scratch_directory() {
        use std::os::unix::fs::PermissionsExt;

        // bwrap may not be installed in every environment. helper_command
        // reports that rather than returning a command, so there is nothing to
        // assert about the scratch directory here.
        if bubblewrap_executable(Path::new(".")).is_none() {
            return;
        }

        let (_command, scratch) = helper_command("true", Path::new("."), Path::new("."))
            .expect("prepare sandbox helper command");
        let mode = std::fs::metadata(&scratch)
            .expect("scratch metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        std::fs::remove_dir_all(scratch).expect("remove scratch directory");
    }

    #[test]
    fn bubblewrap_command_chdirs_to_cwd_and_uses_new_session() {
        let tree = TempTree::new();
        let workspace = tree.path().join("workspace");
        let cwd = workspace.join("src");
        let scratch = tree.path().join("scratch");
        std::fs::create_dir_all(&cwd).expect("create cwd");
        std::fs::create_dir_all(&scratch).expect("create scratch");

        let command = bubblewrap_command(
            Path::new("/usr/bin/bwrap"),
            "pwd",
            &workspace,
            &cwd,
            &scratch,
        )
        .expect("build bubblewrap command");
        let args: Vec<_> = command.get_args().map(|arg| arg.to_os_string()).collect();

        assert!(args.iter().any(|arg| arg.as_os_str() == "--new-session"));
        if Path::new("/etc/ssh").is_dir() {
            assert!(args.windows(2).any(|pair| {
                pair[0].as_os_str() == OsStr::new("--tmpfs")
                    && pair[1].as_os_str() == OsStr::new("/etc/ssh")
            }));
        }
        let chdir = args
            .windows(2)
            .find(|pair| pair[0].as_os_str() == OsStr::new("--chdir"))
            .map(|pair| PathBuf::from(pair[1].clone()))
            .expect("--chdir argument");
        assert_eq!(chdir, cwd.canonicalize().expect("canonical cwd"));
    }

    #[test]
    fn bubblewrap_executable_skips_workspace_symlink_and_uses_later_candidate() {
        use std::os::unix::fs::PermissionsExt;

        let tree = TempTree::new();
        let workspace = tree.path().join("workspace");
        let workspace_bin = workspace.join("bin");
        std::fs::create_dir_all(&workspace_bin).expect("create workspace bin");
        let hijacked = workspace_bin.join("bwrap");
        std::fs::write(&hijacked, b"#!/bin/sh\n").expect("write workspace bwrap");
        std::fs::set_permissions(&hijacked, std::fs::Permissions::from_mode(0o755))
            .expect("chmod workspace bwrap");

        let symlink_bin = tree.path().join("symlink-bin");
        std::fs::create_dir_all(&symlink_bin).expect("create symlink bin");
        std::os::unix::fs::symlink(&hijacked, symlink_bin.join("bwrap")).expect("symlink bwrap");

        let real_bin = tree.path().join("real-bin");
        std::fs::create_dir_all(&real_bin).expect("create real bin");
        let real = real_bin.join("bwrap");
        std::fs::write(&real, b"#!/bin/sh\n").expect("write real bwrap");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755))
            .expect("chmod real bwrap");

        assert_eq!(
            bubblewrap_executable_in_paths(vec![symlink_bin, real_bin], &workspace),
            Some(real.canonicalize().expect("canonical real bwrap"))
        );
    }

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("catdesk-sandbox-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("create temp tree");
            Self(dir.canonicalize().expect("canonical temp tree"))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn workspace_git_paths_empty_for_plain_checkout() {
        let tree = TempTree::new();
        let workspace = tree.path().join("repo");
        std::fs::create_dir_all(workspace.join(".git")).expect("create .git dir");
        assert!(workspace_git_paths(&workspace).is_empty());
    }

    #[test]
    fn workspace_git_paths_accepts_submodule_name_that_differs_from_path() {
        if !Command::new("git")
            .arg("--version")
            .status()
            .is_ok_and(|status| status.success())
        {
            return;
        }

        // super/                <- ancestor holding the real .git
        //   .git/modules/libfoo/
        //   vendor/foo/.git     <- file: "gitdir: ../../.git/modules/libfoo"
        let tree = TempTree::new();
        let module_dir = tree.path().join("super/.git/modules/libfoo");
        std::fs::create_dir_all(&module_dir).expect("create module dir");
        std::fs::write(
            module_dir.join("config"),
            "[core]\n\tworktree = ../../../vendor/foo\n",
        )
        .expect("write module config");
        std::fs::write(
            tree.path().join("super/.gitmodules"),
            "[submodule \"libfoo\"]\n\tpath = vendor/foo\n",
        )
        .expect("write .gitmodules");
        let workspace = tree.path().join("super/vendor/foo");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::write(
            workspace.join(".git"),
            "gitdir: ../../.git/modules/libfoo\n",
        )
        .expect("write .git file");

        let resolved = workspace_git_paths(&workspace);
        assert!(resolved.contains(&module_dir.canonicalize().expect("canonical module dir")));
    }

    #[test]
    fn workspace_git_paths_rejects_gitdir_pointing_at_parent_git() {
        let tree = TempTree::new();
        std::fs::create_dir_all(tree.path().join("super/.git")).expect("create parent .git");
        let workspace = tree.path().join("super/work");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::write(workspace.join(".git"), "gitdir: ../.git\n").expect("write .git file");

        assert!(workspace_git_paths(&workspace).is_empty());
    }

    #[test]
    fn workspace_git_paths_rejects_sibling_submodule() {
        let tree = TempTree::new();
        let module_dir = tree.path().join("super/.git/modules/libbar");
        std::fs::create_dir_all(&module_dir).expect("create module dir");
        std::fs::write(
            module_dir.join("config"),
            "[core]\n\tworktree = ../../../vendor/bar\n",
        )
        .expect("write module config");
        std::fs::write(
            tree.path().join("super/.gitmodules"),
            "[submodule \"libbar\"]\n\tpath = vendor/bar\n",
        )
        .expect("write .gitmodules");
        let workspace = tree.path().join("super/vendor/foo");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::write(
            workspace.join(".git"),
            "gitdir: ../../.git/modules/libbar\n",
        )
        .expect("write .git file");

        assert!(workspace_git_paths(&workspace).is_empty());
    }

    #[test]
    fn workspace_git_paths_accepts_a_sibling_linked_worktree() {
        if !Command::new("git")
            .arg("--version")
            .status()
            .is_ok_and(|status| status.success())
        {
            return;
        }

        let tree = TempTree::new();
        let main = tree.path().join("main");
        let worktree = tree.path().join("linked");
        std::fs::create_dir_all(&main).expect("create main repo");

        run_git(&main, &["init"]);
        std::fs::write(main.join("file"), b"content").expect("write file");
        run_git(&main, &["add", "file"]);
        run_git(
            &main,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "init",
            ],
        );
        run_git(&main, &["worktree", "add", "../linked"]);

        let common = main.join(".git").canonicalize().expect("canonical common");
        let resolved = workspace_git_paths(&worktree);
        assert!(resolved.contains(&common));
        assert!(
            resolved
                .iter()
                .any(|path| path.starts_with(common.join("worktrees")))
        );
    }

    #[test]
    fn workspace_git_paths_accepts_a_nested_linked_worktree() {
        if !Command::new("git")
            .arg("--version")
            .status()
            .is_ok_and(|status| status.success())
        {
            return;
        }

        let tree = TempTree::new();
        let main = tree.path().join("main");
        let worktree = main.join("nested");
        std::fs::create_dir_all(&main).expect("create main repo");

        run_git(&main, &["init"]);
        std::fs::write(main.join("file"), b"content").expect("write file");
        run_git(&main, &["add", "file"]);
        run_git(
            &main,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "init",
            ],
        );
        run_git(&main, &["worktree", "add", "nested"]);

        let common = main.join(".git").canonicalize().expect("canonical common");
        let resolved = workspace_git_paths(&worktree);
        assert!(resolved.contains(&common));
        assert!(
            resolved
                .iter()
                .any(|path| path.starts_with(common.join("worktrees")))
        );
    }

    #[test]
    fn workspace_git_paths_rejects_a_gitdir_pointing_at_an_external_canary() {
        // The workspace names a directory that no ancestor .git/.repo covers.
        // It must be excluded so the sandbox never binds it writable.
        let tree = TempTree::new();
        let canary = tree.path().join("canary");
        std::fs::create_dir_all(&canary).expect("create canary");
        std::fs::write(canary.join("secret"), b"do not touch").expect("write canary file");

        let workspace = tree.path().join("super/work");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let canary_abs = canary.to_string_lossy().into_owned();
        std::fs::write(workspace.join(".git"), format!("gitdir: {canary_abs}\n"))
            .expect("write .git file");

        let resolved = workspace_git_paths(&workspace);
        assert!(
            resolved.is_empty(),
            "expected no paths, canary leaked: {resolved:?}"
        );
        assert!(!resolved.iter().any(|p| p.starts_with(&canary)));
    }

    #[test]
    fn workspace_git_paths_rejects_a_fake_worktree_canary() {
        let tree = TempTree::new();
        let canary = tree.path().join("canary/.git");
        let fake_worktree = canary.join("worktrees/x");
        std::fs::create_dir_all(&canary).expect("create canary");
        std::fs::create_dir_all(&fake_worktree).expect("create fake worktree");

        let workspace = tree.path().join("super/work");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\n", fake_worktree.display()),
        )
        .expect("write .git file");
        std::fs::write(
            fake_worktree.join("gitdir"),
            workspace.join(".git").to_string_lossy().as_bytes(),
        )
        .expect("write fake backpointer");
        std::fs::write(fake_worktree.join("commondir"), "../..\n").expect("write fake commondir");

        assert!(workspace_git_paths(&workspace).is_empty());
    }

    #[test]
    fn workspace_git_paths_rejects_a_symlinked_gitdir_escaping_trusted_roots() {
        let tree = TempTree::new();
        let canary = tree.path().join("canary");
        std::fs::create_dir_all(&canary).expect("create canary");

        let workspace = tree.path().join("super/work");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::os::unix::fs::symlink(&canary, workspace.join(".git")).expect("symlink .git");

        assert!(workspace_git_paths(&workspace).is_empty());
    }

    #[test]
    fn workspace_git_paths_accepts_repo_project_and_shared_objects() {
        let tree = TempTree::new();
        let workspace = tree.path().join("client/a/b");
        let project_git = tree.path().join("client/.repo/projects/a/b.git");
        let shared_objects = tree
            .path()
            .join("client/.repo/project-objects/platform/b.git/objects");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::create_dir_all(&project_git).expect("create project git dir");
        std::fs::create_dir_all(&shared_objects).expect("create shared objects");
        std::os::unix::fs::symlink(
            "../../../project-objects/platform/b.git/objects",
            project_git.join("objects"),
        )
        .expect("symlink shared objects");
        std::os::unix::fs::symlink("../../.repo/projects/a/b.git", workspace.join(".git"))
            .expect("symlink .git");

        let resolved = workspace_git_paths(&workspace);
        assert!(resolved.contains(&project_git.canonicalize().expect("canonical project git")));
        assert!(resolved.contains(&shared_objects.canonicalize().expect("canonical objects")));
    }

    #[test]
    fn workspace_git_paths_rejects_symlinked_repo_root() {
        let tree = TempTree::new();
        let real_repo = tree.path().join("real-repo");
        let workspace = tree.path().join("client/a/b");
        std::fs::create_dir_all(real_repo.join("projects/a/b.git")).expect("create real repo");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::os::unix::fs::symlink(&real_repo, tree.path().join("client/.repo"))
            .expect("symlink .repo");
        std::os::unix::fs::symlink("../../.repo/projects/a/b.git", workspace.join(".git"))
            .expect("symlink .git");

        assert!(workspace_git_paths(&workspace).is_empty());
    }

    #[test]
    fn workspace_git_paths_rejects_symlinked_repo_project() {
        let tree = TempTree::new();
        let workspace = tree.path().join("client/a/b");
        let canary = tree.path().join("canary.git");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::create_dir_all(&canary).expect("create canary");
        std::fs::create_dir_all(tree.path().join("client/.repo/projects/a"))
            .expect("create projects dir");
        std::os::unix::fs::symlink(&canary, tree.path().join("client/.repo/projects/a/b.git"))
            .expect("symlink project git");
        std::os::unix::fs::symlink("../../.repo/projects/a/b.git", workspace.join(".git"))
            .expect("symlink .git");

        assert!(workspace_git_paths(&workspace).is_empty());
    }

    #[test]
    fn workspace_git_paths_rejects_repo_symlink_entries_outside_project_objects() {
        let tree = TempTree::new();
        let workspace = tree.path().join("client/a/b");
        let project_git = tree.path().join("client/.repo/projects/a/b.git");
        let canary = tree.path().join("canary/objects");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::create_dir_all(&project_git).expect("create project git dir");
        std::fs::create_dir_all(&canary).expect("create canary objects");
        std::fs::create_dir_all(tree.path().join("client/.repo/project-objects"))
            .expect("create project-objects");
        std::os::unix::fs::symlink(&canary, project_git.join("objects"))
            .expect("symlink canary objects");
        std::os::unix::fs::symlink("../../.repo/projects/a/b.git", workspace.join(".git"))
            .expect("symlink .git");

        let resolved = workspace_git_paths(&workspace);
        assert!(resolved.contains(&project_git.canonicalize().expect("canonical project git")));
        assert!(!resolved.iter().any(|path| path.starts_with(&canary)));
    }

    #[test]
    fn workspace_git_paths_rejects_worktree_with_symlinked_commondir() {
        let tree = TempTree::new();
        let workspace = tree.path().join("linked");
        let git_dir = tree.path().join("main/.git/worktrees/linked");
        let canary = tree.path().join("canary.git");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::create_dir_all(&git_dir).expect("create git dir");
        std::fs::create_dir_all(&canary).expect("create canary");
        std::fs::write(
            workspace.join(".git"),
            "gitdir: ../main/.git/worktrees/linked\n",
        )
        .expect("write .git file");
        std::fs::write(
            git_dir.join("gitdir"),
            workspace.join(".git").to_string_lossy().as_bytes(),
        )
        .expect("write backpointer");
        std::os::unix::fs::symlink(&canary, tree.path().join("main/common.git"))
            .expect("symlink common");
        std::fs::write(git_dir.join("commondir"), "../../../common.git\n")
            .expect("write commondir");
        assert_eq!(
            git_dir
                .join("../../../common.git")
                .canonicalize()
                .expect("canonical commondir target"),
            canary.canonicalize().expect("canonical canary")
        );

        assert!(workspace_git_paths(&workspace).is_empty());
    }

    #[test]
    fn workspace_git_paths_rejects_worktree_through_symlinked_common_git() {
        let tree = TempTree::new();
        let workspace = tree.path().join("linked");
        let real_common = tree.path().join("evil/.git");
        let git_dir = real_common.join("worktrees/linked");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::create_dir_all(&git_dir).expect("create git dir");
        std::fs::create_dir_all(tree.path().join("main")).expect("create main");
        std::os::unix::fs::symlink(&real_common, tree.path().join("main/.git"))
            .expect("symlink common .git");
        std::fs::write(
            workspace.join(".git"),
            "gitdir: ../main/.git/worktrees/linked\n",
        )
        .expect("write .git file");
        std::fs::write(
            git_dir.join("gitdir"),
            workspace.join(".git").to_string_lossy().as_bytes(),
        )
        .expect("write backpointer");
        std::fs::write(git_dir.join("commondir"), "../..\n").expect("write commondir");

        assert!(workspace_git_paths(&workspace).is_empty());
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed with {status}");
    }
}
