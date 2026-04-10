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
        .any(|segment| segment_contains_git_commit(segment))
}

pub fn inject_catdesk_co_author_trailer(command: &str) -> String {
    let mut rewritten = String::with_capacity(command.len() + 64);
    for segment in shell_segments(command) {
        rewritten.push_str(&inject_trailer_into_segment(&segment));
    }
    rewritten
}

fn segment_contains_git_commit(segment: &str) -> bool {
    if git_commit_insert_pos(segment).is_some() {
        return true;
    }
    nested_shell_command(segment)
        .map(|payload| command_contains_git_commit(&payload.command))
        .unwrap_or(false)
}

fn inject_trailer_into_segment(segment: &str) -> String {
    if let Some(insert_pos) = git_commit_insert_pos(segment) {
        let mut rewritten = String::with_capacity(segment.len() + 48);
        rewritten.push_str(&segment[..insert_pos]);
        rewritten.push_str(" --trailer '");
        rewritten.push_str(CATDESK_CO_AUTHOR_TRAILER);
        rewritten.push('\'');
        rewritten.push_str(&segment[insert_pos..]);
        return rewritten;
    }

    let Some(payload) = nested_shell_command(segment) else {
        return segment.to_string();
    };
    let rewritten_command = inject_catdesk_co_author_trailer(&payload.command);
    if rewritten_command == payload.command {
        return segment.to_string();
    }

    let mut rewritten = String::with_capacity(segment.len() + 64);
    rewritten.push_str(&segment[..payload.start]);
    rewritten.push_str(&shell_single_quote(&rewritten_command));
    rewritten.push_str(&segment[payload.end..]);
    rewritten
}

fn git_commit_insert_pos(segment: &str) -> Option<usize> {
    let words = shell_words(segment);
    let git_idx = command_start_git_index(&words)?;
    let commit_word = words[git_idx + 1..]
        .iter()
        .find(|word| word.lower == "commit")?;
    Some(commit_word.end)
}

#[derive(Clone)]
struct ShellWord {
    text: String,
    lower: String,
    start: usize,
    end: usize,
}

struct NestedShellCommand {
    command: String,
    start: usize,
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
            if let Some(word_start) = start {
                words.push(ShellWord {
                    lower: current.to_ascii_lowercase(),
                    text: current.clone(),
                    start: word_start,
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

    if let Some(word_start) = start {
        words.push(ShellWord {
            lower: current.to_ascii_lowercase(),
            text: current.clone(),
            start: word_start,
            end: segment.len(),
        });
    }

    words
}

fn nested_shell_command(segment: &str) -> Option<NestedShellCommand> {
    let words = shell_words(segment);
    let shell_idx = command_start_shell_index(&words)?;
    let command_idx = shell_command_arg_index(&words, shell_idx)?;
    let payload = words.get(command_idx)?;
    Some(NestedShellCommand {
        command: payload.text.clone(),
        start: payload.start,
        end: payload.end,
    })
}

fn command_start_git_index(words: &[ShellWord]) -> Option<usize> {
    command_start_index(words, |word| word == "git")
}

fn command_start_shell_index(words: &[ShellWord]) -> Option<usize> {
    command_start_index(words, is_shell_command)
}

fn command_start_index<F>(words: &[ShellWord], matches_command: F) -> Option<usize>
where
    F: Fn(&str) -> bool,
{
    let mut idx = 0usize;
    while idx < words.len() && looks_like_env_assignment(&words[idx].text) {
        idx += 1;
    }
    loop {
        let word = words.get(idx)?;
        match word.lower.as_str() {
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
    words
        .get(idx)
        .filter(|word| matches_command(&word.lower))
        .map(|_| idx)
}

fn is_shell_command(word: &str) -> bool {
    matches!(
        word.rsplit('/').next().unwrap_or(word),
        "bash" | "sh" | "zsh" | "dash"
    )
}

fn shell_command_arg_index(words: &[ShellWord], shell_idx: usize) -> Option<usize> {
    let mut idx = shell_idx + 1;
    while idx < words.len() {
        let word = &words[idx].lower;
        if word == "--" {
            return words.get(idx + 1).map(|_| idx + 1);
        }
        if word == "-c" {
            return words.get(idx + 1).map(|_| idx + 1);
        }
        if word.starts_with('-')
            && word.len() > 2
            && word[1..].chars().all(|ch| matches!(ch, 'c' | 'l'))
            && word[1..].contains('c')
        {
            return words.get(idx + 1).map(|_| idx + 1);
        }
        if !word.starts_with('-') {
            return None;
        }
        idx += 1;
    }
    None
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

fn shell_single_quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for ch in text.chars() {
        if ch == '\'' {
            quoted.push_str("'\"'\"'");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
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
        assert!(!contains_catdesk_co_author_marker(
            "git commit -m \"fix bug\""
        ));
    }

    #[test]
    fn inject_catdesk_co_author_trailer_rewrites_each_git_commit_segment() {
        let rewritten =
            inject_catdesk_co_author_trailer("git add . && git commit -m \"test\" && git status");
        assert_eq!(
            rewritten,
            "git add . && git commit --trailer 'Co-Authored-By: CatDesk' -m \"test\" && git status"
        );
    }

    #[test]
    fn inject_catdesk_co_author_trailer_rewrites_nested_shell_commit_commands() {
        let rewritten = inject_catdesk_co_author_trailer(
            "bash -lc 'git add src/widget/catdesk_dashboard.html && git commit -m \"Update catdesk widget meta handling\"'",
        );
        assert_eq!(
            rewritten,
            "bash -lc 'git add src/widget/catdesk_dashboard.html && git commit --trailer '\"'\"'Co-Authored-By: CatDesk'\"'\"' -m \"Update catdesk widget meta handling\"'"
        );
    }

    #[test]
    fn command_contains_git_commit_only_matches_real_commit_tokens() {
        assert!(command_contains_git_commit("git commit -m \"x\""));
        assert!(command_contains_git_commit(
            "FOO=1 git -C repo commit -m \"x\""
        ));
        assert!(command_contains_git_commit(
            "bash -lc 'git commit -m \"x\"'"
        ));
        assert!(!command_contains_git_commit("echo git commit"));
    }
}
