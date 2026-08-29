use serde::{Deserialize, Serialize};
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

#[derive(Clone, Serialize, Deserialize)]
pub struct DetectedBrowser {
    pub name: String,
    pub binary: String,
    pub path: String,
    pub remote_debugging: bool,
    pub remote_debug_hint: String,
    pub mcp_supported: bool,
    pub support_note: String,
    pub remote_debug_active: bool,
    pub remote_debug_target: Option<String>,
    pub remote_debug_pid: Option<u32>,
}

struct BrowserCandidate {
    name: &'static str,
    binary: &'static str,
    remote_debugging: bool,
    remote_debug_hint: &'static str,
    mcp_supported: bool,
    support_note: &'static str,
}

const CANDIDATES: &[BrowserCandidate] = &[
    BrowserCandidate {
        name: "Google Chrome",
        binary: "google-chrome-stable",
        remote_debugging: true,
        remote_debug_hint: "--remote-debugging-port=<port>",
        mcp_supported: true,
        support_note: "Chromium (supported)",
    },
    BrowserCandidate {
        name: "Google Chrome",
        binary: "google-chrome",
        remote_debugging: true,
        remote_debug_hint: "--remote-debugging-port=<port>",
        mcp_supported: true,
        support_note: "Chromium (supported)",
    },
    BrowserCandidate {
        name: "Chromium",
        binary: "chromium",
        remote_debugging: true,
        remote_debug_hint: "--remote-debugging-port=<port>",
        mcp_supported: true,
        support_note: "Chromium (supported)",
    },
    BrowserCandidate {
        name: "Chromium",
        binary: "chromium-browser",
        remote_debugging: true,
        remote_debug_hint: "--remote-debugging-port=<port>",
        mcp_supported: true,
        support_note: "Chromium (supported)",
    },
    BrowserCandidate {
        name: "Microsoft Edge",
        binary: "microsoft-edge-stable",
        remote_debugging: true,
        remote_debug_hint: "--remote-debugging-port=<port>",
        mcp_supported: true,
        support_note: "Chromium (supported)",
    },
    BrowserCandidate {
        name: "Microsoft Edge",
        binary: "microsoft-edge",
        remote_debugging: true,
        remote_debug_hint: "--remote-debugging-port=<port>",
        mcp_supported: true,
        support_note: "Chromium (supported)",
    },
    BrowserCandidate {
        name: "Brave",
        binary: "brave-browser",
        remote_debugging: true,
        remote_debug_hint: "--remote-debugging-port=<port>",
        mcp_supported: true,
        support_note: "Chromium (supported)",
    },
    BrowserCandidate {
        name: "Vivaldi",
        binary: "vivaldi",
        remote_debugging: true,
        remote_debug_hint: "--remote-debugging-port=<port>",
        mcp_supported: true,
        support_note: "Chromium (supported)",
    },
    BrowserCandidate {
        name: "Opera",
        binary: "opera",
        remote_debugging: true,
        remote_debug_hint: "--remote-debugging-port=<port>",
        mcp_supported: true,
        support_note: "Chromium (supported)",
    },
    BrowserCandidate {
        name: "Firefox",
        binary: "firefox",
        remote_debugging: false,
        remote_debug_hint: "--remote-debugging-port <port>",
        mcp_supported: false,
        support_note: "Not supported yet (CDP bridge for Firefox not wired)",
    },
];

