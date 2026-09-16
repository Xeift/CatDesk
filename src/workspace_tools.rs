use crate::command;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

/// Per-file cap; MAX_READ_BATCH_BYTES caps the whole batch.
const MAX_READ_BYTES: usize = 512 * 1024;
pub const MAX_READ_BATCH_FILES: usize = 32;
pub const MAX_READ_BATCH_BYTES: usize = 512 * 1024;
const MAX_WRITE_BYTES: usize = 512 * 1024;
const DEFAULT_LIST_LIMIT: usize = 200;
const HARD_LIST_LIMIT: usize = 1000;
const DEFAULT_SEARCH_LIMIT: usize = 100;
const HARD_SEARCH_LIMIT: usize = 500;
const HARD_SEARCH_CONTEXT_LINES: usize = 20;

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

struct ReadFileOutput {
    path: String,
    size_bytes: u64,
    line_count: usize,
    text: String,
    truncated: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTextEntry {
    pub path: String,
    pub line: usize,
    pub text: String,
    pub is_context: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTextOutput {
    pub pattern: String,
    pub path: String,
    pub backend: String,
    pub backend_note: String,
    /// Set when the pattern could not compile as a regex and was searched as a
    /// literal instead. Empty otherwise.
    pub pattern_note: String,
    pub match_count: usize,
    pub truncated: bool,
    pub limit: usize,
    pub results: Vec<SearchTextEntry>,
}

impl SearchTextOutput {
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("pattern: {}\n", self.pattern));
        out.push_str(&format!("path: {}\n", self.path));
        out.push_str(&format!("backend: {}\n", self.backend));
        if !self.backend_note.is_empty() {
            out.push_str(&format!("backend_note: {}\n", self.backend_note));
        }
        if !self.pattern_note.is_empty() {
            out.push_str(&format!("pattern_note: {}\n", self.pattern_note));
        }
        out.push_str(&format!("matches: {}\n\n", self.match_count));
        out.push_str(
            &self
                .results
                .iter()
                .map(|entry| {
                    let separator = if entry.is_context { "-" } else { ":" };
                    format!(
                        "{}{}{}{} {}",
                        entry.path, separator, entry.line, separator, entry.text
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
        if self.truncated {
            out.push_str(&format!("\n\n[truncated at {} matches]", self.limit));
        }
        out
    }
}

pub struct SearchTextOptions<'a> {
    pub pattern: &'a str,
    pub path: Option<&'a str>,
    pub glob: Option<&'a str>,
    pub fixed_strings: bool,
    pub case_insensitive: bool,
    pub context: Option<usize>,
    pub before: Option<usize>,
    pub after: Option<usize>,
    pub max_matches: Option<usize>,
    pub max_matches_per_file: Option<usize>,
    pub include_hidden: bool,
    pub no_ignore: bool,
}

#[derive(Clone, Copy)]
struct ResolvedSearchTextOptions<'a> {
    pattern: &'a str,
    glob: Option<&'a str>,
    fixed_strings: bool,
    case_insensitive: bool,
    before: usize,
    after: usize,
    max_matches: usize,
    max_matches_per_file: Option<usize>,
    include_hidden: bool,
    no_ignore: bool,
}

enum SearchBackendError {
    Unavailable,
    Failed(String),
}

enum SearchMatcher {
    Fixed {
        pattern: String,
        case_insensitive: bool,
    },
    Regex(Regex),
}

impl SearchMatcher {
    fn new(options: ResolvedSearchTextOptions<'_>) -> Result<Self, String> {
        if options.fixed_strings {
            let pattern = if options.case_insensitive {
                options.pattern.to_lowercase()
            } else {
                options.pattern.to_string()
            };
            return Ok(Self::Fixed {
                pattern,
                case_insensitive: options.case_insensitive,
            });
        }

        let regex = RegexBuilder::new(options.pattern)
            .case_insensitive(options.case_insensitive)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self::Regex(regex))
    }

    fn is_match(&self, line: &str) -> bool {
        match self {
            Self::Fixed {
                pattern,
                case_insensitive,
            } => {
                if *case_insensitive {
                    line.to_lowercase().contains(pattern)
                } else {
                    line.contains(pattern)
                }
            }
            Self::Regex(regex) => regex.is_match(line),
        }
    }
}

fn workspace_root_path(workspace_root: &str) -> Result<PathBuf, String> {
    Path::new(workspace_root)
        .canonicalize()
        .map(command::normalize_windows_verbatim_path)
        .map_err(|e| e.to_string())
}

fn to_workspace_relative(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => tool_path_string(rel),
        Err(_) => tool_path_string(path),
    }
}

fn tool_path_string(path: &Path) -> String {
    let path = path.display().to_string();
    #[cfg(windows)]
    {
        path.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        path
    }
}

fn safe_limit(value: Option<usize>, default_value: usize, hard_max: usize) -> usize {
    value.unwrap_or(default_value).clamp(1, hard_max)
}

fn resolve_target_path(workspace_root: &str, path: &str) -> Result<PathBuf, String> {
    command::resolve_workspace_path(workspace_root, Some(path))
}

/// Planning and the read share this check so an unreadable path fails the same
/// way from either one, rather than with whatever the open happened to return.
fn readable_size(target: &Path) -> Result<u64, String> {
    let metadata = fs::metadata(target).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("File not found: {}", target.display()),
        _ => format!("{e}: {}", target.display()),
    })?;
    if !metadata.is_file() {
        return Err(format!("Not a file: {}", target.display()));
    }
    Ok(metadata.len())
}

