#[cfg(target_os = "macos")]
use std::{
    env,
    ffi::CStr,
    fs,
    os::raw::{c_char, c_int},
    path::PathBuf,
    process::Command,
    thread,
    time::Duration,
};

#[cfg(target_os = "macos")]
const APPLE_TERMINAL_PROGRAM: &str = "Apple_Terminal";
#[cfg(target_os = "macos")]
const OSASCRIPT_PATH: &str = "/usr/bin/osascript";
#[cfg(target_os = "macos")]
const OPEN_PATH: &str = "/usr/bin/open";
#[cfg(target_os = "macos")]
const PLIST_BUDDY_PATH: &str = "/usr/libexec/PlistBuddy";
#[cfg(target_os = "macos")]
const TERMINAL_APP_PATH: &str = "/System/Applications/Utilities/Terminal.app";
#[cfg(target_os = "macos")]
const PROFILE_FILE_NAME: &str = "CatDesk.terminal";
#[cfg(target_os = "macos")]
const PROFILE_NAME: &str = "CatDesk";
#[cfg(target_os = "macos")]
const SKIP_ENV: &str = "CATDESK_SKIP_MACOS_TERMINAL_PROFILE";
#[cfg(target_os = "macos")]
const FONT_HEIGHT_SPACING_ENV: &str = "CATDESK_TERMINAL_FONT_HEIGHT_SPACING";
#[cfg(target_os = "macos")]
const FONT_WIDTH_SPACING_ENV: &str = "CATDESK_TERMINAL_FONT_WIDTH_SPACING";
#[cfg(target_os = "macos")]
const FONT_ANTIALIAS_ENV: &str = "CATDESK_TERMINAL_FONT_ANTIALIAS";
#[cfg(target_os = "macos")]
const PROFILE_IMPORT_DELAY: Duration = Duration::from_millis(400);
#[cfg(target_os = "macos")]
const PROFILE_APPLY_RETRY_DELAY: Duration = Duration::from_millis(250);
#[cfg(target_os = "macos")]
const PROFILE_APPLY_ATTEMPTS: usize = 20;
#[cfg(target_os = "macos")]
const PROFILE_FONT_HEIGHT_SPACING: &str = "0.73";
#[cfg(target_os = "macos")]
const PROFILE_FONT_WIDTH_SPACING: &str = "1.0";
#[cfg(target_os = "macos")]
const PROFILE_FONT_NAME: &str = "Menlo-Regular";
#[cfg(target_os = "macos")]
const PROFILE_FONT_SIZE: i32 = 11;
#[cfg(target_os = "macos")]
const PROFILE_BYTES: &[u8] = include_bytes!("../assets/CatDesk.terminal");

#[cfg(target_os = "macos")]
enum PlistValueType {
    Bool,
    Real,
}

pub enum LaunchAction {
    Continue,
    #[cfg(target_os = "macos")]
    ExitAfterProfileBootstrap,
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn ttyname(fd: c_int) -> *const c_char;
}