pub fn detect_browsers() -> Vec<DetectedBrowser> {
    let mut found: Vec<DetectedBrowser> = Vec::new();
    let mut seen_names: HashSet<&'static str> = HashSet::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    let processes = collect_processes();

    for candidate in CANDIDATES {
        let Some(path) = resolve_binary(candidate.binary) else {
            continue;
        };

        if !seen_names.insert(candidate.name) {
            continue;
        }

        let normalized = normalize_path(&path);
        if !seen_paths.insert(normalized.clone()) {
            continue;
        }

        let active_remote =
            find_active_remote_debug_for_binary(candidate.binary, &normalized, &processes);

        found.push(DetectedBrowser {
            name: candidate.name.to_string(),
            binary: candidate.binary.to_string(),
            path: path.display().to_string(),
            remote_debugging: candidate.remote_debugging,
            remote_debug_hint: candidate.remote_debug_hint.to_string(),
            mcp_supported: candidate.mcp_supported,
            support_note: candidate.support_note.to_string(),
            remote_debug_active: active_remote.is_some(),
            remote_debug_target: active_remote.as_ref().map(|r| r.target.clone()),
            remote_debug_pid: active_remote.as_ref().map(|r| r.pid),
        });
    }

    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn resolve_binary(binary: &str) -> Option<PathBuf> {
    let input = Path::new(binary);
    if input.is_absolute() || binary.contains('/') {
        if input.is_file() {
            return Some(input.to_path_buf());
        }
        return None;
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(binary);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    #[cfg(target_os = "macos")]
    if let Some(candidate) = resolve_macos_application_binary(binary) {
        return Some(candidate);
    }

    None
}

#[cfg(target_os = "macos")]
fn resolve_macos_application_binary(binary: &str) -> Option<PathBuf> {
    let relative = macos_application_binary_relative_path(binary)?;
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }

    roots
        .into_iter()
        .map(|root| root.join(relative))
        .find(|candidate| candidate.is_file())
}

#[cfg(target_os = "macos")]
fn macos_application_binary_relative_path(binary: &str) -> Option<&'static str> {
    match binary {
        "google-chrome-stable" | "google-chrome" => {
            Some("Google Chrome.app/Contents/MacOS/Google Chrome")
        }
        "chromium" | "chromium-browser" => Some("Chromium.app/Contents/MacOS/Chromium"),
        "microsoft-edge-stable" | "microsoft-edge" => {
            Some("Microsoft Edge.app/Contents/MacOS/Microsoft Edge")
        }
        "brave-browser" => Some("Brave Browser.app/Contents/MacOS/Brave Browser"),
        "vivaldi" => Some("Vivaldi.app/Contents/MacOS/Vivaldi"),
        "opera" => Some("Opera.app/Contents/MacOS/Opera"),
        "firefox" => Some("Firefox.app/Contents/MacOS/firefox"),
        _ => None,
    }
}

fn normalize_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

struct ProcessInfo {
    pid: u32,
    cmdline: Vec<String>,
    command_line: String,
}

struct ActiveRemoteDebug {
    pid: u32,
    target: String,
}

#[cfg(target_os = "linux")]
fn collect_processes() -> Vec<ProcessInfo> {
    let mut processes = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return processes;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid_str) = name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        let cmdline_path = entry.path().join("cmdline");
        let Ok(bytes) = fs::read(cmdline_path) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        let args: Vec<String> = bytes
            .split(|b| *b == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect();
        if args.is_empty() {
            continue;
        }
        let command_line = args.join(" ");
        processes.push(ProcessInfo {
            pid,
            cmdline: args,
            command_line,
        });
    }

    processes
}

#[cfg(target_os = "macos")]
fn collect_processes() -> Vec<ProcessInfo> {
    let Ok(output) = Command::new("/bin/ps")
        .args(["-axo", "pid=,command="])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    parse_macos_ps_output(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "macos")]
fn parse_macos_ps_output(output: &str) -> Vec<ProcessInfo> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let separator = line.find(char::is_whitespace)?;
            let pid = line[..separator].parse::<u32>().ok()?;
            let command_line = line[separator..].trim_start();
            if command_line.is_empty() {
                return None;
            }
            let cmdline = command_line
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>();
            Some(ProcessInfo {
                pid,
                cmdline,
                command_line: command_line.to_string(),
            })
        })
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn collect_processes() -> Vec<ProcessInfo> {
    Vec::new()
}

fn find_active_remote_debug_for_binary(
    binary: &str,
    resolved_path: &str,
    processes: &[ProcessInfo],
) -> Option<ActiveRemoteDebug> {
    for p in processes {
        if !process_matches_binary(p, binary, resolved_path) {
            continue;
        }
        let Some(target) = extract_remote_debug_target(&p.cmdline) else {
            continue;
        };
        return Some(ActiveRemoteDebug { pid: p.pid, target });
    }
    None
}

fn process_matches_binary(process: &ProcessInfo, binary: &str, resolved_path: &str) -> bool {
    process
        .cmdline
        .iter()
        .any(|arg| command_matches_binary(arg, binary))
        || command_line_starts_with_executable(&process.command_line, resolved_path)
}

fn command_line_starts_with_executable(command_line: &str, executable: &str) -> bool {
    if command_line == executable {
        return true;
    }
    command_line
        .strip_prefix(executable)
        .map(str::trim_start)
        .is_some_and(|rest| rest.starts_with('-'))
}