fn read_file(workspace_root: &str, path: &str, budget: usize) -> Result<ReadFileOutput, String> {
    let root = workspace_root_path(workspace_root)?;
    let target = resolve_target_path(workspace_root, path)?;
    let size_bytes = readable_size(&target)?;
    let mut file =
        fs::File::open(&target).map_err(|error| format!("{error}: {}", target.display()))?;
    // A single read() may come back short on a network filesystem, and
    // line_count is derived from what was read.
    // Whatever the batch cannot keep would be read and then thrown away, so
    // the read stops at the budget. The open still happens, which is what lets
    // a file that cannot be read report its own error instead of a budget cut.
    let cap = budget.min(MAX_READ_BYTES);
    let mut buf = Vec::with_capacity(cap + 1);
    let read_n = file
        .by_ref()
        .take((cap + 1) as u64)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    let mut truncated = read_n > cap;
    let data = &buf[..read_n.min(cap)];
    let mut text = String::from_utf8_lossy(data).into_owned();
    // from_utf8_lossy expands one invalid byte into three, so the text can
    // exceed the cap even when the file did not. Cutting it here keeps that a
    // per-file limit rather than something the batch blames on its budget.
    if text.len() > MAX_READ_BYTES {
        let keep = floor_char_boundary(&text, MAX_READ_BYTES);
        text.truncate(keep);
        truncated = true;
    }
    // Over what was read, not the whole file: a full scan costs the entire
    // file in disk reads to return at most MAX_READ_BYTES.
    let line_count = count_lines(&text);

    Ok(ReadFileOutput {
        path: to_workspace_relative(&root, &target),
        size_bytes,
        line_count,
        text,
        truncated,
    })
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadBatchEntry {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub bytes: usize,
    pub size_bytes: u64,
    pub line_count: usize,
    pub text: String,
    pub truncated: bool,
    /// This entry was cut by the shared budget, so asking for fewer files
    /// returns more of it. A file over the per-file cap sets `truncated`
    /// without this: no retry returns the rest.
    pub budget_truncated: bool,
}

pub struct ReadBatchOutput {
    pub files: Vec<ReadBatchEntry>,
    pub total_bytes: usize,
    pub total_line_count: usize,
    /// The shared budget cut something short, so a smaller retry returns more.
    /// A file over the per-file cap does not set this: no retry helps.
    pub batch_truncated: bool,
}

fn floor_char_boundary(text: &str, max: usize) -> usize {
    if max >= text.len() {
        return text.len();
    }
    let mut index = max;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// One path form for every entry, readable or not.
fn entry_path(root: &Path, planned: &PlannedRead, requested: &str) -> String {
    planned
        .target
        .as_ref()
        .map(|target| to_workspace_relative(root, target))
        .unwrap_or_else(|| requested.to_string())
}

fn failed_entry(path: &str, error: String) -> ReadBatchEntry {
    ReadBatchEntry {
        path: path.to_string(),
        error: Some(error),
        bytes: 0,
        size_bytes: 0,
        line_count: 0,
        text: String::new(),
        truncated: false,
        budget_truncated: false,
    }
}

struct PlannedRead {
    size_bytes: u64,
    error: Option<String>,
    target: Option<PathBuf>,
}

fn plan_read(workspace_root: &str, path: &str) -> PlannedRead {
    let resolved = resolve_target_path(workspace_root, path);
    let target = resolved.as_ref().ok().cloned();
    let size_bytes = resolved.and_then(|target| readable_size(&target));
    match size_bytes {
        Ok(size_bytes) => PlannedRead {
            size_bytes,
            error: None,
            target,
        },
        Err(error) => PlannedRead {
            size_bytes: 0,
            error: Some(error),
            target,
        },
    }
}

/// Smallest on disk first. Lossy expansion can still make a small file
/// spend more than its size, so this is a heuristic, not a guarantee.
fn read_order(planned: &[PlannedRead]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..planned.len())
        .filter(|index| planned[*index].error.is_none())
        .collect();
    order.sort_by_key(|index| planned[*index].size_bytes);
    order
}

/// Entries come back in the order requested, whatever order they were read in.
pub fn read_files(workspace_root: &str, paths: &[String]) -> Result<ReadBatchOutput, String> {
    if paths.is_empty() {
        return Err("paths must contain at least one path".into());
    }
    if paths.len() > MAX_READ_BATCH_FILES {
        return Err(format!(
            "paths must contain at most {MAX_READ_BATCH_FILES} entries (got {})",
            paths.len()
        ));
    }
    let root = workspace_root_path(workspace_root)?;

    let planned: Vec<PlannedRead> = paths
        .iter()
        .map(|path| plan_read(workspace_root, path))
        .collect();

    let mut entries: Vec<Option<ReadBatchEntry>> = (0..paths.len()).map(|_| None).collect();
    let mut remaining = MAX_READ_BATCH_BYTES;
    let mut batch_truncated = false;
    let mut total_bytes = 0_usize;
    let mut total_line_count = 0_usize;

    // A symlink and its target canonicalize to the same place, as does the
    // same string twice. Reading it twice would charge the budget twice.
    let mut already_read: HashSet<PathBuf> = HashSet::new();

    for index in read_order(&planned) {
        let path = &paths[index];
        if let Some(target) = &planned[index].target
            && already_read.contains(target)
        {
            continue;
        }
        match read_file(workspace_root, path, remaining) {
            Ok(output) => {
                let keep = floor_char_boundary(&output.text, remaining);
                let text_cut = keep < output.text.len();
                // Now that the read stops at the budget, how much came back no
                // longer says which limit stopped it. The budget is the binding
                // one whenever it is tighter than the per-file cap.
                let budget_cut = text_cut || (output.truncated && remaining < MAX_READ_BYTES);
                let truncated = output.truncated || text_cut;
                batch_truncated |= budget_cut;
                let mut text = output.text;
                text.truncate(keep);
                let line_count = if text_cut {
                    count_lines(&text)
                } else {
                    output.line_count
                };
                remaining -= keep;
                total_bytes += keep;
                total_line_count += line_count;
                if let Some(target) = &planned[index].target {
                    already_read.insert(target.clone());
                }
                entries[index] = Some(ReadBatchEntry {
                    path: output.path,
                    error: None,
                    bytes: keep,
                    size_bytes: output.size_bytes,
                    line_count,
                    text,
                    truncated,
                    budget_truncated: budget_cut,
                });
            }
            Err(error) => {
                entries[index] = Some(failed_entry(
                    &entry_path(&root, &planned[index], path),
                    error,
                ))
            }
        }
    }

    let files: Vec<ReadBatchEntry> = entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| match (entry, &planned[index].error) {
            (Some(entry), _) => Some(entry),
            (None, Some(error)) => Some(failed_entry(
                &entry_path(&root, &planned[index], &paths[index]),
                error.clone(),
            )),
            // Deduplicated: an earlier path named the same file.
            (None, None) => None,
        })
        .collect();

    Ok(ReadBatchOutput {
        files,
        total_bytes,
        total_line_count,
        batch_truncated,
    })
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let newlines = text.bytes().filter(|byte| *byte == b'\n').count();
    if text.ends_with('\n') {
        newlines
    } else {
        newlines + 1
    }
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

pub fn list_files_filtered(
    workspace_root: &str,
    path: Option<&str>,
    include_hidden: bool,
    limit: Option<usize>,
    filter: command::FileListingFilter,
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
                if matches!(
                    filter,
                    command::FileListingFilter::All | command::FileListingFilter::DirectoriesOnly
                ) {
                    directory_count += 1;
                    entries.push(ListFilesEntry {
                        path: rel,
                        name,
                        kind: "dir".into(),
                        depth,
                    });
                }
                queue.push_back(path);
            } else if ft.is_file() {
                if matches!(
                    filter,
                    command::FileListingFilter::All | command::FileListingFilter::FilesOnly
                ) {
                    file_count += 1;
                    entries.push(ListFilesEntry {
                        path: rel,
                        name,
                        kind: "file".into(),
                        depth,
                    });
                }
            } else {
                if matches!(filter, command::FileListingFilter::All) {
                    other_count += 1;
                    entries.push(ListFilesEntry {
                        path: rel,
                        name,
                        kind: "other".into(),
                        depth,
                    });
                }
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
    options: SearchTextOptions<'_>,
) -> Result<SearchTextOutput, String> {
    if options.pattern.trim().is_empty() {
        return Err("Pattern must not be empty".into());
    }

    let root = workspace_root_path(workspace_root)?;
    let start = command::resolve_workspace_path(workspace_root, options.path)?;
    if !start.exists() {
        return Err(format!("Path not found: {}", start.display()));
    }
    if !start.is_dir() && !start.is_file() {
        return Err(format!("Not a file or directory: {}", start.display()));
    }

    validate_search_context(options.context, "context")?;
    validate_search_context(options.before, "before")?;
    validate_search_context(options.after, "after")?;
    let max_matches =
        validate_search_limit(options.max_matches, "max_matches", DEFAULT_SEARCH_LIMIT)?;
    let max_matches_per_file =
        validate_optional_search_limit(options.max_matches_per_file, "max_matches_per_file")?;
    let (before, after) = match options.context {
        Some(value) => (value, value),
        None => (options.before.unwrap_or(0), options.after.unwrap_or(0)),
    };
    let resolved = ResolvedSearchTextOptions {
        pattern: options.pattern,
        glob: options.glob.filter(|value| !value.trim().is_empty()),
        fixed_strings: options.fixed_strings,
        case_insensitive: options.case_insensitive,
        before,
        after,
        max_matches,
        max_matches_per_file,
        include_hidden: options.include_hidden,
        no_ignore: options.no_ignore,
    };

    let rejected = match search_with_backend(&root, &start, resolved) {
        Ok(output) => return Ok(output),
        Err(error) => error,
    };
    if resolved.fixed_strings {
        return Err(rejected);
    }

    // Searching for a call site is written `foo(`, which no backend accepts as
    // a regex. Handing that back costs a round trip to learn what the pattern
    // already said, so the search is retried as a literal.
    //
    // Retried rather than pre-checked on purpose: each backend has its own
    // dialect -- ripgrep searches bytes and takes expressions the regex crate
    // rejects, grep is POSIX ERE -- so the only reliable test of whether a
    // pattern works is whether the backend that will run it accepted it.
    let mut literal = resolved;
    literal.fixed_strings = true;
    match search_with_backend(&root, &start, literal) {
        // The note quotes the failure rather than calling it a bad pattern: a
        // backend can also fail to start or to produce readable output, and
        // this retry cannot tell those apart from a rejected expression.
        Ok(mut output) => {
            output.pattern_note = format!("regex search failed ({rejected}); searched literally");
            Ok(output)
        }
        // Nothing was learned from the retry, so report the attempt that was
        // asked for rather than the consolation one.
        Err(_) => Err(rejected),
    }
}

fn search_with_backend(
    root: &Path,
    start: &Path,
    resolved: ResolvedSearchTextOptions<'_>,
) -> Result<SearchTextOutput, String> {
    if command_available("rg") {
        return search_text_rg(root, start, resolved).map_err(|e| match e {
            SearchBackendError::Unavailable => "ripgrep disappeared while running search".into(),
            SearchBackendError::Failed(message) => message,
        });
    }
    if command_available("grep") {
        match search_text_grep(root, start, resolved) {
            Ok(output) => return Ok(output),
            Err(SearchBackendError::Unavailable) => {}
            Err(SearchBackendError::Failed(message)) => {
                return search_text_rust(
                    root,
                    start,
                    resolved,
                    format!("rg not found; grep failed ({message}); used built-in search"),
                );
            }
        }
    }

    search_text_rust(
        root,
        start,
        resolved,
        "rg and grep not found; used built-in search".into(),
    )
}

fn command_available(program: &str) -> bool {
    match ProcessCommand::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => false,
    }
}