#[cfg(target_os = "macos")]
pub fn maybe_relaunch_in_terminal_profile() -> Result<LaunchAction, String> {
    if !should_manage_terminal_launch() {
        return Ok(LaunchAction::Continue);
    }

    if profile_is_installed()? {
        normalize_profile_settings()?;
        if !profile_refresh_requested() && current_tab_uses_profile()? {
            return Ok(LaunchAction::Continue);
        }
        if !profile_refresh_requested() && apply_profile_to_current_tab()? {
            return Ok(LaunchAction::ExitAfterProfileBootstrap);
        }
    }

    let original_tty = current_tty();
    let existing_window_ids = terminal_window_ids().unwrap_or_default();
    let profile_path = write_profile_file()?;
    import_profile(&profile_path)?;
    thread::sleep(PROFILE_IMPORT_DELAY);

    let Some(original_tty) = original_tty.as_deref() else {
        return Err("failed to identify the current Terminal.app tab".to_string());
    };

    let mut last_error = None;
    for _ in 0..PROFILE_APPLY_ATTEMPTS {
        if !profile_is_installed()? {
            thread::sleep(PROFILE_APPLY_RETRY_DELAY);
            continue;
        }
        normalize_profile_settings()?;

        match apply_profile_to_tty(original_tty) {
            Ok(true) => {
                let _ = close_new_helper_windows(&existing_window_ids, original_tty);
                return Ok(LaunchAction::ExitAfterProfileBootstrap);
            }
            Ok(false) => {
                last_error = Some(
                    "failed to match the original Terminal.app tab after importing the profile"
                        .to_string(),
                );
            }
            Err(error) => last_error = Some(error),
        }
        thread::sleep(PROFILE_APPLY_RETRY_DELAY);
    }

    let _ = close_new_helper_windows(&existing_window_ids, original_tty);
    if profile_is_installed()? {
        return Ok(LaunchAction::ExitAfterProfileBootstrap);
    }

    Err(last_error.unwrap_or_else(|| "failed to import the Terminal.app profile".to_string()))
}

#[cfg(not(target_os = "macos"))]
pub fn maybe_relaunch_in_terminal_profile() -> Result<LaunchAction, String> {
    Ok(LaunchAction::Continue)
}

#[cfg(target_os = "macos")]
fn should_manage_terminal_launch() -> bool {
    env::var("TERM_PROGRAM").ok().as_deref() == Some(APPLE_TERMINAL_PROGRAM)
        && env::var_os(SKIP_ENV).is_none()
}

#[cfg(target_os = "macos")]
fn profile_refresh_requested() -> bool {
    [
        FONT_HEIGHT_SPACING_ENV,
        FONT_WIDTH_SPACING_ENV,
        FONT_ANTIALIAS_ENV,
    ]
    .into_iter()
    .any(|name| env::var_os(name).is_some())
}

#[cfg(target_os = "macos")]
fn profile_is_installed() -> Result<bool, String> {
    let script = format!(
        r#"
tell application "Terminal"
  return exists settings set "{PROFILE_NAME}"
end tell
"#
    );
    let stdout = run_osascript(&script, "failed to inspect Terminal.app profiles")?;
    Ok(stdout.trim() == "true")
}

#[cfg(target_os = "macos")]
fn write_profile_file() -> Result<PathBuf, String> {
    let path = env::temp_dir().join(PROFILE_FILE_NAME);
    fs::write(&path, PROFILE_BYTES)
        .map_err(|error| format!("failed to write {PROFILE_FILE_NAME}: {error}"))?;
    customize_profile_file(&path)?;
    Ok(path)
}

#[cfg(target_os = "macos")]
fn import_profile(profile_path: &PathBuf) -> Result<(), String> {
    let status = Command::new(OPEN_PATH)
        .args(["-a", TERMINAL_APP_PATH])
        .arg(profile_path)
        .status()
        .map_err(|error| format!("failed to import {PROFILE_FILE_NAME}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Terminal.app rejected {PROFILE_FILE_NAME} with status {status}"
        ))
    }
}

#[cfg(target_os = "macos")]
fn apply_profile_to_current_tab() -> Result<bool, String> {
    let Some(current_tty) = current_tty() else {
        return Ok(false);
    };
    apply_profile_to_tty(&current_tty)
}

#[cfg(target_os = "macos")]
fn current_tab_uses_profile() -> Result<bool, String> {
    let Some(current_tty) = current_tty() else {
        return Ok(false);
    };
    current_profile_name_for_tty(&current_tty).map(|name| name.as_deref() == Some(PROFILE_NAME))
}