fn command_matches_binary(arg: &str, binary: &str) -> bool {
    if arg == binary {
        return true;
    }
    Path::new(arg)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == binary)
}

fn extract_remote_debug_target(args: &[String]) -> Option<String> {
    let mut address = "127.0.0.1".to_string();
    let mut port: Option<String> = None;

    for (idx, arg) in args.iter().enumerate() {
        if arg == "--remote-debugging-pipe" {
            return Some("pipe".into());
        }

        if let Some(v) = arg.strip_prefix("--remote-debugging-address=") {
            if !v.is_empty() {
                address = v.to_string();
            }
        } else if arg == "--remote-debugging-address" {
            if let Some(v) = args.get(idx + 1) {
                if !v.is_empty() {
                    address = v.clone();
                }
            }
        }

        if let Some(v) = arg.strip_prefix("--remote-debugging-port=") {
            if !v.is_empty() {
                port = Some(v.to_string());
            }
        } else if arg == "--remote-debugging-port" {
            if let Some(v) = args.get(idx + 1) {
                if !v.is_empty() {
                    port = Some(v.clone());
                }
            }
        }

        if let Some(v) = arg.strip_prefix("--start-debugger-server=") {
            if !v.is_empty() {
                port = Some(v.to_string());
            }
        } else if arg == "--start-debugger-server" {
            if let Some(v) = args.get(idx + 1) {
                if !v.is_empty() {
                    port = Some(v.clone());
                }
            }
        }
    }

    port.map(|p| format!("{address}:{p}"))
}

pub fn format_browser_names(browsers: &[DetectedBrowser]) -> String {
    if browsers.is_empty() {
        return "--".into();
    }
    browsers
        .iter()
        .map(|b| b.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn format_remote_debug_names(browsers: &[DetectedBrowser]) -> String {
    let remote: Vec<&str> = browsers
        .iter()
        .filter(|b| b.mcp_supported && b.remote_debugging)
        .map(|b| b.name.as_str())
        .collect();
    if remote.is_empty() {
        return "--".into();
    }
    remote.join(", ")
}

pub fn format_active_remote_debug_names(browsers: &[DetectedBrowser]) -> String {
    let active: Vec<String> = browsers
        .iter()
        .filter(|b| b.mcp_supported && b.remote_debug_active)
        .map(|b| {
            if let Some(target) = &b.remote_debug_target {
                format!("{} ({target})", b.name)
            } else {
                b.name.clone()
            }
        })
        .collect();
    if active.is_empty() {
        return "--".into();
    }
    active.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_application_paths_cover_supported_chromium_browsers() {
        assert_eq!(
            macos_application_binary_relative_path("google-chrome"),
            Some("Google Chrome.app/Contents/MacOS/Google Chrome")
        );
        assert_eq!(
            macos_application_binary_relative_path("chromium"),
            Some("Chromium.app/Contents/MacOS/Chromium")
        );
        assert_eq!(
            macos_application_binary_relative_path("microsoft-edge"),
            Some("Microsoft Edge.app/Contents/MacOS/Microsoft Edge")
        );
        assert_eq!(
            macos_application_binary_relative_path("brave-browser"),
            Some("Brave Browser.app/Contents/MacOS/Brave Browser")
        );
        assert_eq!(
            macos_application_binary_relative_path("vivaldi"),
            Some("Vivaldi.app/Contents/MacOS/Vivaldi")
        );
        assert_eq!(
            macos_application_binary_relative_path("opera"),
            Some("Opera.app/Contents/MacOS/Opera")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_ps_parser_matches_app_bundle_executable_with_spaces() {
        let executable = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        let processes = parse_macos_ps_output(&format!(
            "  4242 {executable} --remote-debugging-address=127.0.0.1 --remote-debugging-port=9222\n"
        ));

        let active = find_active_remote_debug_for_binary("google-chrome", executable, &processes)
            .expect("detect active Chrome remote debugging process");
        assert_eq!(active.pid, 4242);
        assert_eq!(active.target, "127.0.0.1:9222");
    }

    #[test]
    fn executable_prefix_requires_argument_boundary() {
        assert!(command_line_starts_with_executable(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome --flag",
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
        ));
        assert!(!command_line_starts_with_executable(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome Helper --flag",
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
        ));
    }
}
