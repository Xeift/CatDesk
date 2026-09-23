use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::time::{Duration, timeout};

const READ_CHUNK_BYTES: usize = 8 * 1024;

#[derive(Debug)]
pub struct ProcessRunResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

/// A spawned shell process owned by CatDesk.
///
/// Dropping this value is intentionally destructive: if the command is still
/// alive, CatDesk terminates the process tree. This is what keeps a cancelled
/// MCP request from leaving a compiler or build process behind.
pub struct SpawnedProcess {
    child: Child,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    tree: ProcessTreeGuard,
    cleanup_dir: Option<PathBuf>,
}

impl SpawnedProcess {
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.stderr.take()
    }

    pub async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    /// Terminate the root process and all descendants owned by this command.
    pub async fn terminate_tree(&mut self) {
        self.tree.terminate().await;
        // Job-object / process-group termination should already include the
        // root, but keep Tokio's direct kill as a best-effort fallback.
        let _ = self.child.start_kill();
    }

    /// Finalize ownership after the root process exits. Any descendants still
    /// alive at that point are terminated so a command cannot silently detach
    /// work that outlives its CatDesk job.
    pub async fn disarm(&mut self) {
        self.tree.disarm().await;
        self.cleanup_command_dir();
    }

    fn cleanup_command_dir(&mut self) {
        if let Some(path) = self.cleanup_dir.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

impl Drop for SpawnedProcess {
    fn drop(&mut self) {
        if self.tree.is_armed() {
            self.tree.terminate_blocking();
            let _ = self.child.start_kill();
        }
        self.cleanup_command_dir();
    }
}

#[derive(Debug)]
struct ProcessTreeGuard {
    pid: u32,
    armed: bool,
    #[cfg(windows)]
    job_handle: Option<usize>,
}

impl ProcessTreeGuard {
    #[cfg(not(windows))]
    fn new(pid: u32) -> Self {
        Self { pid, armed: true }
    }

    #[cfg(windows)]
    fn with_windows_job(pid: u32, job_handle: usize) -> Self {
        Self {
            pid,
            armed: true,
            job_handle: Some(job_handle),
        }
    }

    fn is_armed(&self) -> bool {
        self.armed
    }

    async fn disarm(&mut self) {
        if !self.armed {
            return;
        }
        #[cfg(windows)]
        {
            if self.job_handle.is_some() {
                close_windows_job(&mut self.job_handle);
            } else {
                terminate_process_tree_async(self.pid).await;
            }
        }
        #[cfg(not(windows))]
        terminate_process_tree(self.pid);
        self.armed = false;
    }

    async fn terminate(&mut self) {
        if !self.armed {
            return;
        }
        #[cfg(windows)]
        {
            if !terminate_windows_job(&mut self.job_handle) {
                terminate_process_tree_async(self.pid).await;
            }
        }
        #[cfg(not(windows))]
        terminate_process_tree(self.pid);
        self.armed = false;
    }

    fn terminate_blocking(&mut self) {
        if !self.armed {
            return;
        }
        #[cfg(windows)]
        {
            if !terminate_windows_job(&mut self.job_handle) {
                terminate_process_tree_blocking(self.pid);
            }
        }
        #[cfg(not(windows))]
        terminate_process_tree(self.pid);
        self.armed = false;
    }
}

impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        self.terminate_blocking();
    }
}

#[cfg(windows)]
fn create_windows_job_for_process(pid: u32) -> io::Result<usize> {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            let error = io::Error::last_os_error();
            CloseHandle(job);
            return Err(error);
        }

        let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            let error = io::Error::last_os_error();
            CloseHandle(job);
            return Err(error);
        }
        let assigned = AssignProcessToJobObject(job, process) != 0;
        let assign_error = if assigned {
            None
        } else {
            Some(io::Error::last_os_error())
        };
        CloseHandle(process);
        if let Some(error) = assign_error {
            CloseHandle(job);
            return Err(error);
        }

        Ok(job as usize)
    }
}

#[cfg(windows)]
fn resume_windows_process(pid: u32) -> io::Result<()> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        let mut entry: THREADENTRY32 = zeroed();
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut found = Thread32First(snapshot, &mut entry) != 0;
        while found {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if thread.is_null() {
                    let error = io::Error::last_os_error();
                    CloseHandle(snapshot);
                    return Err(error);
                }
                let previous_suspend_count = ResumeThread(thread);
                let resume_error = if previous_suspend_count == u32::MAX {
                    Some(io::Error::last_os_error())
                } else {
                    None
                };
                CloseHandle(thread);
                CloseHandle(snapshot);
                return match resume_error {
                    Some(error) => Err(error),
                    None => Ok(()),
                };
            }
            found = Thread32Next(snapshot, &mut entry) != 0;
        }

        CloseHandle(snapshot);
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "suspended process did not expose a resumable thread",
        ))
    }
}

