use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use tree_sitter::{Node, Parser};
use tree_sitter_bash::LANGUAGE as BASH_LANGUAGE;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileListingFilter {
    All,
    FilesOnly,
    DirectoriesOnly,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterceptedListFilesRequest {
    pub path: Option<String>,
    pub include_hidden: bool,
    pub filter: FileListingFilter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterceptedMovePathRequest {
    pub from: String,
    pub to: String,
    pub overwrite: bool,
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

/// Resolve `input` relative to `cwd`, rejecting path traversal outside the workspace root.
pub fn resolve_command_path(
    workspace_root: &str,
    cwd: &Path,
    input: Option<&str>,
) -> Result<PathBuf, String> {
    let root = Path::new(workspace_root)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let input = input.unwrap_or(".");

    let candidate = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        cwd.join(input)
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

pub fn detect_list_files_intercept(command: &str) -> Option<InterceptedListFilesRequest> {
    let words = parse_word_only_shell_command(command)?;
    detect_list_files_intercept_from_words(&words)
}

pub fn detect_move_path_intercept(command: &str) -> Option<InterceptedMovePathRequest> {
    let words = parse_word_only_shell_command(command)?;
    detect_move_path_intercept_from_words(&words)
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

fn detect_list_files_intercept_from_words(words: &[String]) -> Option<InterceptedListFilesRequest> {
    let command_idx = command_start_word_index(words)?;
    let command = words.get(command_idx)?;
    if is_shell_command(command) {
        let nested_idx = shell_command_arg_word_index(words, command_idx)?;
        let nested_command = words.get(nested_idx)?;
        return detect_list_files_intercept(nested_command);
    }

    match command_basename(command).as_str() {
        "find" => parse_find_list_files_args(&words[command_idx + 1..]),
        "tree" => parse_tree_list_files_args(&words[command_idx + 1..]),
        "ls" => parse_ls_list_files_args(&words[command_idx + 1..]),
        "rg" => parse_rg_list_files_args(&words[command_idx + 1..]),
        _ => None,
    }
}

fn detect_move_path_intercept_from_words(words: &[String]) -> Option<InterceptedMovePathRequest> {
    let command_idx = command_start_word_index(words)?;
    let command = words.get(command_idx)?;
    if is_shell_command(command) {
        let nested_idx = shell_command_arg_word_index(words, command_idx)?;
        let nested_command = words.get(nested_idx)?;
        return detect_move_path_intercept(nested_command);
    }

    match command_basename(command).as_str() {
        "mv" => parse_mv_move_path_args(&words[command_idx + 1..]),
        _ => None,
    }
}

fn command_start_word_index(words: &[String]) -> Option<usize> {
    let mut idx = 0usize;
    while idx < words.len() && looks_like_env_assignment(&words[idx]) {
        idx += 1;
    }
    loop {
        let word = words.get(idx)?;
        let lower = word.to_ascii_lowercase();
        match lower.as_str() {
            "sudo" => {
                idx += 1;
                while idx < words.len() && words[idx].starts_with('-') {
                    idx += 1;
                }
            }
            "env" => {
                idx += 1;
                while idx < words.len()
                    && (words[idx].starts_with('-') || looks_like_env_assignment(&words[idx]))
                {
                    idx += 1;
                }
            }
            _ => break,
        }
    }
    words.get(idx).map(|_| idx)
}

fn shell_command_arg_word_index(words: &[String], shell_idx: usize) -> Option<usize> {
    let mut idx = shell_idx + 1;
    while idx < words.len() {
        let word = words[idx].to_ascii_lowercase();
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

fn parse_find_list_files_args(args: &[String]) -> Option<InterceptedListFilesRequest> {
    let (path, remainder) = match args.first() {
        Some(arg) if !is_find_expression_token(arg) => (Some(arg.clone()), &args[1..]),
        _ => (None, args),
    };

    let filter = match remainder {
        [] => FileListingFilter::All,
        [flag, kind] if flag == "-type" => match kind.as_str() {
            "f" => FileListingFilter::FilesOnly,
            "d" => FileListingFilter::DirectoriesOnly,
            _ => return None,
        },
        _ => return None,
    };

    Some(InterceptedListFilesRequest {
        path,
        include_hidden: true,
        filter,
    })
}

fn parse_tree_list_files_args(args: &[String]) -> Option<InterceptedListFilesRequest> {
    let mut path = None;
    let mut include_hidden = false;

    for arg in args {
        match arg.as_str() {
            "-a" | "--all" => include_hidden = true,
            value if value.starts_with('-') => return None,
            value => {
                if path.is_some() {
                    return None;
                }
                path = Some(value.to_string());
            }
        }
    }

    Some(InterceptedListFilesRequest {
        path,
        include_hidden,
        filter: FileListingFilter::All,
    })
}

fn parse_ls_list_files_args(args: &[String]) -> Option<InterceptedListFilesRequest> {
    let mut path = None;
    let mut include_hidden = false;
    let mut recursive = false;

    for arg in args {
        match arg.as_str() {
            "--recursive" => recursive = true,
            "--all" | "--almost-all" => include_hidden = true,
            value if value.starts_with("--") => return None,
            value if value.starts_with('-') => {
                for ch in value[1..].chars() {
                    match ch {
                        'R' => recursive = true,
                        'a' | 'A' => include_hidden = true,
                        _ => return None,
                    }
                }
            }
            value => {
                if path.is_some() {
                    return None;
                }
                path = Some(value.to_string());
            }
        }
    }

    if !recursive {
        return None;
    }

    Some(InterceptedListFilesRequest {
        path,
        include_hidden,
        filter: FileListingFilter::All,
    })
}

fn parse_rg_list_files_args(args: &[String]) -> Option<InterceptedListFilesRequest> {
    let mut path = None;
    let mut include_hidden = false;
    let mut files_only = false;
    let mut treat_next_as_path = false;

    for arg in args {
        if treat_next_as_path {
            if path.is_some() {
                return None;
            }
            path = Some(arg.clone());
            treat_next_as_path = false;
            continue;
        }

        match arg.as_str() {
            "--files" => files_only = true,
            "--hidden" => include_hidden = true,
            "--" => treat_next_as_path = true,
            value if value.starts_with('-') => return None,
            value => {
                if path.is_some() {
                    return None;
                }
                path = Some(value.to_string());
            }
        }
    }

    if !files_only || treat_next_as_path {
        return None;
    }

    Some(InterceptedListFilesRequest {
        path,
        include_hidden,
        filter: FileListingFilter::FilesOnly,
    })
}

fn parse_mv_move_path_args(args: &[String]) -> Option<InterceptedMovePathRequest> {
    let mut operands: Vec<String> = Vec::new();
    let mut overwrite = true;
    let mut parse_options = true;

    for arg in args {
        if parse_options && arg == "--" {
            parse_options = false;
            continue;
        }

        if parse_options && arg.starts_with("--") {
            match arg.as_str() {
                "--force" => overwrite = true,
                "--no-clobber" => overwrite = false,
                _ => return None,
            }
            continue;
        }

        if parse_options && arg.starts_with('-') && arg != "-" {
            for ch in arg[1..].chars() {
                match ch {
                    'f' => overwrite = true,
                    'n' => overwrite = false,
                    _ => return None,
                }
            }
            continue;
        }

        operands.push(arg.clone());
    }

    match operands.as_slice() {
        [from, to] => Some(InterceptedMovePathRequest {
            from: from.clone(),
            to: to.clone(),
            overwrite,
        }),
        _ => None,
    }
}

fn is_find_expression_token(word: &str) -> bool {
    word.starts_with('-') || matches!(word, "!" | "(" | ")")
}

fn command_basename(word: &str) -> String {
    word.rsplit('/').next().unwrap_or(word).to_ascii_lowercase()
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

fn parse_word_only_shell_command(command: &str) -> Option<Vec<String>> {
    let tree = parse_shell(command)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }

    const ALLOWED_KINDS: &[&str] = &[
        "program",
        "command",
        "command_name",
        "word",
        "string",
        "string_content",
        "raw_string",
        "number",
        "concatenation",
    ];
    const ALLOWED_PUNCTUATION: &[&str] = &["\"", "'"];

    let mut stack = vec![root];
    let mut command_node = None;
    while let Some(node) = stack.pop() {
        if node.is_named() {
            if !ALLOWED_KINDS.contains(&node.kind()) {
                return None;
            }
            if node.kind() == "command" {
                if command_node.is_some() {
                    return None;
                }
                command_node = Some(node);
            }
        } else if !(node.kind().trim().is_empty() || ALLOWED_PUNCTUATION.contains(&node.kind())) {
            return None;
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    parse_plain_command_from_node(command_node?, command)
}

fn parse_shell(command: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser.set_language(&BASH_LANGUAGE.into()).ok()?;
    parser.parse(command, None)
}

fn parse_plain_command_from_node(node: Node<'_>, src: &str) -> Option<Vec<String>> {
    if node.kind() != "command" {
        return None;
    }

    let mut words = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "command_name" => {
                let word_node = child.named_child(0)?;
                if !matches!(word_node.kind(), "word" | "number") {
                    return None;
                }
                words.push(word_node.utf8_text(src.as_bytes()).ok()?.to_owned());
            }
            "word" | "number" => {
                words.push(child.utf8_text(src.as_bytes()).ok()?.to_owned());
            }
            "string" => words.push(parse_double_quoted_string(child, src)?),
            "raw_string" => words.push(parse_raw_string(child, src)?),
            "concatenation" => {
                let mut combined = String::new();
                let mut concat_cursor = child.walk();
                for part in child.named_children(&mut concat_cursor) {
                    match part.kind() {
                        "word" | "number" => {
                            combined.push_str(part.utf8_text(src.as_bytes()).ok()?);
                        }
                        "string" => combined.push_str(&parse_double_quoted_string(part, src)?),
                        "raw_string" => combined.push_str(&parse_raw_string(part, src)?),
                        _ => return None,
                    }
                }
                if combined.is_empty() {
                    return None;
                }
                words.push(combined);
            }
            _ => return None,
        }
    }

    Some(words)
}

fn parse_double_quoted_string(node: Node<'_>, src: &str) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "string_content" {
            return None;
        }
    }

    node.utf8_text(src.as_bytes())
        .ok()?
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .map(str::to_owned)
}

fn parse_raw_string(node: Node<'_>, src: &str) -> Option<String> {
    if node.kind() != "raw_string" {
        return None;
    }

    node.utf8_text(src.as_bytes())
        .ok()?
        .strip_prefix('\'')
        .and_then(|text| text.strip_suffix('\''))
        .map(str::to_owned)
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

    #[test]
    fn detect_list_files_intercept_for_plain_find_command() {
        assert_eq!(
            detect_list_files_intercept("find src"),
            Some(InterceptedListFilesRequest {
                path: Some("src".into()),
                include_hidden: true,
                filter: FileListingFilter::All,
            })
        );
    }

    #[test]
    fn detect_list_files_intercept_for_nested_shell_rg_files() {
        assert_eq!(
            detect_list_files_intercept("bash -lc 'rg --files --hidden src'"),
            Some(InterceptedListFilesRequest {
                path: Some("src".into()),
                include_hidden: true,
                filter: FileListingFilter::FilesOnly,
            })
        );
    }

    #[test]
    fn detect_list_files_intercept_for_ls_recursive() {
        assert_eq!(
            detect_list_files_intercept("ls -Ra src"),
            Some(InterceptedListFilesRequest {
                path: Some("src".into()),
                include_hidden: true,
                filter: FileListingFilter::All,
            })
        );
    }

    #[test]
    fn detect_list_files_intercept_rejects_complex_find_expression() {
        assert_eq!(detect_list_files_intercept("find . -name '*.rs'"), None);
    }

    #[test]
    fn detect_move_path_intercept_for_plain_mv_command() {
        assert_eq!(
            detect_move_path_intercept("mv src/old.txt src/new.txt"),
            Some(InterceptedMovePathRequest {
                from: "src/old.txt".into(),
                to: "src/new.txt".into(),
                overwrite: true,
            })
        );
    }

    #[test]
    fn detect_move_path_intercept_for_nested_no_clobber_mv_command() {
        assert_eq!(
            detect_move_path_intercept("bash -lc 'mv -n src/old.txt src/new.txt'"),
            Some(InterceptedMovePathRequest {
                from: "src/old.txt".into(),
                to: "src/new.txt".into(),
                overwrite: false,
            })
        );
    }

    #[test]
    fn detect_move_path_intercept_rejects_multi_source_or_unsupported_flags() {
        assert_eq!(detect_move_path_intercept("mv a b c"), None);
        assert_eq!(detect_move_path_intercept("mv -r a b"), None);
    }
}
