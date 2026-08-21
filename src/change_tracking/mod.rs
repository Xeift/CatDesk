mod diff;
mod ignore;
mod snapshot;

use std::path::{Path, PathBuf};

pub(crate) use diff::FileChange;

const MAX_DIFF_FILES: usize = 16;
const MAX_DIFF_CHARS_PER_FILE: usize = 12_000;
const MAX_WATCHED_ENTRIES: usize = 512;
const MAX_FILE_CAPTURE_BYTES: usize = 128 * 1024;
const MAX_TEXT_CAPTURE_LINES: usize = 420;

#[derive(Clone, Debug)]
pub(crate) struct ChangeTarget {
    path: PathBuf,
    recursive: bool,
    respect_project_ignores: bool,
}

impl ChangeTarget {
    pub(crate) fn explicit(path: PathBuf, recursive: bool) -> Self {
        Self {
            path,
            recursive,
            respect_project_ignores: false,
        }
    }

    pub(crate) fn discovered(path: PathBuf, recursive: bool) -> Self {
        Self {
            path,
            recursive,
            respect_project_ignores: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ChangeScope {
    targets: Vec<ChangeTarget>,
}

impl ChangeScope {
    pub(crate) fn none() -> Self {
        Self::default()
    }

    pub(crate) fn single(target: ChangeTarget) -> Self {
        Self {
            targets: vec![target],
        }
    }

    pub(crate) fn many(targets: Vec<ChangeTarget>) -> Self {
        Self { targets }
    }

    fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ChangeSession {
    workspace_root: PathBuf,
    scope: ChangeScope,
    before: snapshot::WorkspaceSnapshot,
}

impl ChangeSession {
    pub(crate) fn begin(workspace_root: &Path, scope: ChangeScope) -> Self {
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        let before = snapshot::collect_snapshot(&workspace_root, &scope.targets);
        Self {
            workspace_root,
            scope,
            before,
        }
    }

    pub(crate) fn changes(&self) -> Vec<FileChange> {
        if self.scope.is_empty() {
            return Vec::new();
        }
        let after = snapshot::collect_snapshot(&self.workspace_root, &self.scope.targets);
        diff::diff_snapshots(&self.before, &after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use uuid::Uuid;

    fn workspace(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("catdesk-change-tracking-{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create workspace");
        root
    }

    #[test]
    fn recursive_discovery_hard_excludes_vcs_admin_paths_only() {
        let root = workspace("vcs-exclude");
        fs::create_dir_all(root.join(".git/logs")).expect("create git internals");
        fs::create_dir_all(root.join(".github/workflows")).expect("create github config");
        fs::write(root.join(".gitignore"), "target/\n").expect("write gitignore");
        fs::write(root.join(".git/index"), "before").expect("write git index");
        fs::write(root.join(".git/logs/HEAD"), "before").expect("write git log");
        fs::write(root.join(".github/workflows/ci.yml"), "before\n").expect("write workflow");

        let session = ChangeSession::begin(
            &root,
            ChangeScope::single(ChangeTarget::discovered(root.clone(), true)),
        );
        fs::write(root.join(".git/index"), "after").expect("change git index");
        fs::write(root.join(".git/logs/HEAD"), "after").expect("change git log");
        fs::write(root.join(".gitignore"), "target/\ndist/\n").expect("change gitignore");
        fs::write(root.join(".github/workflows/ci.yml"), "after\n").expect("change workflow");

        let changes = session.changes();
        let paths = changes
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>();
        assert!(paths.contains(&".gitignore"));
        assert!(paths.contains(&".github/workflows/ci.yml"));
        assert!(paths.iter().all(|path| !path.starts_with(".git/")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recursive_discovery_respects_project_ignore_files() {
        let root = workspace("project-ignore");
        fs::create_dir_all(root.join("target")).expect("create target");
        fs::write(root.join(".gitignore"), "target/\n").expect("write gitignore");
        fs::write(root.join("tracked.txt"), "before\n").expect("write tracked");
        fs::write(root.join("target/generated.txt"), "before\n").expect("write generated");

        let session = ChangeSession::begin(
            &root,
            ChangeScope::single(ChangeTarget::discovered(root.clone(), true)),
        );
        fs::write(root.join("tracked.txt"), "after\n").expect("change tracked");
        fs::write(root.join("target/generated.txt"), "after\n").expect("change generated");

        let changes = session.changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "tracked.txt");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_targets_override_project_ignore_files() {
        let root = workspace("explicit-ignore-override");
        fs::create_dir_all(root.join("target")).expect("create target");
        fs::write(root.join(".gitignore"), "target/\n").expect("write gitignore");
        fs::write(root.join("target/result.txt"), "before\n").expect("write target file");

        let session = ChangeSession::begin(
            &root,
            ChangeScope::single(ChangeTarget::explicit(
                root.join("target/result.txt"),
                false,
            )),
        );
        fs::write(root.join("target/result.txt"), "after\n").expect("change target file");

        let changes = session.changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "target/result.txt");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_vcs_admin_targets_stay_hard_excluded() {
        let root = workspace("explicit-vcs-exclude");
        fs::create_dir_all(root.join(".git")).expect("create git dir");
        fs::write(root.join(".git/HEAD"), "before\n").expect("write git head");

        let session = ChangeSession::begin(
            &root,
            ChangeScope::single(ChangeTarget::explicit(root.join(".git/HEAD"), false)),
        );
        fs::write(root.join(".git/HEAD"), "after\n").expect("change git head");

        assert!(session.changes().is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
