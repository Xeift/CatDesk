use super::snapshot::{FileSnapshot, WorkspaceSnapshot, snapshots_equal};
use super::{MAX_DIFF_CHARS_PER_FILE, MAX_DIFF_FILES};

#[derive(Clone, Debug, Default)]
pub(crate) struct FileChange {
    pub(crate) path: String,
    pub(crate) status: String,
    pub(crate) added: u64,
    pub(crate) removed: u64,
    pub(crate) diff: String,
}

pub(super) fn diff_snapshots(
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
) -> Vec<FileChange> {
    let mut paths = before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();

    let mut changes = Vec::new();
    for path in paths {
        if let Some(change) = build_change(&path, before.files.get(&path), after.files.get(&path)) {
            changes.push(change);
            if changes.len() == MAX_DIFF_FILES {
                break;
            }
        }
    }
    changes
}

fn build_change(
    path: &str,
    before: Option<&FileSnapshot>,
    after: Option<&FileSnapshot>,
) -> Option<FileChange> {
    match (before, after) {
        (None, None) => None,
        (Some(left), Some(right)) if snapshots_equal(left, right) => None,
        (None, Some(right)) => build_added_change(path, right),
        (Some(left), None) => build_deleted_change(path, left),
        (Some(left), Some(right)) => build_modified_change(path, left, right),
    }
}

fn build_added_change(path: &str, after: &FileSnapshot) -> Option<FileChange> {
    let diff = truncate_diff(&build_added_diff(path, after));
    let (added, removed) = diff_line_stats(&diff);
    Some(FileChange {
        path: path.to_string(),
        status: "added".into(),
        added,
        removed,
        diff,
    })
}

fn build_deleted_change(path: &str, before: &FileSnapshot) -> Option<FileChange> {
    let diff = truncate_diff(&build_deleted_diff(path, before));
    let (added, removed) = diff_line_stats(&diff);
    Some(FileChange {
        path: path.to_string(),
        status: "deleted".into(),
        added,
        removed,
        diff,
    })
}

fn build_modified_change(
    path: &str,
    before: &FileSnapshot,
    after: &FileSnapshot,
) -> Option<FileChange> {
    let diff = truncate_diff(&build_modified_diff(path, before, after));
    let (added, removed) = diff_line_stats(&diff);
    Some(FileChange {
        path: path.to_string(),
        status: "modified".into(),
        added,
        removed,
        diff,
    })
}

fn build_added_diff(path: &str, after: &FileSnapshot) -> String {
    if after.is_directory {
        return format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,1 @@\n+<directory>\n");
    }
    if after.is_symlink {
        return format!(
            "--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,1 @@\n+<symlink -> {}>\n",
            after.text
        );
    }
    if after.is_binary {
        return format!(
            "--- /dev/null\n+++ b/{path}\nBinary file added ({} bytes)\n",
            after.size_bytes
        );
    }

    let mut diff = String::new();
    let lines = after.text.lines().count().max(1);
    diff.push_str(&format!(
        "--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{lines} @@\n"
    ));
    append_prefixed_lines(&mut diff, '+', &after.text);
    append_capture_truncation_note(&mut diff, after);
    diff
}

fn build_deleted_diff(path: &str, before: &FileSnapshot) -> String {
    if before.is_directory {
        return format!("--- a/{path}\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-<directory>\n");
    }
    if before.is_symlink {
        return format!(
            "--- a/{path}\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-<symlink -> {}>\n",
            before.text
        );
    }
    if before.is_binary {
        return format!(
            "--- a/{path}\n+++ /dev/null\nBinary file deleted ({} bytes)\n",
            before.size_bytes
        );
    }

    let mut diff = String::new();
    let lines = before.text.lines().count().max(1);
    diff.push_str(&format!(
        "--- a/{path}\n+++ /dev/null\n@@ -1,{lines} +0,0 @@\n"
    ));
    append_prefixed_lines(&mut diff, '-', &before.text);
    append_capture_truncation_note(&mut diff, before);
    diff
}