#[cfg(windows)]
fn close_windows_job(job_handle: &mut Option<usize>) {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    if let Some(raw) = job_handle.take() {
        unsafe {
            CloseHandle(raw as HANDLE);
        }
    }
}

#[cfg(windows)]
fn terminate_windows_job(job_handle: &mut Option<usize>) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::TerminateJobObject;
    let Some(raw) = job_handle.take() else {
        return false;
    };
    let handle = raw as HANDLE;
    unsafe {
        let terminated = TerminateJobObject(handle, 1) != 0;
        CloseHandle(handle);
        terminated
    }
}

#[cfg(windows)]
fn terminate_process_tree_blocking(pid: u32) {
    // `/T` includes descendants and `/F` makes cancellation deterministic.
    // Use the executable directly rather than a shell command so the PID never
    // passes through shell parsing. This synchronous path is reserved for Drop,
    // where Rust cannot await cleanup.
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(windows)]
async fn terminate_process_tree_async(pid: u32) {
    let _ = tokio::task::spawn_blocking(move || terminate_process_tree_blocking(pid)).await;
}

#[cfg(unix)]
fn terminate_process_tree(pid: u32) {
    let pgid = match i32::try_from(pid) {
        Ok(value) => value,
        Err(_) => return,
    };
    // The shell is placed in its own process group at spawn time. A negative
    // PID targets the complete process group, including compiler descendants.
    unsafe {
        let _ = libc::kill(-pgid, libc::SIGKILL);
    }
}

#[cfg(not(any(windows, unix)))]
fn terminate_process_tree(_pid: u32) {}

struct PreparedShellCommand {
    command: Command,
    cleanup_dir: Option<PathBuf>,
}

fn uses_linux_sandbox(sandbox_enabled: bool) -> bool {
    cfg!(target_os = "linux") && sandbox_enabled
}

fn shell_command(
    command: &str,
    workspace_root: &Path,
    cwd: &Path,
    sandbox_enabled: bool,
) -> io::Result<PreparedShellCommand> {
    #[cfg(windows)]
    {
        let _ = workspace_root;
        let _ = cwd;
        let _ = sandbox_enabled;
        let mut shell = Command::new("powershell.exe");
        shell
            .arg("-NoLogo")
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(command);
        Ok(PreparedShellCommand {
            command: shell,
            cleanup_dir: None,
        })
    }

    #[cfg(all(not(windows), not(target_os = "linux")))]
    {
        let _ = workspace_root;
        let _ = cwd;
        let _ = sandbox_enabled;
        let mut shell = Command::new("/bin/bash");
        shell.arg("-c").arg(command);
        Ok(PreparedShellCommand {
            command: shell,
            cleanup_dir: None,
        })
    }

    #[cfg(target_os = "linux")]
    {
        if uses_linux_sandbox(sandbox_enabled) {
            let (helper, scratch_dir) =
                crate::linux_sandbox::helper_command(command, workspace_root, cwd)?;
            return Ok(PreparedShellCommand {
                command: Command::from(helper),
                cleanup_dir: Some(scratch_dir),
            });
        }
        let mut shell = Command::new("/bin/bash");
        shell.arg("-c").arg(command);
        Ok(PreparedShellCommand {
            command: shell,
            cleanup_dir: None,
        })
    }
}