fn search_text_rg(
    root: &Path,
    start: &Path,
    options: ResolvedSearchTextOptions<'_>,
) -> Result<SearchTextOutput, SearchBackendError> {
    let mut command = ProcessCommand::new("rg");
    command
        .current_dir(&root)
        .arg("--json")
        .arg("--line-number")
        .arg("--with-filename")
        .arg("--no-heading")
        .arg("--no-messages")
        .arg("--color")
        .arg("never");
    if options.fixed_strings {
        command.arg("--fixed-strings");
    }
    if options.case_insensitive {
        command.arg("--ignore-case");
    }
    if options.include_hidden {
        command.arg("--hidden");
    }
    if options.no_ignore {
        command.arg("--no-ignore");
    }
    if let Some(glob) = options.glob {
        command.arg("--glob").arg(glob);
    }
    if let Some(value) = options.max_matches_per_file {
        command.arg("--max-count").arg(value.to_string());
    }
    if options.before == options.after && options.before > 0 {
        command.arg("--context").arg(options.before.to_string());
    } else {
        if options.before > 0 {
            command
                .arg("--before-context")
                .arg(options.before.to_string());
        }
        if options.after > 0 {
            command
                .arg("--after-context")
                .arg(options.after.to_string());
        }
    }
    let include_context = options.before > 0 || options.after > 0;
    command.arg("--regexp").arg(options.pattern).arg(&start);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            SearchBackendError::Unavailable
        } else {
            SearchBackendError::Failed(e.to_string())
        }
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SearchBackendError::Failed("Failed to capture ripgrep stdout".into()))?;
    let mut reader = BufReader::new(stdout);
    let mut results: Vec<SearchTextEntry> = Vec::new();
    let mut returned_matches = 0_usize;
    let mut truncated = false;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader
            .read_line(&mut line)
            .map_err(|e| SearchBackendError::Failed(e.to_string()))?;
        if bytes_read == 0 {
            break;
        }
        let event: Value = serde_json::from_str(line.trim_end()).map_err(|e| {
            SearchBackendError::Failed(format!("Failed to parse ripgrep JSON output: {e}"))
        })?;
        let event_type = event.get("type").and_then(Value::as_str);
        match event_type {
            Some("match") => {
                if returned_matches >= options.max_matches {
                    truncated = true;
                    let _ = child.kill();
                    break;
                }
                results.push(
                    parse_rg_search_entry(root, &event, false)
                        .map_err(SearchBackendError::Failed)?,
                );
                returned_matches += 1;
            }
            Some("context") if include_context => {
                results.push(
                    parse_rg_search_entry(root, &event, true)
                        .map_err(SearchBackendError::Failed)?,
                );
            }
            _ => {}
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| SearchBackendError::Failed(e.to_string()))?;
    let status_code = output.status.code().unwrap_or(2);
    if !truncated && status_code != 0 && status_code != 1 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        return Err(SearchBackendError::Failed(if message.is_empty() {
            format!("ripgrep exited with status {status_code}")
        } else {
            message.to_string()
        }));
    }

    Ok(SearchTextOutput {
        pattern: options.pattern.to_string(),
        path: to_workspace_relative(root, start),
        backend: "rg".into(),
        backend_note: String::new(),
        pattern_note: String::new(),
        match_count: returned_matches,
        truncated,
        limit: options.max_matches,
        results,
    })
}

