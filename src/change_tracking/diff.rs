use super::snapshot::{FileSnapshot, WorkspaceSnapshot, snapshots_equal};
use super::{MAX_DIFF_CHARS_PER_FILE, MAX_DIFF_FILES};

const DIFF_CONTEXT_LINES: usize = 3;

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
    let full_diff = build_added_diff(path, after);
    let (added, removed) = diff_line_stats(&full_diff);
    let diff = truncate_diff(&full_diff);
    Some(FileChange {
        path: path.to_string(),
        status: "added".into(),
        added,
        removed,
        diff,
    })
}

fn build_deleted_change(path: &str, before: &FileSnapshot) -> Option<FileChange> {
    let full_diff = build_deleted_diff(path, before);
    let (added, removed) = diff_line_stats(&full_diff);
    let diff = truncate_diff(&full_diff);
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
    let full_diff = build_modified_diff(path, before, after);
    let (added, removed) = diff_line_stats(&full_diff);
    let diff = truncate_diff(&full_diff);
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
    let has_line_level_change = ops.iter().any(LineDiffOp::is_change);
    let mut diff = format!("--- a/{path}\n+++ b/{path}\n");

    if has_line_level_change {
        append_context_hunks(&mut diff, &ops);
    } else {
        let before_count = before_lines.len();
        let after_count = after_lines.len();
        let before_start = usize::from(before_count > 0);
        let after_start = usize::from(after_count > 0);
        diff.push_str(&format!(
            "@@ -{before_start},{before_count} +{after_start},{after_count} @@\n"
        ));
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

impl LineDiffOp<'_> {
    fn is_change(&self) -> bool {
        !matches!(self, Self::Keep(_))
    }

    fn consumes_old(&self) -> bool {
        !matches!(self, Self::Insert(_))
    }

    fn consumes_new(&self) -> bool {
        !matches!(self, Self::Delete(_))
    }

    fn prefix(&self) -> char {
        match self {
            Self::Keep(_) => ' ',
            Self::Delete(_) => '-',
            Self::Insert(_) => '+',
        }
    }

    fn line(&self) -> &str {
        match self {
            Self::Keep(line) | Self::Delete(line) | Self::Insert(line) => line,
        }
    }
}

fn append_context_hunks(out: &mut String, ops: &[LineDiffOp<'_>]) {
    let mut old_lines = Vec::with_capacity(ops.len() + 1);
    let mut new_lines = Vec::with_capacity(ops.len() + 1);
    let (mut old_line, mut new_line) = (1usize, 1usize);

    for op in ops {
        old_lines.push(old_line);
        new_lines.push(new_line);
        if op.consumes_old() {
            old_line += 1;
        }
        if op.consumes_new() {
            new_line += 1;
        }
    }
    old_lines.push(old_line);
    new_lines.push(new_line);

    let mut hunks = Vec::<(usize, usize)>::new();
    for change_idx in ops
        .iter()
        .enumerate()
        .filter_map(|(idx, op)| op.is_change().then_some(idx))
    {
        let start = change_idx.saturating_sub(DIFF_CONTEXT_LINES);
        let end = (change_idx + DIFF_CONTEXT_LINES + 1).min(ops.len());
        if let Some((_, last_end)) = hunks.last_mut()
            && start <= *last_end
        {
            *last_end = (*last_end).max(end);
        } else {
            hunks.push((start, end));
        }
    }

    for (start, end) in hunks {
        let old_count = ops[start..end]
            .iter()
            .filter(|op| op.consumes_old())
            .count();
        let new_count = ops[start..end]
            .iter()
            .filter(|op| op.consumes_new())
            .count();
        let old_start = if old_count == 0 {
            old_lines[start].saturating_sub(1)
        } else {
            old_lines[start]
        };
        let new_start = if new_count == 0 {
            new_lines[start].saturating_sub(1)
        } else {
            new_lines[start]
        };

        out.push_str(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));
        for op in &ops[start..end] {
            append_line(out, op.prefix(), op.line());
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn text_snapshot(text: String) -> FileSnapshot {
        FileSnapshot {
            digest: text.len() as u64,
            size_bytes: text.len(),
            is_binary: false,
            is_directory: false,
            is_symlink: false,
            text,
            text_truncated: false,
        }
    }

    fn numbered_lines(count: usize) -> String {
        (1..=count)
            .map(|line| format!("line {line:03}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn modified_diff_limits_single_change_to_three_lines_of_context() {
        let before = text_snapshot(numbered_lines(40));
        let mut after_lines = numbered_lines(40)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        after_lines[24] = "changed line 025".to_string();
        let after = text_snapshot(after_lines.join("\n"));

        let diff = build_modified_diff("sample.py", &before, &after);

        assert!(diff.contains("@@ -22,7 +22,7 @@"));
        assert!(diff.contains(" line 022"));
        assert!(diff.contains("-line 025"));
        assert!(diff.contains("+changed line 025"));
        assert!(diff.contains(" line 028"));
        assert!(!diff.contains(" line 021"));
        assert!(!diff.contains(" line 029"));
    }

    #[test]
    fn modified_diff_splits_distant_changes_into_multiple_hunks() {
        let before = text_snapshot(numbered_lines(100));
        let mut after_lines = numbered_lines(100)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        after_lines[24] = "changed line 025".to_string();
        after_lines[74] = "changed line 075".to_string();
        let after = text_snapshot(after_lines.join("\n"));

        let diff = build_modified_diff("sample.py", &before, &after);

        assert_eq!(
            diff.lines().filter(|line| line.starts_with("@@")).count(),
            2
        );
        assert!(diff.contains("@@ -22,7 +22,7 @@"));
        assert!(diff.contains("@@ -72,7 +72,7 @@"));
        assert!(!diff.contains(" line 050"));
    }

    #[test]
    fn modified_diff_merges_nearby_change_context() {
        let before = text_snapshot(numbered_lines(30));
        let mut after_lines = numbered_lines(30)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        after_lines[9] = "changed line 010".to_string();
        after_lines[14] = "changed line 015".to_string();
        let after = text_snapshot(after_lines.join("\n"));

        let diff = build_modified_diff("sample.py", &before, &after);

        assert_eq!(
            diff.lines().filter(|line| line.starts_with("@@")).count(),
            1
        );
        assert!(diff.contains("-line 010"));
        assert!(diff.contains("+changed line 015"));
    }
}