fn spawn_prepared_shell_command(
    prepared: PreparedShellCommand,
    cwd: &Path,
) -> io::Result<SpawnedProcess> {
    let mut shell = prepared.command;
    let mut cleanup_dir = prepared.cleanup_dir;
    shell
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        shell.as_std_mut().process_group(0);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
        shell.as_std_mut().creation_flags(CREATE_SUSPENDED);
    }

    let mut child = match shell.spawn() {
        Ok(child) => child,
        Err(error) => {
            if let Some(path) = cleanup_dir.take() {
                let _ = std::fs::remove_dir_all(path);
            }
            return Err(error);
        }
    };
    let Some(pid) = child.id() else {
        let _ = child.start_kill();
        if let Some(path) = cleanup_dir.take() {
            let _ = std::fs::remove_dir_all(path);
        }
        return Err(io::Error::other(
            "spawned command did not expose a process id",
        ));
    };

    #[cfg(windows)]
    let tree = {
        let job_handle = match create_windows_job_for_process(pid) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = child.start_kill();
                return Err(io::Error::new(
                    error.kind(),
                    format!("failed to assign suspended command to Windows Job Object: {error}"),
                ));
            }
        };
        if let Err(error) = resume_windows_process(pid) {
            let mut job_handle = Some(job_handle);
            close_windows_job(&mut job_handle);
            let _ = child.start_kill();
            return Err(io::Error::new(
                error.kind(),
                format!("failed to resume suspended command process: {error}"),
            ));
        }
        ProcessTreeGuard::with_windows_job(pid, job_handle)
    };

    #[cfg(not(windows))]
    let tree = ProcessTreeGuard::new(pid);

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    Ok(SpawnedProcess {
        child,
        stdout,
        stderr,
        tree,
        cleanup_dir,
    })
}

pub async fn spawn_shell_command(
    command: &str,
    workspace_root: &Path,
    cwd: &Path,
    sandbox_enabled: bool,
) -> io::Result<SpawnedProcess> {
    let command = command.to_owned();
    let workspace_root = workspace_root.to_path_buf();
    let cwd_for_prepare = cwd.to_path_buf();
    let prepared = tokio::task::spawn_blocking(move || {
        shell_command(&command, &workspace_root, &cwd_for_prepare, sandbox_enabled)
    })
    .await
    .map_err(|error| io::Error::other(format!("command preparation task failed: {error}")))??;

    // Keep the actual process spawn on a Tokio runtime worker rather than a
    // spawn_blocking worker. Linux bubblewrap uses --die-with-parent, whose
    // PR_SET_PDEATHSIG parent is the specific thread that created the process.
    // Tokio retires idle blocking workers, so spawning bwrap there can kill an
    // otherwise healthy long-running command when that worker exits.
    spawn_prepared_shell_command(prepared, cwd)
}

#[derive(Debug)]
struct BoundedBytes {
    bytes: Vec<u8>,
    max_bytes: usize,
    truncated: bool,
}

impl BoundedBytes {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(max_bytes.min(64 * 1024)),
            max_bytes,
            truncated: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if self.max_bytes == 0 {
            self.truncated |= !chunk.is_empty();
            return;
        }
        let remaining = self.max_bytes.saturating_sub(self.bytes.len());
        if chunk.len() <= remaining {
            self.bytes.extend_from_slice(chunk);
            return;
        }
        self.bytes.extend_from_slice(&chunk[..remaining]);
        self.truncated = true;
    }

    fn into_text(self) -> (String, bool) {
        (
            String::from_utf8_lossy(&self.bytes).into_owned(),
            self.truncated,
        )
    }
}

#[derive(Debug, Default)]
struct CapturedOutput {
    text: String,
    truncated: bool,
    read_error: Option<String>,
}

async fn capture_reader<R>(mut reader: R, max_bytes: usize) -> CapturedOutput
where
    R: AsyncRead + Unpin,
{
    let mut output = BoundedBytes::new(max_bytes);
    let mut buffer = vec![0_u8; READ_CHUNK_BYTES];
    let mut read_error = None;
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => output.push(&buffer[..read]),
            Err(error) => {
                read_error = Some(error.to_string());
                break;
            }
        }
    }
    let (text, truncated) = output.into_text();
    CapturedOutput {
        text,
        truncated,
        read_error,
    }
}

async fn finish_capture(
    task: Option<tokio::task::JoinHandle<CapturedOutput>>,
    stream: &str,
) -> CapturedOutput {
    let Some(task) = task else {
        return CapturedOutput::default();
    };
    match task.await {
        Ok(captured) => captured,
        Err(error) => CapturedOutput {
            read_error: Some(format!("{stream} capture task failed: {error}")),
            ..CapturedOutput::default()
        },
    }
}

fn append_stderr_diagnostic(stderr: &mut String, message: &str) {
    if !stderr.is_empty() && !stderr.ends_with('\n') {
        stderr.push('\n');
    }
    stderr.push_str(message);
}

