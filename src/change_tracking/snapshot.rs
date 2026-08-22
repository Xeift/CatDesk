use std::collections::{HashMap, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;

use ignore::WalkBuilder;

use super::ignore::is_vcs_admin_path;
use super::{ChangeTarget, MAX_FILE_CAPTURE_BYTES, MAX_WATCHED_ENTRIES};

#[derive(Clone, Debug, Default)]
pub(super) struct WorkspaceSnapshot {
    pub(super) files: HashMap<String, FileSnapshot>,
}

#[derive(Clone, Debug)]
pub(super) struct FileSnapshot {
    pub(super) digest: u64,
    pub(super) size_bytes: usize,
    pub(super) is_binary: bool,
    pub(super) is_directory: bool,
    pub(super) is_symlink: bool,
    pub(super) text: String,
    pub(super) text_truncated: bool,
}

pub(super) fn collect_snapshot(
    workspace_root: &Path,
    targets: &[ChangeTarget],
) -> WorkspaceSnapshot {
    let mut files = HashMap::new();
    let mut remaining = MAX_WATCHED_ENTRIES;

    for target in targets {
        if remaining == 0 {
            break;
        }
        collect_target(workspace_root, target, &mut files, &mut remaining);
    }

    WorkspaceSnapshot { files }
}

fn collect_target(
    workspace_root: &Path,
    target: &ChangeTarget,
    files: &mut HashMap<String, FileSnapshot>,
    remaining: &mut usize,
) {
    if *remaining == 0 || is_vcs_admin_path(workspace_root, &target.path) {
        return;
    }

    let Ok(metadata) = fs::symlink_metadata(&target.path) else {
        return;
    };
    let file_type = metadata.file_type();
    if file_type.is_file() || file_type.is_symlink() {
        capture_path(workspace_root, &target.path, files, remaining);
        return;
    }
    if !file_type.is_dir() {
        return;
    }

    if target.respect_project_ignores {
        collect_directory_with_project_ignores(
            workspace_root,
            &target.path,
            target.recursive,
            files,
            remaining,
        );
    } else {
        collect_directory_explicit(
            workspace_root,
            &target.path,
            target.recursive,
            files,
            remaining,
        );
    }
}

fn collect_directory_with_project_ignores(
    workspace_root: &Path,
    start: &Path,
    recursive: bool,
    files: &mut HashMap<String, FileSnapshot>,
    remaining: &mut usize,
) {
    let mut builder = WalkBuilder::new(start);
    builder
        .hidden(false)
        .parents(true)
        .ignore(true)
        .git_global(true)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .follow_links(false)
        .max_depth((!recursive).then_some(1));
    let root = workspace_root.to_path_buf();
    builder.filter_entry(move |entry| !is_vcs_admin_path(&root, entry.path()));

    for entry in builder.build().flatten() {
        if *remaining == 0 {
            return;
        }
        let path = entry.path();
        if is_vcs_admin_path(workspace_root, path) {
            continue;
        }
        capture_path(workspace_root, path, files, remaining);
    }
}

fn collect_directory_explicit(
    workspace_root: &Path,
    start: &Path,
    recursive: bool,
    files: &mut HashMap<String, FileSnapshot>,
    remaining: &mut usize,
) {
    capture_path(workspace_root, start, files, remaining);
    if *remaining == 0 {
        return;
    }

    let mut stack = vec![start.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            if *remaining == 0 {
                return;
            }
            let path = entry.path();
            if is_vcs_admin_path(workspace_root, &path) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            capture_path(workspace_root, &path, files, remaining);
            if file_type.is_dir() && recursive {
                stack.push(path);
            }
        }
    }
}

fn capture_path(
    workspace_root: &Path,
    path: &Path,
    files: &mut HashMap<String, FileSnapshot>,
    remaining: &mut usize,
) {
    if *remaining == 0 || is_vcs_admin_path(workspace_root, path) {
        return;
    }
    let key = relative_path(workspace_root, path);
    if files.contains_key(&key) {
        return;
    }
    let Some(snapshot) = capture_entry(path) else {
        return;
    };
    files.insert(key, snapshot);
    *remaining = remaining.saturating_sub(1);
}

fn capture_entry(path: &Path) -> Option<FileSnapshot> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        return Some(FileSnapshot {
            digest: 0,
            size_bytes: 0,
            is_binary: false,
            is_directory: true,
            is_symlink: false,
            text: String::new(),
            text_truncated: false,
        });
    }
    if file_type.is_symlink() {
        let target = fs::read_link(path).ok()?.to_string_lossy().into_owned();
        let mut hasher = DefaultHasher::new();
        target.hash(&mut hasher);
        return Some(FileSnapshot {
            digest: hasher.finish(),
            size_bytes: target.len(),
            is_binary: false,
            is_directory: false,
            is_symlink: true,
            text: target,
            text_truncated: false,
        });
    }
    if !file_type.is_file() {
        return None;
    }

    let data = fs::read(path).ok()?;
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    let digest = hasher.finish();
    let preview = &data[..data.len().min(MAX_FILE_CAPTURE_BYTES)];
    let is_binary = preview.iter().any(|byte| *byte == 0);
    let mut text = String::new();
    let text_truncated = data.len() > MAX_FILE_CAPTURE_BYTES;

    if !is_binary {
        text = String::from_utf8_lossy(preview).into_owned();
    }

    Some(FileSnapshot {
        digest,
        size_bytes: data.len(),
        is_binary,
        is_directory: false,
        is_symlink: false,
        text,
        text_truncated,
    })
}

pub(super) fn snapshots_equal(left: &FileSnapshot, right: &FileSnapshot) -> bool {
    left.digest == right.digest
        && left.size_bytes == right.size_bytes
        && left.is_binary == right.is_binary
        && left.is_directory == right.is_directory
        && left.is_symlink == right.is_symlink
}

fn relative_path(workspace_root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(workspace_root).unwrap_or(path);
    if relative.as_os_str().is_empty() {
        return "./".to_string();
    }
    let value = relative.display().to_string();
    #[cfg(windows)]
    {
        value.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        value
    }
}
