use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use landlock::{
    ABI, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus, path_beneath_rules,
};

pub const HELPER_ARG: &str = "__catdesk_landlock_exec";

// ABI v3 is the minimum safe baseline for filesystem confinement because it
// adds control over truncate(2). Older ABIs could otherwise leave an outside
// file truncatable even when ordinary write opens are denied.
const LANDLOCK_ABI: ABI = ABI::V3;

pub fn is_helper_invocation() -> bool {
    std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == OsStr::new(HELPER_ARG))
}

pub fn exec_helper() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os();
    let _program = args.next();
    let helper = args
        .next()
        .ok_or_else(|| io::Error::other("missing Landlock helper marker"))?;
    if helper != OsStr::new(HELPER_ARG) {
        return Err(io::Error::other("invalid Landlock helper invocation").into());
    }

    let workspace = PathBuf::from(
        args.next()
            .ok_or_else(|| io::Error::other("missing Landlock workspace path"))?,
    );
    let scratch = PathBuf::from(
        args.next()
            .ok_or_else(|| io::Error::other("missing Landlock scratch path"))?,
    );
    let command = args
        .next()
        .ok_or_else(|| io::Error::other("missing Landlock shell command"))?;
    if args.next().is_some() {
        return Err(io::Error::other("unexpected Landlock helper arguments").into());
    }

    apply_workspace_landlock(&workspace, &scratch)?;

    let error = Command::new("/bin/bash")
        .arg("-c")
        .arg(command)
        .env("TMPDIR", &scratch)
        .env("TMP", &scratch)
        .env("TEMP", &scratch)
        .exec();
    Err(io::Error::new(
        error.kind(),
        format!("failed to exec /bin/bash after applying Landlock: {error}"),
    )
    .into())
}

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
    }

    paths
}

fn runtime_write_paths() -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();

    for path in [
        "/dev/null",
        "/dev/zero",
        "/dev/full",
        "/dev/random",
        "/dev/urandom",
        "/dev/tty",
    ] {
        insert_existing(&mut paths, path);
    }

    paths
}

pub fn apply_workspace_landlock(
    workspace: &Path,
    scratch: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = canonical_existing(workspace)?;
    if !workspace.is_dir() {
        return Err(io::Error::other(format!(
            "Landlock workspace is not a directory: {}",
            workspace.display()
        ))
        .into());
    }
    let scratch = canonical_existing(scratch)?;
    if !scratch.is_dir() {
        return Err(io::Error::other(format!(
            "Landlock scratch path is not a directory: {}",
            scratch.display()
        ))
        .into());
    }

    let access_all = AccessFs::from_all(LANDLOCK_ABI);
    let access_read = AccessFs::from_read(LANDLOCK_ABI);

    let read_paths = runtime_read_paths();
    let mut write_paths = runtime_write_paths();
    write_paths.insert(workspace);
    write_paths.insert(scratch);

    let ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(access_all)?
        .create()?
        .add_rules(path_beneath_rules(&read_paths, access_read))?
        .add_rules(path_beneath_rules(&write_paths, access_all))?
        .no_new_privs(true)
        .restrict_self()?;

    if ruleset.ruleset != RulesetStatus::FullyEnforced {
        return Err(io::Error::other(format!(
            "Landlock sandbox was not fully enforced: {:?}",
            ruleset.ruleset
        ))
        .into());
    }

    Ok(())
}

pub fn helper_command(command: &str, workspace: &Path) -> io::Result<(Command, PathBuf)> {
    let executable = std::env::current_exe().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to locate CatDesk executable for Landlock helper: {error}"),
        )
    })?;

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
                    "failed to create Landlock scratch directory {}: {error}",
                    scratch_dir.display()
                ),
            )
        })?;

    let mut helper = Command::new(executable);
    helper
        .arg(HELPER_ARG)
        .arg(workspace)
        .arg(&scratch_dir)
        .arg(command);
    Ok((helper, scratch_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_read_paths_do_not_grant_the_home_directory_itself() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let home = PathBuf::from(home).canonicalize().expect("canonical HOME");
        assert!(!runtime_read_paths().contains(&home));
    }

    #[test]
    fn helper_marker_detection_is_exact() {
        assert_ne!(HELPER_ARG, "");
        assert!(!HELPER_ARG.contains(char::is_whitespace));
    }

    #[test]
    fn helper_command_creates_private_scratch_directory() {
        use std::os::unix::fs::PermissionsExt;

        let (_command, scratch) =
            helper_command("true", Path::new(".")).expect("prepare Landlock helper command");
        let mode = std::fs::metadata(&scratch)
            .expect("scratch metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        std::fs::remove_dir_all(scratch).expect("remove scratch directory");
    }

    #[test]
    fn runtime_write_paths_do_not_grant_global_tmp() {
        let tmp = Path::new("/tmp").canonicalize().expect("canonical /tmp");
        assert!(!runtime_write_paths().contains(&tmp));
    }
}
