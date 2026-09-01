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
/// workspace and scratch directories, so an unbound path is simply absent
/// rather than merely denied. `--dev /dev` supplies a minimal set of device
/// nodes (`/dev/null`, `/dev/zero`, `/dev/random`, `/dev/tty` and the like),
/// `--tmpfs /tmp` keeps the host's `/tmp` out of reach, and `--unshare-pid`
/// hides host processes.
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
}