#[cfg(target_os = "macos")]
fn normalize_profile_settings() -> Result<(), String> {
    let script = format!(
        r#"
tell application "Terminal"
  tell settings set "{PROFILE_NAME}"
    set font name to "{PROFILE_FONT_NAME}"
    set font size to {PROFILE_FONT_SIZE}
    set font antialiasing to true
  end tell
end tell
"#
    );

    let _ = run_osascript(
        &script,
        "failed to normalize Terminal.app profile font settings",
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn current_profile_name_for_tty(current_tty: &str) -> Result<Option<String>, String> {
    let script = r#"
tell application "Terminal"
  set targetTTY to system attribute "CATDESK_TERMINAL_TARGET_TTY"
  repeat with w in windows
    repeat with t in tabs of w
      try
        if tty of t is targetTTY then
          return name of current settings of t
        end if
      end try
    end repeat
  end repeat
  return ""
end tell
"#;

    let profile_name = run_osascript_with_env(
        &[("CATDESK_TERMINAL_TARGET_TTY", current_tty)],
        script,
        "failed to inspect the current Terminal.app tab profile",
    )?
    .trim()
    .to_string();

    if profile_name.is_empty() {
        Ok(None)
    } else {
        Ok(Some(profile_name))
    }
}

#[cfg(target_os = "macos")]
fn apply_profile_to_tty(current_tty: &str) -> Result<bool, String> {
    let script = format!(
        r#"
tell application "Terminal"
  set targetTTY to system attribute "CATDESK_TERMINAL_TARGET_TTY"
  repeat with w in windows
    repeat with t in tabs of w
      try
        if tty of t is targetTTY then
          set current settings of t to settings set "{PROFILE_NAME}"
          set font name of t to "{PROFILE_FONT_NAME}"
          set font size of t to {PROFILE_FONT_SIZE}
          set font antialiasing of t to true
          return true
        end if
      end try
    end repeat
  end repeat
  return false
end tell
"#
    );

    run_osascript_with_env(
        &[("CATDESK_TERMINAL_TARGET_TTY", current_tty)],
        &script,
        "failed to apply Terminal.app profile in place",
    )
    .map(|stdout| stdout.trim() == "true")
}

#[cfg(target_os = "macos")]
fn current_tty() -> Option<String> {
    for fd in [0, 1, 2] {
        let tty_ptr = unsafe { ttyname(fd) };
        if tty_ptr.is_null() {
            continue;
        }
        let tty = unsafe { CStr::from_ptr(tty_ptr) }
            .to_string_lossy()
            .trim()
            .to_string();
        if !tty.is_empty() {
            return Some(tty);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn terminal_window_ids() -> Result<Vec<i64>, String> {
    let stdout = run_osascript(
        r#"
tell application "Terminal"
  if (count of windows) is 0 then
    return ""
  end if
  set AppleScript's text item delimiters to ","
  set windowIDs to (id of every window) as text
  set AppleScript's text item delimiters to ""
  return windowIDs
end tell
"#,
        "failed to inspect Terminal.app windows",
    )?;

    let mut ids = Vec::new();
    for raw in stdout.split(',') {
        let value = raw.trim();
        if value.is_empty() {
            continue;
        }
        if let Ok(id) = value.parse::<i64>() {
            ids.push(id);
        }
    }
    Ok(ids)
}

#[cfg(target_os = "macos")]
fn close_new_helper_windows(existing_window_ids: &[i64], original_tty: &str) -> Result<(), String> {
    let window_ids = existing_window_ids
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");

    let script = format!(
        r#"
tell application "Terminal"
  set targetTTY to system attribute "CATDESK_TERMINAL_TARGET_TTY"
  set existingWindowIDsText to system attribute "CATDESK_EXISTING_WINDOW_IDS"
  if existingWindowIDsText is "" then
    set existingWindowIDs to {{}}
  else
    set AppleScript's text item delimiters to ","
    set existingWindowIDs to text items of existingWindowIDsText
    set AppleScript's text item delimiters to ""
  end if

  set windowsToClose to {{}}
  repeat with w in windows
    try
      set windowIDText to (id of w) as text
      if windowIDText is not in existingWindowIDs then
        set t to selected tab of w
        if (tty of t is not targetTTY) and (name of current settings of t is "{PROFILE_NAME}") then
          set end of windowsToClose to w
        end if
      end if
    end try
  end repeat

  repeat with w in windowsToClose
    try
      do script "exit" in selected tab of w
    end try
  end repeat

  repeat 20 times
    set anyBusy to false
    repeat with w in windowsToClose
      try
        if busy of selected tab of w then
          set anyBusy to true
        end if
      end try
    end repeat
    if anyBusy is false then
      exit repeat
    end if
    delay 0.2
  end repeat

  repeat with w in windowsToClose
    try
      if busy of selected tab of w is false then
        close w saving no
      end if
    end try
  end repeat
  return (count of windowsToClose) as text
end tell
"#
    );

    let _ = run_osascript_with_env(
        &[
            ("CATDESK_TERMINAL_TARGET_TTY", original_tty),
            ("CATDESK_EXISTING_WINDOW_IDS", &window_ids),
        ],
        &script,
        "failed to close the temporary Terminal.app helper window",
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_osascript(script: &str, context: &str) -> Result<String, String> {
    run_osascript_with_env(&[], script, context)
}

#[cfg(target_os = "macos")]
fn run_osascript_with_env(
    env_pairs: &[(&str, &str)],
    script: &str,
    context: &str,
) -> Result<String, String> {
    let mut command = Command::new(OSASCRIPT_PATH);
    for (key, value) in env_pairs {
        command.env(key, value);
    }

    let output = command
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|error| format!("{context}: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return if stderr.is_empty() {
            Err(format!("{context}: status {}", output.status))
        } else {
            Err(stderr)
        };
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(target_os = "macos")]
fn plist_mutate(plist_path: &PathBuf, command: &str) -> Result<bool, String> {
    let output = Command::new(PLIST_BUDDY_PATH)
        .args(["-c", command])
        .arg(plist_path)
        .output()
        .map_err(|error| format!("failed to update Terminal.app profile settings: {error}"))?;
    Ok(output.status.success())
}

#[cfg(target_os = "macos")]
fn plist_set_or_add(
    plist_path: &PathBuf,
    key_path: &str,
    value_type: PlistValueType,
    value: &str,
) -> Result<(), String> {
    if plist_mutate(plist_path, &format!("Set {key_path} {value}"))? {
        return Ok(());
    }

    let value_type = match value_type {
        PlistValueType::Bool => "bool",
        PlistValueType::Real => "real",
    };
    if plist_mutate(plist_path, &format!("Add {key_path} {value_type} {value}"))? {
        return Ok(());
    }

    Err(format!(
        "failed to update Terminal.app profile setting {key_path}"
    ))
}

#[cfg(target_os = "macos")]
fn customize_profile_file(profile_path: &PathBuf) -> Result<(), String> {
    plist_set_or_add(
        profile_path,
        ":FontHeightSpacing",
        PlistValueType::Real,
        &profile_font_height_spacing(),
    )?;
    plist_set_or_add(
        profile_path,
        ":FontWidthSpacing",
        PlistValueType::Real,
        &profile_font_width_spacing(),
    )?;
    plist_set_or_add(
        profile_path,
        ":FontAntialias",
        PlistValueType::Bool,
        profile_font_antialias_plist(),
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn profile_font_height_spacing() -> String {
    env::var(FONT_HEIGHT_SPACING_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| PROFILE_FONT_HEIGHT_SPACING.to_string())
}

#[cfg(target_os = "macos")]
fn profile_font_width_spacing() -> String {
    env::var(FONT_WIDTH_SPACING_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| PROFILE_FONT_WIDTH_SPACING.to_string())
}

#[cfg(target_os = "macos")]
fn profile_font_antialias() -> bool {
    env::var(FONT_ANTIALIAS_ENV)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .and_then(|value| match value.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(true)
}

#[cfg(target_os = "macos")]
fn profile_font_antialias_plist() -> &'static str {
    if profile_font_antialias() {
        "true"
    } else {
        "false"
    }
}