fn search_text_grep(
    root: &Path,
    start: &Path,
    options: ResolvedSearchTextOptions<'_>,
) -> Result<SearchTextOutput, SearchBackendError> {
    let files = collect_search_files(root, start, options)
        .map_err(|e| SearchBackendError::Failed(e.to_string()))?;
    let mut results = Vec::new();
    let mut returned_matches = 0_usize;
    let mut truncated = false;

    for file in files.iter() {
        let remaining_matches = options.max_matches.saturating_sub(returned_matches);
        let probe_limit = remaining_matches.saturating_add(1);
        let file_match_limit = options
            .max_matches_per_file
            .map(|value| value.min(probe_limit))
            .unwrap_or(probe_limit);
        let mut command = ProcessCommand::new("grep");
        command
            .current_dir(root)
            .arg("-n")
            .arg("-I")
            .arg("-m")
            .arg(file_match_limit.to_string());
        if options.fixed_strings {
            command.arg("-F");
        } else {
            command.arg("-E");
        }
        if options.case_insensitive {
            command.arg("-i");
        }
        if options.before == options.after && options.before > 0 {
            command.arg("-C").arg(options.before.to_string());
        } else {
            if options.before > 0 {
                command.arg("-B").arg(options.before.to_string());
            }
            if options.after > 0 {
                command.arg("-A").arg(options.after.to_string());
            }
        }
        command.arg("--").arg(options.pattern).arg(file);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = command.output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SearchBackendError::Unavailable
            } else {
                SearchBackendError::Failed(e.to_string())
            }
        })?;
        let status_code = output.status.code().unwrap_or(2);
        if status_code != 0 && status_code != 1 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = stderr.trim();
            return Err(SearchBackendError::Failed(if message.is_empty() {
                format!("grep exited with status {status_code}")
            } else {
                message.to_string()
            }));
        }

        let mut file_matches = 0_usize;
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line == "--" {
                continue;
            }
            let Some(entry) = parse_grep_search_entry(root, file, line) else {
                continue;
            };
            if !entry.is_context {
                if returned_matches >= options.max_matches {
                    truncated = true;
                    break;
                }
                if let Some(max_per_file) = options.max_matches_per_file {
                    if file_matches >= max_per_file {
                        continue;
                    }
                }
                file_matches += 1;
                returned_matches += 1;
            }
            results.push(entry);
        }
        if truncated {
            break;
        }
    }

    Ok(SearchTextOutput {
        pattern: options.pattern.to_string(),
        path: to_workspace_relative(root, start),
        backend: "grep".into(),
        backend_note: "rg not found; used grep".into(),
        pattern_note: String::new(),
        match_count: returned_matches,
        truncated,
        limit: options.max_matches,
        results,
    })
}