pub async fn run_shell_command(
    command: &str,
    workspace_root: &Path,
    cwd: &Path,
    sandbox_enabled: bool,
    timeout_ms: u64,
    max_capture_bytes: usize,
) -> ProcessRunResult {
    let started = Instant::now();
    let mut process = match spawn_shell_command(command, workspace_root, cwd, sandbox_enabled).await
    {
        Ok(process) => process,
        Err(error) => {
            return ProcessRunResult {
                stdout: String::new(),
                stderr: format!("Failed to execute: {error}"),
                success: false,
                exit_code: None,
                elapsed_ms: started.elapsed().as_millis() as u64,
                timed_out: false,
                stdout_truncated: false,
                stderr_truncated: false,
            };
        }
    };

    let stdout_task = process
        .take_stdout()
        .map(|stdout| tokio::spawn(capture_reader(stdout, max_capture_bytes)));
    let stderr_task = process
        .take_stderr()
        .map(|stderr| tokio::spawn(capture_reader(stderr, max_capture_bytes)));

    let mut timed_out = false;
    let mut wait_error = None;
    let status = match timeout(Duration::from_millis(timeout_ms), process.wait()).await {
        Ok(Ok(status)) => Some(status),
        Ok(Err(error)) => {
            wait_error = Some(error.to_string());
            process.terminate_tree().await;
            process.wait().await.ok()
        }
        Err(_) => {
            timed_out = true;
            process.terminate_tree().await;
            process.wait().await.ok()
        }
    };
    process.disarm().await;

    let stdout_capture = finish_capture(stdout_task, "stdout").await;
    let stderr_capture = finish_capture(stderr_task, "stderr").await;
    let stdout = stdout_capture.text;
    let mut stderr = stderr_capture.text;

    if let Some(error) = wait_error.as_deref() {
        append_stderr_diagnostic(
            &mut stderr,
            &format!("Failed while waiting for command: {error}"),
        );
    }
    if let Some(error) = stdout_capture.read_error.as_deref() {
        append_stderr_diagnostic(
            &mut stderr,
            &format!("CatDesk failed to read stdout: {error}"),
        );
    }
    if let Some(error) = stderr_capture.read_error.as_deref() {
        append_stderr_diagnostic(
            &mut stderr,
            &format!("CatDesk failed to read stderr: {error}"),
        );
    }
    if timed_out {
        append_stderr_diagnostic(
            &mut stderr,
            &format!("Command timed out after {timeout_ms} ms"),
        );
    }

    let exit_code = status.as_ref().and_then(std::process::ExitStatus::code);
    let success = wait_error.is_none()
        && !timed_out
        && status
            .as_ref()
            .is_some_and(std::process::ExitStatus::success);

    ProcessRunResult {
        stdout,
        stderr,
        success,
        exit_code,
        elapsed_ms: started.elapsed().as_millis() as u64,
        timed_out,
        stdout_truncated: stdout_capture.truncated,
        stderr_truncated: stderr_capture.truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;
    use uuid::Uuid;

    struct PartialThenError {
        emitted: bool,
    }

    impl AsyncRead for PartialThenError {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if !self.emitted {
                self.emitted = true;
                buf.put_slice(b"partial-output");
                Poll::Ready(Ok(()))
            } else {
                Poll::Ready(Err(io::Error::other("synthetic read failure")))
            }
        }
    }

    fn workspace(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("catdesk-process-{name}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create test workspace");
        path
    }

    #[test]
    fn sandbox_toggle_selects_linux_sandbox_only_when_enabled() {
        assert_eq!(uses_linux_sandbox(true), cfg!(target_os = "linux"));
        assert!(!uses_linux_sandbox(false));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn disabled_sandbox_uses_direct_bash_on_linux() {
        let root = workspace("direct-bash");
        let prepared = shell_command("true", &root, &root, false).expect("prepare shell command");
        assert_eq!(prepared.command.as_std().get_program(), "/bin/bash");
        assert!(prepared.cleanup_dir.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parent_death_signal_child_outlives_blocking_worker_retirement() {
        use std::os::unix::process::CommandExt;

        let root = workspace("blocking-worker-parent");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .thread_keep_alive(std::time::Duration::from_millis(50))
            .enable_all()
            .build()
            .expect("build regression-test runtime");

        runtime.block_on(async {
            let prepared = tokio::task::spawn_blocking(|| {
                let mut command = Command::new("/bin/bash");
                command.arg("-c").arg("sleep 0.25; printf 'survived\\n'");
                unsafe {
                    command.as_std_mut().pre_exec(|| {
                        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                            return Err(io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                PreparedShellCommand {
                    command,
                    cleanup_dir: None,
                }
            })
            .await
            .expect("prepare task joined");

            let mut process = spawn_prepared_shell_command(prepared, &root).expect("spawn command");
            let stdout = process.take_stdout().expect("command stdout");
            let output_task = tokio::spawn(capture_reader(stdout, 1024));
            let status = process.wait().await.expect("wait for command");
            process.disarm().await;
            let output = output_task.await.expect("join stdout capture");

            assert!(status.success(), "command was killed: {status}");
            assert_eq!(output.text.trim(), "survived");
        });

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn capture_reader_preserves_partial_output_on_read_error() {
        let captured = capture_reader(PartialThenError { emitted: false }, 1024).await;
        assert_eq!(captured.text, "partial-output");
        assert!(!captured.truncated);
        assert_eq!(
            captured.read_error.as_deref(),
            Some("synthetic read failure")
        );
    }

    #[tokio::test]
    async fn run_shell_command_captures_output_and_exit_status() {
        let root = workspace("success");
        let command = if cfg!(windows) {
            "Write-Output 'hello'"
        } else {
            "printf 'hello\\n'"
        };
        let result = run_shell_command(command, &root, &root, false, 5_000, 1024).await;
        assert!(result.success, "stderr: {}", result.stderr);
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout.trim(), "hello");
        assert!(!result.timed_out);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn large_stdout_and_stderr_are_drained_without_deadlock_and_bounded() {
        let root = workspace("bounded-output");
        let command = if cfg!(windows) {
            "[Console]::Out.Write(('x' * 200000)); [Console]::Error.Write(('y' * 200000))"
        } else {
            "printf '%*s' 200000 ''; printf '%*s' 200000 '' >&2"
        };
        let result = run_shell_command(command, &root, &root, false, 5_000, 4_096).await;
        assert!(
            result.success,
            "large-output command failed: {}",
            result.stderr
        );
        assert!(result.stdout.len() <= 4_096);
        assert!(result.stderr.len() <= 4_096);
        assert!(result.stdout_truncated);
        assert!(result.stderr_truncated);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn timed_out_command_cannot_continue_after_return() {
        let root = workspace("timeout");
        let sentinel = root.join("sentinel.txt");
        let command = if cfg!(windows) {
            "Start-Sleep -Milliseconds 700; Set-Content -Path sentinel.txt -Value survived"
        } else {
            "sleep 0.7; printf survived > sentinel.txt"
        };
        let result = run_shell_command(command, &root, &root, false, 100, 1024).await;
        assert!(result.timed_out);
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(
            !sentinel.exists(),
            "timed-out process survived and wrote sentinel"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn timeout_terminates_descendant_process_tree() {
        let root = workspace("descendant-timeout");
        let sentinel = root.join("descendant.txt");
        let command = if cfg!(windows) {
            "Start-Process powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep -Milliseconds 800; Set-Content -Path descendant.txt -Value survived' -WorkingDirectory .; Start-Sleep -Seconds 5"
        } else {
            "(sleep 0.8; printf survived > descendant.txt) & sleep 5"
        };
        let result = run_shell_command(command, &root, &root, false, 150, 1024).await;
        assert!(result.timed_out);
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        assert!(
            !sentinel.exists(),
            "timed-out root shell left a descendant process alive"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn successful_root_exit_cannot_leave_detached_descendant_alive() {
        let root = workspace("detached-success");
        let sentinel = root.join("detached.txt");
        let command = if cfg!(windows) {
            "Start-Process powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep -Milliseconds 800; Set-Content -Path detached.txt -Value survived' -WorkingDirectory .; Write-Output root-done"
        } else {
            "(sleep 0.8; printf survived > detached.txt) & printf 'root-done\\n'"
        };
        let result = run_shell_command(command, &root, &root, false, 5_000, 1024).await;
        assert!(result.success, "root command failed: {}", result.stderr);
        assert!(result.stdout.contains("root-done"));
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        assert!(
            !sentinel.exists(),
            "successful root shell detached a descendant outside CatDesk ownership"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn dropping_run_future_terminates_the_process() {
        let root = workspace("drop");
        let sentinel = root.join("sentinel.txt");
        let command = if cfg!(windows) {
            "Start-Sleep -Milliseconds 700; Set-Content -Path sentinel.txt -Value survived"
        } else {
            "sleep 0.7; printf survived > sentinel.txt"
        };
        let root_for_task = root.clone();
        let task = tokio::spawn(async move {
            run_shell_command(command, &root_for_task, &root_for_task, false, 5_000, 1024).await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        task.abort();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(
            !sentinel.exists(),
            "dropped command future left the process alive"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
