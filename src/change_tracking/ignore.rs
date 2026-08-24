use std::ffi::OsStr;
use std::path::{Component, Path};

const VCS_ADMIN_COMPONENTS: &[&str] = &[".git", ".hg", ".svn", ".jj"];

const DEFAULT_IGNORED_DIR_NAMES: &[&str] = &[
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".nox",
    "htmlcov",
    "coverage",
    ".gradle",
];

const DEFAULT_IGNORED_FILE_NAMES: &[&str] = &[
    ".env",
    ".env.local",
    ".env.development",
    ".env.development.local",
    ".env.test",
    ".env.test.local",
    ".env.production",
    ".env.production.local",
    ".env.staging",
    ".env.staging.local",
    ".coverage",
    ".DS_Store",
    "Thumbs.db",
];

pub(super) fn has_project_ignore_file(workspace_root: &Path) -> bool {
    workspace_root.join(".gitignore").is_file() || workspace_root.join(".ignore").is_file()
}

pub(super) fn is_default_ignored_path(path: &Path, is_directory: bool) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    let blocked_names = if is_directory {
        DEFAULT_IGNORED_DIR_NAMES
    } else {
        DEFAULT_IGNORED_FILE_NAMES
    };
    blocked_names
        .iter()
        .any(|blocked| name == OsStr::new(blocked))
}

pub(super) fn is_vcs_admin_path(workspace_root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(workspace_root).unwrap_or(path);
    relative.components().any(|component| match component {
        Component::Normal(value) => VCS_ADMIN_COMPONENTS
            .iter()
            .any(|blocked| value == OsStr::new(blocked)),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_exclusion_matches_components_without_eating_git_prefixed_files() {
        let root = Path::new("/repo");
        assert!(is_vcs_admin_path(root, Path::new("/repo/.git/index")));
        assert!(is_vcs_admin_path(root, Path::new("/repo/nested/.git/HEAD")));
        assert!(is_vcs_admin_path(root, Path::new("/repo/.hg/store")));
        assert!(!is_vcs_admin_path(root, Path::new("/repo/.gitignore")));
        assert!(!is_vcs_admin_path(
            root,
            Path::new("/repo/.github/workflows/ci.yml")
        ));
    }

    #[test]
    fn default_ignores_match_only_explicit_names() {
        assert!(is_default_ignored_path(Path::new("/repo/target"), true));
        assert!(is_default_ignored_path(
            Path::new("/repo/node_modules"),
            true
        ));
        assert!(is_default_ignored_path(Path::new("/repo/.env"), false));
        assert!(is_default_ignored_path(
            Path::new("/repo/.env.production.local"),
            false
        ));

        assert!(!is_default_ignored_path(Path::new("/repo/targeted"), true));
        assert!(!is_default_ignored_path(
            Path::new("/repo/.env.example"),
            false
        ));
        assert!(!is_default_ignored_path(
            Path::new("/repo/.env.custom"),
            false
        ));
    }
}
