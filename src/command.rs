use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::time::{Duration, timeout};

const MAX_BUFFER_BYTES: usize = 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 120_000;
pub const CATDESK_CO_AUTHOR_TRAILER: &str = "Co-Authored-By: CatDesk";

#[derive(Debug)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

/// Clamp timeout to [1, MAX_TIMEOUT_MS].
pub fn clamp_timeout(t: Option<u64>) -> u64 {
    match t {
        Some(v) if v >= 1 => v.min(MAX_TIMEOUT_MS),
        _ => DEFAULT_TIMEOUT_MS,
    }
}

/// Resolve `input` relative to `workspace_root`, rejecting path traversal.
pub fn resolve_workspace_path(
    workspace_root: &str,
    input: Option<&str>,
) -> Result<PathBuf, String> {
    let root = Path::new(workspace_root)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let input = input.unwrap_or(".");

    let candidate = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        root.join(input)
    };

    let candidate = candidate.canonicalize().unwrap_or(candidate);
    if !candidate.starts_with(&root) {
        return Err(format!(
            "Path escapes workspace root: {}",
            candidate.display()
        ));
    }
    Ok(candidate)
}

/// Execute a shell command via `/bin/bash`.
pub async fn run_command(command: &str, cwd: &Path, timeout_ms: u64) -> CommandResult {
    let fut = Command::new("/bin/bash")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .output();

    match timeout(Duration::from_millis(timeout_ms), fut).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(
                &output.stdout[..output.stdout.len().min(MAX_BUFFER_BYTES)],
            )
            .to_string();
            let stderr = String::from_utf8_lossy(
                &output.stderr[..output.stderr.len().min(MAX_BUFFER_BYTES)],
            )
            .to_string();
            CommandResult {
                stdout,
                stderr,
                success: output.status.success(),
            }
        }
        Ok(Err(e)) => CommandResult {
            stdout: String::new(),
            stderr: format!("Failed to execute: {e}"),
            success: false,
        },
        Err(_) => CommandResult {
            stdout: String::new(),
            stderr: format!("Command timed out after {timeout_ms} ms"),
            success: false,
        },
    }
}

/// Format stdout+stderr into a single string.
pub fn format_result(r: &CommandResult) -> String {
    let mut out = String::new();
    if !r.stdout.is_empty() {
        out.push_str(&r.stdout);
    }
    if !r.stderr.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\nSTDERR:\n");
        } else {
            out.push_str("STDERR:\n");
        }
        out.push_str(&r.stderr);
    }
    if out.is_empty() {
        out.push_str("(no output)");
    }
    out
}

pub fn contains_catdesk_co_author_marker(command: &str) -> bool {
    let haystack = command.to_ascii_lowercase();
    let mut cursor = 0usize;
    for needle in ["co", "author", "by", "catdesk"] {
        let Some(offset) = haystack[cursor..].find(needle) else {
            return false;
        };
        cursor += offset + needle.len();
    }
    true
}

pub fn command_contains_git_commit(command: &str) -> bool {
    shell_segments(command)
        .iter()
        .any(|segment| git_commit_insert_pos(segment).is_some())
}

pub fn inject_catdesk_co_author_trailer(command: &str) -> String {
    let mut rewritten = String::with_capacity(command.len() + 64);
    for segment in shell_segments(command) {
        rewritten.push_str(&inject_trailer_into_segment(&segment));
    }
    rewritten
}

fn inject_trailer_into_segment(segment: &str) -> String {
    let Some(insert_pos) = git_commit_insert_pos(segment) else {
        return segment.to_string();
    };
    let mut rewritten = String::with_capacity(segment.len() + 48);
    rewritten.push_str(&segment[..insert_pos]);
    rewritten.push_str(" --trailer '");
    rewritten.push_str(CATDESK_CO_AUTHOR_TRAILER);
    rewritten.push('\'');
    rewritten.push_str(&segment[insert_pos..]);
    rewritten
}

fn git_commit_insert_pos(segment: &str) -> Option<usize> {
    let words = shell_words(segment);
    let git_idx = command_start_git_index(&words)?;
    let commit_word = words[git_idx + 1..]
        .iter()
        .find(|word| word.text == "commit")?;
    Some(commit_word.end)
}