fn search_text_rust(
    root: &Path,
    start: &Path,
    options: ResolvedSearchTextOptions<'_>,
    backend_note: String,
) -> Result<SearchTextOutput, String> {
    let matcher = SearchMatcher::new(options)?;
    let files = collect_search_files(root, start, options)?;
    let mut results = Vec::new();
    let mut returned_matches = 0_usize;
    let mut truncated = false;

    for file in files.iter() {
        if returned_matches >= options.max_matches {
            truncated = true;
            break;
        }
        let data = fs::read(file).map_err(|e| e.to_string())?;
        if data.iter().any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&data);
        let lines: Vec<&str> = text.lines().collect();
        let mut match_indexes = Vec::new();
        for (idx, line) in lines.iter().enumerate() {
            if !matcher.is_match(line) {
                continue;
            }
            if let Some(max_per_file) = options.max_matches_per_file {
                if match_indexes.len() >= max_per_file {
                    break;
                }
            }
            if returned_matches + match_indexes.len() >= options.max_matches {
                truncated = true;
                break;
            }
            match_indexes.push(idx);
        }
        append_rust_search_entries(
            root,
            file,
            &lines,
            &match_indexes,
            options.before,
            options.after,
            &mut results,
        );
        returned_matches += match_indexes.len();
    }

    Ok(SearchTextOutput {
        pattern: options.pattern.to_string(),
        path: to_workspace_relative(root, start),
        backend: "rust".into(),
        backend_note,
        pattern_note: String::new(),
        match_count: returned_matches,
        truncated,
        limit: options.max_matches,
        results,
    })
}

fn validate_search_limit(
    value: Option<usize>,
    name: &str,
    default_value: usize,
) -> Result<usize, String> {
    match value {
        Some(value) if value == 0 || value > HARD_SEARCH_LIMIT => {
            Err(format!("{name} must be between 1 and {HARD_SEARCH_LIMIT}"))
        }
        Some(value) => Ok(value),
        None => Ok(default_value),
    }
}

fn validate_optional_search_limit(
    value: Option<usize>,
    name: &str,
) -> Result<Option<usize>, String> {
    match value {
        Some(value) if value == 0 || value > HARD_SEARCH_LIMIT => {
            Err(format!("{name} must be between 1 and {HARD_SEARCH_LIMIT}"))
        }
        Some(value) => Ok(Some(value)),
        None => Ok(None),
    }
}

fn validate_search_context(value: Option<usize>, name: &str) -> Result<(), String> {
    if let Some(value) = value {
        if value > HARD_SEARCH_CONTEXT_LINES {
            return Err(format!(
                "{name} must be between 0 and {HARD_SEARCH_CONTEXT_LINES}"
            ));
        }
    }
    Ok(())
}

fn parse_rg_search_entry(
    root: &Path,
    event: &Value,
    is_context: bool,
) -> Result<SearchTextEntry, String> {
    let data = event
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| "ripgrep JSON event is missing data".to_string())?;
    let raw_path = data
        .get("path")
        .and_then(|path| path.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| "ripgrep JSON event is missing path text".to_string())?;
    let line = data
        .get("line_number")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| "ripgrep JSON event is missing line number".to_string())?;
    let text = data
        .get("lines")
        .and_then(|lines| lines.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| "ripgrep JSON event is missing line text".to_string())?
        .trim_end_matches(['\r', '\n'])
        .to_string();
    let path = PathBuf::from(raw_path);
    let absolute_path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };

    Ok(SearchTextEntry {
        path: to_workspace_relative(root, &absolute_path),
        line,
        text,
        is_context,
    })
}

fn parse_grep_search_entry(root: &Path, file: &Path, line: &str) -> Option<SearchTextEntry> {
    let (line_number, separator, text) = parse_grep_line_prefix(line)?;
    Some(SearchTextEntry {
        path: to_workspace_relative(root, file),
        line: line_number,
        text: text.to_string(),
        is_context: separator == '-',
    })
}

fn parse_grep_line_prefix(line: &str) -> Option<(usize, char, &str)> {
    let mut digits_end = 0_usize;
    for (idx, ch) in line.char_indices() {
        if ch.is_ascii_digit() {
            digits_end = idx + ch.len_utf8();
            continue;
        }
        if digits_end == 0 || (ch != ':' && ch != '-') {
            return None;
        }
        let line_number = line[..digits_end].parse::<usize>().ok()?;
        let text_start = idx + ch.len_utf8();
        return Some((line_number, ch, &line[text_start..]));
    }
    None
}

fn collect_search_files(
    root: &Path,
    start: &Path,
    options: ResolvedSearchTextOptions<'_>,
) -> Result<Vec<PathBuf>, String> {
    let glob = compile_search_glob(options.glob)?;
    let mut builder = WalkBuilder::new(start);
    builder
        .hidden(!options.include_hidden)
        .parents(!options.no_ignore)
        .ignore(!options.no_ignore)
        .git_global(!options.no_ignore)
        .git_ignore(!options.no_ignore)
        .git_exclude(!options.no_ignore);

    let mut files = Vec::new();
    for entry_result in builder.build() {
        let entry = entry_result.map_err(|e| e.to_string())?;
        let path = entry.path();
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        if let Some(glob) = glob.as_ref() {
            let rel = path.strip_prefix(root).unwrap_or(path);
            if !glob.is_match(rel) {
                continue;
            }
        }
        files.push(path.to_path_buf());
    }
    files.sort();
    Ok(files)
}

