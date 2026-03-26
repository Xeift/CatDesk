use crate::command;
use serde::Serialize;
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAX_READ_BYTES: usize = 512 * 1024;
const MAX_WRITE_BYTES: usize = 512 * 1024;
const DEFAULT_LIST_LIMIT: usize = 200;
const HARD_LIST_LIMIT: usize = 1000;
const DEFAULT_SEARCH_LIMIT: usize = 100;
const HARD_SEARCH_LIMIT: usize = 500;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFilesEntry {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub depth: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFilesOutput {
    pub path: String,
    pub item_count: usize,
    pub directory_count: usize,
    pub file_count: usize,
    pub other_count: usize,
    pub truncated: bool,
    pub limit: usize,
    pub entries: Vec<ListFilesEntry>,
}

impl ListFilesOutput {
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("path: {}\n", self.path));
        out.push_str(&format!("items: {}\n\n", self.item_count));
        out.push_str(
            &self
                .entries
                .iter()
                .map(|entry| match entry.kind.as_str() {
                    "dir" => format!("dir  {}/", entry.path),
                    "file" => format!("file {}", entry.path),
                    _ => format!("other {}", entry.path),
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
        if self.truncated {
            out.push_str(&format!("\n\n[truncated at {} items]", self.limit));
        }
        out
    }
}

fn workspace_root_path(workspace_root: &str) -> Result<PathBuf, String> {
    Path::new(workspace_root)
        .canonicalize()
        .map_err(|e| e.to_string())
}

fn to_workspace_relative(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => rel.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

fn safe_limit(value: Option<usize>, default_value: usize, hard_max: usize) -> usize {
    value.unwrap_or(default_value).clamp(1, hard_max)
}

fn resolve_target_path(workspace_root: &str, path: &str) -> Result<PathBuf, String> {
    command::resolve_workspace_path(workspace_root, Some(path))
}

pub fn read_file(workspace_root: &str, path: &str) -> Result<String, String> {
    let root = workspace_root_path(workspace_root)?;
    let target = resolve_target_path(workspace_root, path)?;
    if !target.exists() {
        return Err(format!("File not found: {}", target.display()));
    }
    if !target.is_file() {
        return Err(format!("Not a file: {}", target.display()));
    }

    let mut file = fs::File::open(&target).map_err(|e| e.to_string())?;
    let mut buf = vec![0_u8; MAX_READ_BYTES + 1];
    let read_n = file.read(&mut buf).map_err(|e| e.to_string())?;
    let truncated = read_n > MAX_READ_BYTES;
    let data = &buf[..read_n.min(MAX_READ_BYTES)];
    let text = String::from_utf8_lossy(data).into_owned();

    let mut out = String::new();
    out.push_str(&format!(
        "path: {}\n",
        to_workspace_relative(&root, &target)
    ));
    out.push_str(&format!("bytes: {}\n\n", data.len()));
    out.push_str(&text);
    if truncated {
        out.push_str(&format!("\n\n[truncated at {} bytes]", MAX_READ_BYTES));
    }
    Ok(out)
}

pub fn write_file(
    workspace_root: &str,
    path: &str,
    content: &str,
    create_dirs: bool,
) -> Result<String, String> {
    if content.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Content too large: {} bytes (max {})",
            content.len(),
            MAX_WRITE_BYTES
        ));
    }

    let root = workspace_root_path(workspace_root)?;
    let target = resolve_target_path(workspace_root, path)?;
    if let Some(parent) = target.parent() {
        if !parent.exists() {
            if create_dirs {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            } else {
                return Err(format!(
                    "Parent directory does not exist: {} (set create_dirs=true)",
                    parent.display()
                ));
            }
        }
    }

    fs::write(&target, content).map_err(|e| e.to_string())?;
    Ok(format!(
        "wrote {} bytes to {}",
        content.len(),
        to_workspace_relative(&root, &target)
    ))
}

pub fn append_file(
    workspace_root: &str,
    path: &str,
    content: &str,
    create_dirs: bool,
) -> Result<String, String> {
    if content.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Content too large: {} bytes (max {})",
            content.len(),
            MAX_WRITE_BYTES
        ));
    }

    let root = workspace_root_path(workspace_root)?;
    let target = resolve_target_path(workspace_root, path)?;
    if let Some(parent) = target.parent() {
        if !parent.exists() {
            if create_dirs {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            } else {
                return Err(format!(
                    "Parent directory does not exist: {} (set create_dirs=true)",
                    parent.display()
                ));
            }
        }
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&target)
        .map_err(|e| e.to_string())?;
    file.write_all(content.as_bytes())
        .map_err(|e| e.to_string())?;

    Ok(format!(
        "appended {} bytes to {}",
        content.len(),
        to_workspace_relative(&root, &target)
    ))
}