#[derive(Clone)]
struct ShellWord {
    text: String,
    end: usize,
}

fn shell_words(segment: &str) -> Vec<ShellWord> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut start: Option<usize> = None;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for (idx, ch) in segment.char_indices() {
        if escaped {
            if start.is_none() {
                start = Some(idx);
            }
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && !in_single {
            if start.is_none() {
                start = Some(idx);
            }
            escaped = true;
            continue;
        }
        if ch == '\'' && !in_double {
            if start.is_none() {
                start = Some(idx);
            }
            in_single = !in_single;
            continue;
        }
        if ch == '"' && !in_single {
            if start.is_none() {
                start = Some(idx);
            }
            in_double = !in_double;
            continue;
        }
        if !in_single && !in_double && ch.is_whitespace() {
            if start.is_some() {
                words.push(ShellWord {
                    text: current.to_ascii_lowercase(),
                    end: idx,
                });
                current.clear();
                start = None;
            }
            continue;
        }
        if start.is_none() {
            start = Some(idx);
        }
        current.push(ch);
    }

    if start.is_some() {
        words.push(ShellWord {
            text: current.to_ascii_lowercase(),
            end: segment.len(),
        });
    }

    words
}

fn command_start_git_index(words: &[ShellWord]) -> Option<usize> {
    let mut idx = 0usize;
    while idx < words.len() && looks_like_env_assignment(&words[idx].text) {
        idx += 1;
    }
    loop {
        let word = words.get(idx)?;
        match word.text.as_str() {
            "sudo" => {
                idx += 1;
                while idx < words.len() && words[idx].text.starts_with('-') {
                    idx += 1;
                }
            }
            "env" => {
                idx += 1;
                while idx < words.len()
                    && (words[idx].text.starts_with('-')
                        || looks_like_env_assignment(&words[idx].text))
                {
                    idx += 1;
                }
            }
            _ => break,
        }
    }
    words.get(idx).filter(|word| word.text == "git").map(|_| idx)
}

fn looks_like_env_assignment(word: &str) -> bool {
    let Some((name, _value)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn shell_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut chars = command.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && !in_single {
            escaped = true;
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            continue;
        }
        if in_single || in_double {
            continue;
        }

        let separator_len = match ch {
            ';' | '\n' => Some(1usize),
            '&' => {
                if matches!(chars.peek(), Some((_, '&'))) {
                    chars.next();
                    Some(2usize)
                } else {
                    Some(1usize)
                }
            }
            '|' => {
                if matches!(chars.peek(), Some((_, '|'))) {
                    chars.next();
                    Some(2usize)
                } else {
                    Some(1usize)
                }
            }
            _ => None,
        };

        if let Some(separator_len) = separator_len {
            segments.push(command[start..idx + separator_len].to_string());
            start = idx + separator_len;
        }
    }

    if start < command.len() {
        segments.push(command[start..].to_string());
    }

    if segments.is_empty() {
        segments.push(String::new());
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_catdesk_co_author_marker_matches_spaced_and_punctuated_phrase() {
        assert!(contains_catdesk_co_author_marker(
            "git commit -m \"Fix bug\\n\\nCo-Authored-By: CatDesk\""
        ));
        assert!(contains_catdesk_co_author_marker(
            "git commit -m \"co***author___by:::catdesk\""
        ));
        assert!(!contains_catdesk_co_author_marker("git commit -m \"fix bug\""));
    }

    #[test]
    fn inject_catdesk_co_author_trailer_rewrites_each_git_commit_segment() {
        let rewritten = inject_catdesk_co_author_trailer(
            "git add . && git commit -m \"test\" && git status",
        );
        assert_eq!(
            rewritten,
            "git add . && git commit --trailer 'Co-Authored-By: CatDesk' -m \"test\" && git status"
        );
    }

    #[test]
    fn command_contains_git_commit_only_matches_real_commit_tokens() {
        assert!(command_contains_git_commit("git commit -m \"x\""));
        assert!(command_contains_git_commit("FOO=1 git -C repo commit -m \"x\""));
        assert!(!command_contains_git_commit("echo git commit"));
    }
}