fn compile_search_glob(glob: Option<&str>) -> Result<Option<GlobSet>, String> {
    let Some(glob) = glob else {
        return Ok(None);
    };
    let mut builder = GlobSetBuilder::new();
    builder.add(Glob::new(glob).map_err(|e| e.to_string())?);
    if !glob.contains('/') && !glob.contains('\\') {
        builder.add(Glob::new(&format!("**/{glob}")).map_err(|e| e.to_string())?);
    }
    builder.build().map(Some).map_err(|e| e.to_string())
}

fn append_rust_search_entries(
    root: &Path,
    file: &Path,
    lines: &[&str],
    match_indexes: &[usize],
    before: usize,
    after: usize,
    results: &mut Vec<SearchTextEntry>,
) {
    let mut selected: BTreeMap<usize, bool> = BTreeMap::new();
    for match_idx in match_indexes {
        let start_idx = match_idx.saturating_sub(before);
        let end_idx = (*match_idx + after).min(lines.len().saturating_sub(1));
        for idx in start_idx..=end_idx {
            selected.entry(idx).or_insert(true);
        }
        selected.insert(*match_idx, false);
    }

    let rel = to_workspace_relative(root, file);
    for (idx, is_context) in selected {
        results.push(SearchTextEntry {
            path: rel.clone(),
            line: idx + 1,
            text: lines[idx].to_string(),
            is_context,
        });
    }
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

#[derive(Clone, Debug)]
pub enum EditOperation {
    Replace {
        old_string: String,
        new_string: String,
        replace_all: bool,
    },
    Range {
        start_line: usize,
        end_line: usize,
        old_text: String,
        new_text: String,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditFileOutput {
    pub path: String,
    pub operation_count: usize,
    pub applied_operations: usize,
    pub replaced_occurrences: usize,
    pub bytes_written: usize,
}

impl EditFileOutput {
    pub fn render_text(&self) -> String {
        format!(
            "applied {} edit operation(s) to {} ({} replacement occurrence(s), {} bytes)",
            self.applied_operations, self.path, self.replaced_occurrences, self.bytes_written
        )
    }
}

fn line_range_byte_bounds(
    content: &str,
    start_line: usize,
    end_line: usize,
) -> Result<(usize, usize), String> {
    if start_line == 0 || end_line == 0 {
        return Err("range line numbers are 1-based and must be at least 1".into());
    }
    if start_line > end_line {
        return Err(format!(
            "range start_line ({start_line}) must not exceed end_line ({end_line})"
        ));
    }

    let mut starts = if content.is_empty() {
        Vec::new()
    } else {
        vec![0_usize]
    };
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    if content.ends_with('\n') {
        starts.pop();
    }

    let line_count = starts.len();
    if start_line > line_count || end_line > line_count {
        return Err(format!(
            "range {start_line}-{end_line} exceeds file line count {line_count}"
        ));
    }

    let start = starts[start_line - 1];
    let end = if end_line < line_count {
        starts[end_line]
    } else {
        content.len()
    };
    Ok((start, end))
}

pub fn edit_file(
    workspace_root: &str,
    path: &str,
    operations: &[EditOperation],
) -> Result<EditFileOutput, String> {
    if operations.is_empty() {
        return Err("edits must contain at least one operation".into());
    }

    let root = workspace_root_path(workspace_root)?;
    let target = resolve_target_path(workspace_root, path)?;
    if !target.exists() {
        return Err(format!("File not found: {}", target.display()));
    }
    if !target.is_file() {
        return Err(format!("Not a file: {}", target.display()));
    }

    let original_content = fs::read_to_string(&target).map_err(|e| e.to_string())?;
    let mut content = original_content.clone();
    let mut replaced_occurrences = 0_usize;

    for (index, operation) in operations.iter().enumerate() {
        let operation_number = index + 1;
        match operation {
            EditOperation::Replace {
                old_string,
                new_string,
                replace_all,
            } => {
                if old_string.is_empty() {
                    return Err(format!(
                        "edit operation {operation_number}: old_string must not be empty"
                    ));
                }
                if old_string == new_string {
                    return Err(format!(
                        "edit operation {operation_number}: old_string and new_string must be different"
                    ));
                }

                let matched = content.matches(old_string).count();
                if matched == 0 {
                    return Err(format!(
                        "edit operation {operation_number}: old_string not found in {}",
                        to_workspace_relative(&root, &target)
                    ));
                }
                if matched > 1 && !replace_all {
                    return Err(format!(
                        "edit operation {operation_number}: old_string matched {} occurrences in {}. Set replace_all=true to replace every occurrence, or provide more context to make old_string unique.",
                        matched,
                        to_workspace_relative(&root, &target)
                    ));
                }

                content = if *replace_all {
                    content.replace(old_string, new_string)
                } else {
                    content.replacen(old_string, new_string, 1)
                };
                replaced_occurrences = replaced_occurrences.saturating_add(matched);
            }
            EditOperation::Range {
                start_line,
                end_line,
                old_text,
                new_text,
            } => {
                if old_text == new_text {
                    return Err(format!(
                        "edit operation {operation_number}: old_text and new_text must be different"
                    ));
                }
                let (start, end) = line_range_byte_bounds(&content, *start_line, *end_line)
                    .map_err(|error| format!("edit operation {operation_number}: {error}"))?;
                let selected = &content[start..end];
                if selected != old_text {
                    return Err(format!(
                        "edit operation {operation_number}: old_text does not match lines {}-{} in {}",
                        start_line,
                        end_line,
                        to_workspace_relative(&root, &target)
                    ));
                }

                content.replace_range(start..end, new_text);
                replaced_occurrences = replaced_occurrences.saturating_add(1);
            }
        }
    }

    let current_content = fs::read_to_string(&target).map_err(|e| e.to_string())?;
    if current_content != original_content {
        return Err(format!(
            "file changed during edit; refusing to overwrite {}",
            to_workspace_relative(&root, &target)
        ));
    }

    fs::write(&target, &content).map_err(|e| e.to_string())?;
    Ok(EditFileOutput {
        path: to_workspace_relative(&root, &target),
        operation_count: operations.len(),
        applied_operations: operations.len(),
        replaced_occurrences,
        bytes_written: content.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_workspace(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("catdesk-workspace-tools-{name}-{}", Uuid::new_v4()))
    }

    #[test]
    fn search_falls_back_to_a_literal_instead_of_rejecting_a_call_site_pattern() {
        let workspace_root = test_workspace("search-literal-fallback");
        fs::create_dir_all(&workspace_root).expect("create workspace");
        fs::write(
            workspace_root.join("a.rs"),
            "assertByteLength(x)\nassertByteLengthOther\n",
        )
        .expect("write source");
        let root = workspace_root.to_string_lossy().into_owned();

        // Both patterns are taken verbatim from a session that spent a round
        // trip on the rejection and then repeated the search with
        // fixed_strings.
        for pattern in ["assertByteLength(", "Into("] {
            let output = search_text(
                &root,
                SearchTextOptions {
                    pattern,
                    path: None,
                    glob: None,
                    fixed_strings: false,
                    case_insensitive: false,
                    context: None,
                    before: None,
                    after: None,
                    max_matches: None,
                    max_matches_per_file: None,
                    include_hidden: false,
                    no_ignore: false,
                },
            )
            .unwrap_or_else(|error| panic!("{pattern} must not be rejected: {error}"));
            assert!(
                output.pattern_note.contains("searched literally"),
                "{pattern} must say the results came from a literal search, got {:?}",
                output.pattern_note
            );
        }

        let output = search_text(
            &root,
            SearchTextOptions {
                pattern: "assertByteLength(",
                path: None,
                glob: None,
                fixed_strings: false,
                case_insensitive: false,
                context: None,
                before: None,
                after: None,
                max_matches: None,
                max_matches_per_file: None,
                include_hidden: false,
                no_ignore: false,
            },
        )
        .expect("search");
        assert_eq!(
            output
                .results
                .iter()
                .filter(|entry| !entry.is_context)
                .map(|entry| entry.line)
                .collect::<Vec<_>>(),
            vec![1],
            "the literal must still match the call site and not the bare name"
        );
        let _ = fs::remove_dir_all(&workspace_root);
    }

    #[test]
    fn search_does_not_downgrade_a_regex_its_backend_accepts() {
        // rg searches bytes and accepts expressions the regex crate rejects,
        // so pre-checking the pattern against that crate would turn a working
        // search into a literal one that finds nothing. Only rg can say.
        if !command_available("rg") {
            return;
        }
        let workspace_root = test_workspace("search-rg-dialect");
        fs::create_dir_all(&workspace_root).expect("create workspace");
        fs::write(workspace_root.join("a.rs"), "a b\n").expect("write source");

        let output = search_text(
            &workspace_root.to_string_lossy(),
            SearchTextOptions {
                pattern: r"(?-u:\W)",
                path: None,
                glob: None,
                fixed_strings: false,
                case_insensitive: false,
                context: None,
                before: None,
                after: None,
                max_matches: None,
                max_matches_per_file: None,
                include_hidden: false,
                no_ignore: false,
            },
        )
        .expect("rg accepts this pattern");

        assert_eq!(
            output.match_count, 1,
            "the space must match, which a literal search for the pattern text cannot do"
        );
        assert!(
            output.pattern_note.is_empty(),
            "nothing was downgraded, so there is nothing to report"
        );
        let _ = fs::remove_dir_all(&workspace_root);
    }

    #[test]
    fn search_leaves_a_valid_regex_alone() {
        let workspace_root = test_workspace("search-regex-untouched");
        fs::create_dir_all(&workspace_root).expect("create workspace");
        fs::write(workspace_root.join("a.rs"), "alpha1\nalpha2\n").expect("write source");

        let output = search_text(
            &workspace_root.to_string_lossy(),
            SearchTextOptions {
                pattern: "alpha[0-9]",
                path: None,
                glob: None,
                fixed_strings: false,
                case_insensitive: false,
                context: None,
                before: None,
                after: None,
                max_matches: None,
                max_matches_per_file: None,
                include_hidden: false,
                no_ignore: false,
            },
        )
        .expect("search");
        assert_eq!(output.match_count, 2, "a valid pattern stays a regex");
        assert!(
            output.pattern_note.is_empty(),
            "nothing to report when the backend accepted the pattern"
        );
        let _ = fs::remove_dir_all(&workspace_root);
    }

    #[test]
    fn rust_search_backend_supports_regex_glob_and_context() {
        let workspace_root = test_workspace("search-rust");
        fs::create_dir_all(workspace_root.join("src")).expect("create workspace");
        fs::write(workspace_root.join("notes.txt"), "alpha1\n").expect("write notes");
        fs::write(
            workspace_root.join("src").join("main.rs"),
            "before\nalpha1\nafter\nalpha2\n",
        )
        .expect("write source");

        let output = search_text_rust(
            &workspace_root,
            &workspace_root,
            ResolvedSearchTextOptions {
                pattern: "alpha[0-9]",
                glob: Some("*.rs"),
                fixed_strings: false,
                case_insensitive: false,
                before: 1,
                after: 1,
                max_matches: 1,
                max_matches_per_file: None,
                include_hidden: false,
                no_ignore: false,
            },
            "test rust backend".into(),
        )
        .expect("search");

        assert_eq!(output.backend, "rust");
        assert_eq!(output.match_count, 1);
        assert!(output.truncated);
        assert_eq!(
            output
                .results
                .iter()
                .filter(|entry| !entry.is_context)
                .map(|entry| (entry.path.as_str(), entry.line, entry.text.as_str()))
                .collect::<Vec<_>>(),
            vec![("src/main.rs", 2, "alpha1")]
        );
        assert!(output.results.iter().any(|entry| entry.is_context));

        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn grep_search_backend_marks_truncated_when_an_extra_match_exists() {
        if !command_available("grep") {
            return;
        }

        let workspace_root = test_workspace("search-grep-truncated");
        fs::create_dir_all(workspace_root.join("src")).expect("create workspace");
        fs::write(
            workspace_root.join("src").join("main.rs"),
            "alpha1\nbeta\nalpha2\n",
        )
        .expect("write source");

        let output = search_text_grep(
            &workspace_root,
            &workspace_root,
            ResolvedSearchTextOptions {
                pattern: "alpha[0-9]",
                glob: Some("*.rs"),
                fixed_strings: false,
                case_insensitive: false,
                before: 0,
                after: 0,
                max_matches: 1,
                max_matches_per_file: None,
                include_hidden: false,
                no_ignore: false,
            },
        )
        .unwrap_or_else(|_| panic!("search"));

        assert_eq!(output.backend, "grep");
        assert_eq!(output.match_count, 1);
        assert!(output.truncated);
        assert_eq!(
            output
                .results
                .iter()
                .filter(|entry| !entry.is_context)
                .map(|entry| (entry.path.as_str(), entry.line, entry.text.as_str()))
                .collect::<Vec<_>>(),
            vec![("src/main.rs", 1, "alpha1")]
        );

        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn grep_search_backend_does_not_mark_exact_limit_as_truncated() {
        if !command_available("grep") {
            return;
        }

        let workspace_root = test_workspace("search-grep-exact-limit");
        fs::create_dir_all(workspace_root.join("src")).expect("create workspace");
        fs::write(workspace_root.join("src").join("a.rs"), "alpha1\n").expect("write match");
        fs::write(workspace_root.join("src").join("b.rs"), "beta\n").expect("write non-match");

        let output = search_text_grep(
            &workspace_root,
            &workspace_root,
            ResolvedSearchTextOptions {
                pattern: "alpha[0-9]",
                glob: Some("*.rs"),
                fixed_strings: false,
                case_insensitive: false,
                before: 0,
                after: 0,
                max_matches: 1,
                max_matches_per_file: None,
                include_hidden: false,
                no_ignore: false,
            },
        )
        .unwrap_or_else(|_| panic!("search"));

        assert_eq!(output.backend, "grep");
        assert_eq!(output.match_count, 1);
        assert!(!output.truncated);
        assert_eq!(
            output
                .results
                .iter()
                .filter(|entry| !entry.is_context)
                .map(|entry| (entry.path.as_str(), entry.line, entry.text.as_str()))
                .collect::<Vec<_>>(),
            vec![("src/a.rs", 1, "alpha1")]
        );

        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn edit_file_applies_replace_and_range_operations_in_one_atomic_batch() {
        let workspace_root = test_workspace("edit-batch");
        fs::create_dir_all(&workspace_root).expect("create workspace");
        fs::write(workspace_root.join("notes.txt"), "alpha\nbeta\ngamma\n").expect("write file");
        let workspace_root_str = workspace_root.to_string_lossy().into_owned();

        let output = edit_file(
            &workspace_root_str,
            "notes.txt",
            &[
                EditOperation::Replace {
                    old_string: "alpha".into(),
                    new_string: "ALPHA".into(),
                    replace_all: false,
                },
                EditOperation::Range {
                    start_line: 2,
                    end_line: 3,
                    old_text: "beta\ngamma\n".into(),
                    new_text: "BETA\nGAMMA\n".into(),
                },
            ],
        )
        .expect("edit batch");

        assert_eq!(
            fs::read_to_string(workspace_root.join("notes.txt")).expect("read file"),
            "ALPHA\nBETA\nGAMMA\n"
        );
        assert_eq!(output.path, "notes.txt");
        assert_eq!(output.operation_count, 2);
        assert_eq!(output.applied_operations, 2);
        assert_eq!(output.replaced_occurrences, 2);
        assert_eq!(output.bytes_written, "ALPHA\nBETA\nGAMMA\n".len());

        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn edit_file_does_not_write_partial_results_when_later_operation_fails() {
        let workspace_root = test_workspace("edit-atomic-failure");
        fs::create_dir_all(&workspace_root).expect("create workspace");
        fs::write(workspace_root.join("notes.txt"), "alpha\nbeta\n").expect("write file");
        let workspace_root_str = workspace_root.to_string_lossy().into_owned();

        let error = edit_file(
            &workspace_root_str,
            "notes.txt",
            &[
                EditOperation::Replace {
                    old_string: "alpha".into(),
                    new_string: "ALPHA".into(),
                    replace_all: false,
                },
                EditOperation::Replace {
                    old_string: "missing".into(),
                    new_string: "present".into(),
                    replace_all: false,
                },
            ],
        )
        .expect_err("second operation should fail");

        assert!(error.contains("edit operation 2"));
        assert_eq!(
            fs::read_to_string(workspace_root.join("notes.txt")).expect("read file"),
            "alpha\nbeta\n"
        );

        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn edit_file_range_requires_exact_guard_text() {
        let workspace_root = test_workspace("edit-range-guard");
        fs::create_dir_all(&workspace_root).expect("create workspace");
        fs::write(workspace_root.join("notes.txt"), "alpha\nbeta\ngamma\n").expect("write file");
        let workspace_root_str = workspace_root.to_string_lossy().into_owned();

        let error = edit_file(
            &workspace_root_str,
            "notes.txt",
            &[EditOperation::Range {
                start_line: 2,
                end_line: 2,
                old_text: "wrong\n".into(),
                new_text: "BETA\n".into(),
            }],
        )
        .expect_err("guard should reject stale range");

        assert!(error.contains("old_text does not match lines 2-2"));
        assert_eq!(
            fs::read_to_string(workspace_root.join("notes.txt")).expect("read file"),
            "alpha\nbeta\ngamma\n"
        );

        let _ = fs::remove_dir_all(workspace_root);
    }
}