pub fn list_files(
    workspace_root: &str,
    path: Option<&str>,
    include_hidden: bool,
    limit: Option<usize>,
) -> Result<ListFilesOutput, String> {
    let root = workspace_root_path(workspace_root)?;
    let start = command::resolve_workspace_path(workspace_root, path)?;
    if !start.exists() {
        return Err(format!("Path not found: {}", start.display()));
    }
    if !start.is_dir() {
        return Err(format!("Not a directory: {}", start.display()));
    }

    let max_items = safe_limit(limit, DEFAULT_LIST_LIMIT, HARD_LIST_LIMIT);
    let mut queue = VecDeque::new();
    queue.push_back(start.clone());

    let mut entries: Vec<ListFilesEntry> = Vec::new();
    let mut directory_count: usize = 0;
    let mut file_count: usize = 0;
    let mut other_count: usize = 0;
    let mut truncated = false;

    while let Some(dir) = queue.pop_front() {
        let mut dir_entries = fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .map(|entry_res| {
                let entry = entry_res.map_err(|e| e.to_string())?;
                let name = entry.file_name().to_string_lossy().to_string();
                Ok((name, entry))
            })
            .collect::<Result<Vec<_>, String>>()?;
        dir_entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, entry) in dir_entries {
            if !include_hidden && name.starts_with('.') {
                continue;
            }

            let path = entry.path();
            let ft = entry.file_type().map_err(|e| e.to_string())?;
            let rel = to_workspace_relative(&root, &path);
            let rel_from_start = path.strip_prefix(&start).unwrap_or(path.as_path());
            let depth = rel_from_start.components().count().saturating_sub(1);

            if ft.is_dir() {
                directory_count += 1;
                entries.push(ListFilesEntry {
                    path: rel,
                    name,
                    kind: "dir".into(),
                    depth,
                });
                queue.push_back(path);
            } else if ft.is_file() {
                file_count += 1;
                entries.push(ListFilesEntry {
                    path: rel,
                    name,
                    kind: "file".into(),
                    depth,
                });
            } else {
                other_count += 1;
                entries.push(ListFilesEntry {
                    path: rel,
                    name,
                    kind: "other".into(),
                    depth,
                });
            }

            if entries.len() >= max_items {
                truncated = true;
                break;
            }
        }

        if truncated {
            break;
        }
    }

    Ok(ListFilesOutput {
        path: to_workspace_relative(&root, &start),
        item_count: entries.len(),
        directory_count,
        file_count,
        other_count,
        truncated,
        limit: max_items,
        entries,
    })
}

pub fn search_text(
    workspace_root: &str,
    query: &str,
    path: Option<&str>,
    include_hidden: bool,
    limit: Option<usize>,
) -> Result<String, String> {
    if query.trim().is_empty() {
        return Err("Query must not be empty".into());
    }

    let root = workspace_root_path(workspace_root)?;
    let start = command::resolve_workspace_path(workspace_root, path)?;
    if !start.exists() {
        return Err(format!("Path not found: {}", start.display()));
    }
    if !start.is_dir() {
        return Err(format!("Not a directory: {}", start.display()));
    }

    let max_matches = safe_limit(limit, DEFAULT_SEARCH_LIMIT, HARD_SEARCH_LIMIT);
    let mut queue = VecDeque::new();
    queue.push_back(start.clone());

    let mut matches: Vec<String> = Vec::new();
    let mut visited_files: usize = 0;
    let mut truncated = false;

    while let Some(dir) = queue.pop_front() {
        let entries = fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for entry_res in entries {
            let entry = entry_res.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !include_hidden && name.starts_with('.') {
                continue;
            }

            let path = entry.path();
            let ft = entry.file_type().map_err(|e| e.to_string())?;
            if ft.is_dir() {
                queue.push_back(path);
                continue;
            }
            if !ft.is_file() {
                continue;
            }

            visited_files += 1;
            let data = fs::read(&path).map_err(|e| e.to_string())?;
            if data.iter().any(|b| *b == 0) {
                continue;
            }
            let text = String::from_utf8_lossy(&data);
            let rel = to_workspace_relative(&root, &path);
            for (idx, line) in text.lines().enumerate() {
                if line.contains(query) {
                    matches.push(format!("{rel}:{}: {}", idx + 1, line));
                    if matches.len() >= max_matches {
                        truncated = true;
                        break;
                    }
                }
            }
            if truncated {
                break;
            }
        }
        if truncated {
            break;
        }
    }

    let mut out = String::new();
    out.push_str(&format!("query: {query}\n"));
    out.push_str(&format!("path: {}\n", to_workspace_relative(&root, &start)));
    out.push_str(&format!("files_scanned: {visited_files}\n"));
    out.push_str(&format!("matches: {}\n\n", matches.len()));
    out.push_str(&matches.join("\n"));
    if truncated {
        out.push_str(&format!("\n\n[truncated at {} matches]", max_matches));
    }
    Ok(out)
}

pub fn make_directory(workspace_root: &str, path: &str, recursive: bool) -> Result<String, String> {
    let root = workspace_root_path(workspace_root)?;
    let target = resolve_target_path(workspace_root, path)?;

    if target.exists() {
        if target.is_dir() {
            return Ok(format!(
                "directory already exists: {}",
                to_workspace_relative(&root, &target)
            ));
        }
        return Err(format!(
            "Path exists and is not a directory: {}",
            target.display()
        ));
    }

    if recursive {
        fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    } else {
        fs::create_dir(&target).map_err(|e| e.to_string())?;
    }

    Ok(format!(
        "created directory: {}",
        to_workspace_relative(&root, &target)
    ))
}

