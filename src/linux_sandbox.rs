use std::collections::BTreeSet;
use std::io;
use std::os::unix::fs::DirBuilderExt;
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

fn runtime_read_paths() -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();

    for path in ["/bin", "/sbin", "/usr", "/lib", "/lib64", "/etc", "/sys"] {
        insert_existing(&mut paths, path);
    }

    insert_existing(&mut paths, "/etc/resolv.conf");

    // Executables installed outside the standard system prefixes must remain
    // executable when their directory is explicitly present in PATH.
    insert_env_path_list(&mut paths, "PATH");

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
        insert_existing(&mut paths, home.join(".ssh/known_hosts"));
    }

    paths
}

/// Directories at or above `workspace` that may legitimately hold its git
/// metadata: an ancestor's own `.git` (worktrees and submodules point their
/// `gitdir:` there) or `.repo` (a `repo` client keeps every checkout's git
/// directory and the shared object store under it).
///
/// A `.git` pointer is workspace-controlled input, so whatever it resolves to
/// is confined to these roots before being granted. Without that, a crafted
/// checkout could name `$HOME`, `/etc`, or an unrelated checkout and have the
/// sandbox bind it writable.
fn trusted_git_metadata_roots(workspace: &Path) -> BTreeSet<PathBuf> {
    let mut roots = BTreeSet::new();
    // Strict ancestors only: the workspace's own `.git` is the pointer being
    // validated, so canonicalising it here would let a `.git` symlink nominate
    // its own target as trusted.
    for ancestor in workspace.ancestors().skip(1) {
        insert_existing(&mut roots, ancestor.join(".git"));
        insert_existing(&mut roots, ancestor.join(".repo"));
    }
    roots
}

/// Whether `path` (already canonical) sits at or beneath one of `roots`.
fn within_trusted_root(path: &Path, roots: &BTreeSet<PathBuf>) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Canonicalise `path` and insert it only when it lands inside a trusted root.
fn insert_trusted(paths: &mut BTreeSet<PathBuf>, path: &Path, roots: &BTreeSet<PathBuf>) {
    if let Ok(canonical) = path.canonicalize()
        && within_trusted_root(&canonical, roots)
    {
        paths.insert(canonical);
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
///     `gitdir:` line, and that directory's `commondir` points at the main one.
///
/// Without these, every git command inside the sandbox fails with "not a git
/// repository", because the target is simply absent. They are writable rather
/// than read-only for parity with a plain checkout, where `.git` sits in the
/// writable workspace and commands like `git commit` work.
///
/// Every resolved path is checked against [`trusted_git_metadata_roots`]; a
/// pointer that escapes them is ignored entirely, so this can only ever widen
/// the sandbox to an ancestor's `.git`/`.repo`, never to an arbitrary directory
/// the checkout names.
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

    let roots = trusted_git_metadata_roots(workspace);

    let git_dir = if metadata.is_file() {
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
        match workspace.join(target).canonicalize() {
            Ok(path) => path,
            Err(_) => return paths,
        }
    } else {
        match dot_git.canonicalize() {
            Ok(path) => path,
            Err(_) => return paths,
        }
    };

    // The git directory itself must resolve inside a trusted root; if it does
    // not, the checkout is pointing somewhere it has no business pointing and
    // nothing further is trusted either.
    if !within_trusted_root(&git_dir, &roots) {
        return paths;
    }
    paths.insert(git_dir.clone());

    // Entries inside the git directory may point outside it again -- `repo`
    // shares objects between checkouts this way -- so each is re-checked.
    if let Ok(entries) = std::fs::read_dir(&git_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_symlink()) {
                insert_trusted(&mut paths, &path, &roots);
            }
        }
    }

    // Worktrees keep shared state in the directory named by `commondir`.
    if let Ok(common) = std::fs::read_to_string(git_dir.join("commondir")) {
        insert_trusted(&mut paths, &git_dir.join(common.trim()), &roots);
    }

    paths
}

/// Locate an executable `bwrap` on PATH. Bubblewrap confines through mount
/// namespaces rather than an LSM, so it works on kernels far older than
/// Landlock's 5.13 baseline -- RHEL 8 / Rocky 8 (4.18), Ubuntu 20.04 (5.4) and
/// Debian 11 (5.10) included.
///
/// A non-executable file named `bwrap` earlier in PATH must not shadow a real
/// one later, so the execute bit is checked rather than just the file type.
fn bubblewrap_executable() -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("bwrap"))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

/// Build a bubblewrap invocation that confines `command` to `workspace` plus its
/// private `scratch` directory.
///
/// The namespace contains only what [`runtime_read_paths`] returns plus the
/// workspace, scratch and any external git metadata directories, so an unbound
/// path is simply absent rather than merely denied. `--dev /dev` supplies a
/// minimal set of device nodes (`/dev/null`, `/dev/zero`, `/dev/random`,
/// `/dev/tty` and the like), `--tmpfs /tmp` keeps the host's `/tmp` out of
/// reach, and `--unshare-pid` hides host processes.
///
/// `--new-session` is deliberately omitted: it detaches the controlling
/// terminal, which would make `/dev/tty` unusable.
fn bubblewrap_command(
    bwrap: &Path,
    command: &str,
    workspace: &Path,
    scratch: &Path,
) -> io::Result<Command> {
    let workspace = canonical_existing(workspace)?;
    let scratch = canonical_existing(scratch)?;

    let mut bwrap_command = Command::new(bwrap);
    bwrap_command
        .arg("--unshare-user")
        .arg("--unshare-pid")
        .arg("--unshare-ipc")
        .arg("--unshare-uts")
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

    bwrap_command
        .arg("--chdir")
        .arg(&workspace)
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
pub fn helper_command(command: &str, workspace: &Path) -> io::Result<(Command, PathBuf)> {
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

    let prepared = match bubblewrap_executable() {
        Some(bwrap) => bubblewrap_command(&bwrap, command, workspace, &scratch_dir),
        None => Err(io::Error::other(
            "no usable sandbox: bwrap was not found on PATH. Install bubblewrap to run \
             commands confined.",
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
        if bubblewrap_executable().is_none() {
            return;
        }

        let (_command, scratch) =
            helper_command("true", Path::new(".")).expect("prepare sandbox helper command");
        let mode = std::fs::metadata(&scratch)
            .expect("scratch metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        std::fs::remove_dir_all(scratch).expect("remove scratch directory");
    }

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("catdesk-gitpaths-{}", uuid::Uuid::new_v4()));
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
    fn workspace_git_paths_follows_gitdir_into_an_ancestor() {
        // super/                <- ancestor holding the real .git
        //   .git/modules/sub/
        //   sub/.git            <- file: "gitdir: ../.git/modules/sub"
        let tree = TempTree::new();
        let module_dir = tree.path().join("super/.git/modules/sub");
        std::fs::create_dir_all(&module_dir).expect("create module dir");
        let workspace = tree.path().join("super/sub");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::write(workspace.join(".git"), "gitdir: ../.git/modules/sub\n")
            .expect("write .git file");

        let resolved = workspace_git_paths(&workspace);
        assert!(resolved.contains(&module_dir.canonicalize().expect("canonical module dir")));
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
    fn workspace_git_paths_rejects_a_symlinked_gitdir_escaping_trusted_roots() {
        let tree = TempTree::new();
        let canary = tree.path().join("canary");
        std::fs::create_dir_all(&canary).expect("create canary");

        let workspace = tree.path().join("super/work");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::os::unix::fs::symlink(&canary, workspace.join(".git")).expect("symlink .git");

        assert!(workspace_git_paths(&workspace).is_empty());
    }
}
