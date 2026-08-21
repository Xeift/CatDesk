use std::path::{Component, Path};

const VCS_ADMIN_COMPONENTS: &[&str] = &[".git", ".hg", ".svn", ".jj"];

pub(super) fn is_vcs_admin_path(workspace_root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(workspace_root).unwrap_or(path);
    relative.components().any(|component| match component {
        Component::Normal(value) => VCS_ADMIN_COMPONENTS
            .iter()
            .any(|blocked| value == std::ffi::OsStr::new(blocked)),
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
}