pub fn delete_path(workspace_root: &str, path: &str, recursive: bool) -> Result<String, String> {
    let root = workspace_root_path(workspace_root)?;
    let target = resolve_target_path(workspace_root, path)?;

    if !target.exists() {
        return Err(format!("Path not found: {}", target.display()));
    }

    let meta = fs::symlink_metadata(&target).map_err(|e| e.to_string())?;
    let ft = meta.file_type();
    if ft.is_dir() {
        if recursive {
            fs::remove_dir_all(&target).map_err(|e| e.to_string())?;
            Ok(format!(
                "deleted directory recursively: {}",
                to_workspace_relative(&root, &target)
            ))
        } else {
            fs::remove_dir(&target).map_err(|e| e.to_string())?;
            Ok(format!(
                "deleted empty directory: {}",
                to_workspace_relative(&root, &target)
            ))
        }
    } else {
        fs::remove_file(&target).map_err(|e| e.to_string())?;
        Ok(format!(
            "deleted file: {}",
            to_workspace_relative(&root, &target)
        ))
    }
}

fn copy_file(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::copy(src, dst).map_err(|e| e.to_string())?;
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry_res in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry_res.map_err(|e| e.to_string())?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            copy_file(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

pub fn move_path(
    workspace_root: &str,
    from: &str,
    to: &str,
    overwrite: bool,
    create_dirs: bool,
) -> Result<String, String> {
    let root = workspace_root_path(workspace_root)?;
    let src = resolve_target_path(workspace_root, from)?;
    let dst = resolve_target_path(workspace_root, to)?;

    if !src.exists() {
        return Err(format!("Source path not found: {}", src.display()));
    }

    if src == dst {
        return Ok("source and destination are the same path".into());
    }

    if let Some(parent) = dst.parent() {
        if !parent.exists() {
            if create_dirs {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            } else {
                return Err(format!(
                    "Destination parent does not exist: {} (set create_dirs=true)",
                    parent.display()
                ));
            }
        }
    }

    if dst.exists() {
        if !overwrite {
            return Err(format!(
                "Destination already exists: {} (set overwrite=true)",
                dst.display()
            ));
        }
        let meta = fs::symlink_metadata(&dst).map_err(|e| e.to_string())?;
        if meta.file_type().is_dir() {
            fs::remove_dir_all(&dst).map_err(|e| e.to_string())?;
        } else {
            fs::remove_file(&dst).map_err(|e| e.to_string())?;
        }
    }

    match fs::rename(&src, &dst) {
        Ok(_) => {}
        Err(e) => {
            // Cross-device rename fallback.
            if e.raw_os_error() == Some(18) {
                let meta = fs::symlink_metadata(&src).map_err(|err| err.to_string())?;
                if meta.file_type().is_dir() {
                    copy_dir_recursive(&src, &dst)?;
                    fs::remove_dir_all(&src).map_err(|err| err.to_string())?;
                } else {
                    copy_file(&src, &dst)?;
                    fs::remove_file(&src).map_err(|err| err.to_string())?;
                }
            } else {
                return Err(e.to_string());
            }
        }
    }

    Ok(format!(
        "moved {} -> {}",
        to_workspace_relative(&root, &src),
        to_workspace_relative(&root, &dst)
    ))
}

pub fn replace_in_file(
    workspace_root: &str,
    path: &str,
    find: &str,
    replace: &str,
    replace_all: bool,
) -> Result<String, String> {
    if find.is_empty() {
        return Err("find must not be empty".into());
    }

    let root = workspace_root_path(workspace_root)?;
    let target = resolve_target_path(workspace_root, path)?;
    if !target.exists() {
        return Err(format!("File not found: {}", target.display()));
    }
    if !target.is_file() {
        return Err(format!("Not a file: {}", target.display()));
    }

    let content = fs::read_to_string(&target).map_err(|e| e.to_string())?;
    let replaced_content;
    let replaced_count: usize;
    if replace_all {
        replaced_count = content.matches(find).count();
        replaced_content = content.replace(find, replace);
    } else if let Some(pos) = content.find(find) {
        replaced_count = 1;
        let mut out =
            String::with_capacity(content.len() + replace.len().saturating_sub(find.len()));
        out.push_str(&content[..pos]);
        out.push_str(replace);
        out.push_str(&content[pos + find.len()..]);
        replaced_content = out;
    } else {
        replaced_count = 0;
        replaced_content = content;
    }

    if replaced_count == 0 {
        return Ok(format!(
            "no match found in {}",
            to_workspace_relative(&root, &target)
        ));
    }

    fs::write(&target, replaced_content).map_err(|e| e.to_string())?;
    Ok(format!(
        "replaced {} occurrence(s) in {}",
        replaced_count,
        to_workspace_relative(&root, &target)
    ))
}