fn build_modified_diff(path: &str, before: &FileSnapshot, after: &FileSnapshot) -> String {
    if before.is_directory || after.is_directory || before.is_symlink || after.is_symlink {
        return format!(
            "--- a/{path}\n+++ b/{path}\n@@ -1,1 +1,1 @@\n-{}\n+{}\n",
            entry_marker(before),
            entry_marker(after)
        );
    }
    if before.is_binary || after.is_binary {
        return format!(
            "--- a/{path}\n+++ b/{path}\nBinary file changed ({} -> {} bytes)\n",
            before.size_bytes, after.size_bytes
        );
    }

    let before_lines = before.text.lines().collect::<Vec<_>>();
    let after_lines = after.text.lines().collect::<Vec<_>>();
    let ops = diff_lines(&before_lines, &after_lines);
    let has_line_level_change = ops.iter().any(|op| !matches!(op, LineDiffOp::Keep(_)));
    let before_count = before_lines.len();
    let after_count = after_lines.len();
    let before_start = usize::from(before_count > 0);
    let after_start = usize::from(after_count > 0);
    let mut diff = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -{before_start},{before_count} +{after_start},{after_count} @@\n"
    );

    if has_line_level_change {
        for op in ops {
            match op {
                LineDiffOp::Keep(line) => append_line(&mut diff, ' ', line),
                LineDiffOp::Delete(line) => append_line(&mut diff, '-', line),
                LineDiffOp::Insert(line) => append_line(&mut diff, '+', line),
            }
        }
    } else {
        append_prefixed_lines(&mut diff, '-', &before.text);
        append_prefixed_lines(&mut diff, '+', &after.text);
    }

    if before.text_truncated || after.text_truncated {
        diff.push_str("\n[file content preview truncated]\n");
    }
    diff
}

fn entry_marker(snapshot: &FileSnapshot) -> String {
    if snapshot.is_directory {
        "<directory>".to_string()
    } else if snapshot.is_symlink {
        format!("<symlink -> {}>", snapshot.text)
    } else if snapshot.is_binary {
        format!("<binary file: {} bytes>", snapshot.size_bytes)
    } else {
        "<file>".to_string()
    }
}

fn append_capture_truncation_note(diff: &mut String, snapshot: &FileSnapshot) {
    if snapshot.text_truncated {
        diff.push_str("\n[file content preview truncated]\n");
    }
}

fn append_line(out: &mut String, prefix: char, line: &str) {
    out.push(prefix);
    out.push_str(line);
    out.push('\n');
}

fn append_prefixed_lines(out: &mut String, prefix: char, text: &str) {
    if text.is_empty() {
        append_line(out, prefix, "");
        return;
    }
    for line in text.lines() {
        append_line(out, prefix, line);
    }
}

enum LineDiffOp<'a> {
    Keep(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

fn diff_lines<'a>(before: &'a [&'a str], after: &'a [&'a str]) -> Vec<LineDiffOp<'a>> {
    let n = before.len();
    let m = after.len();
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];

    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if before[i] == after[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if before[i] == after[j] {
            ops.push(LineDiffOp::Keep(before[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push(LineDiffOp::Delete(before[i]));
            i += 1;
        } else {
            ops.push(LineDiffOp::Insert(after[j]));
            j += 1;
        }
    }
    while i < n {
        ops.push(LineDiffOp::Delete(before[i]));
        i += 1;
    }
    while j < m {
        ops.push(LineDiffOp::Insert(after[j]));
        j += 1;
    }
    ops
}

fn diff_line_stats(diff: &str) -> (u64, u64) {
    let mut added = 0u64;
    let mut removed = 0u64;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added = added.saturating_add(1);
        } else if line.starts_with('-') {
            removed = removed.saturating_add(1);
        }
    }
    (added, removed)
}

fn truncate_diff(text: &str) -> String {
    if text.chars().count() <= MAX_DIFF_CHARS_PER_FILE {
        return text.to_string();
    }
    let keep = MAX_DIFF_CHARS_PER_FILE.saturating_sub(96);
    let mut out = text.chars().take(keep).collect::<String>();
    out.push_str("\n\n[diff truncated]\n");
    out
}
