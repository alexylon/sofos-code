//! Process spawn and result-shaping for the bash executor. The
//! permission gate ([`BashExecutor::execute`]) decides whether to run
//! the command at all; [`BashExecutor::execute_after_permission_check`]
//! is the path that actually spawns `sh -c <command>`, applies the
//! per-stream output caps from [`super::output`], and renders the
//! result string the model sees.

use crate::error::{Result, SofosError};
use crate::tools::bash::BashExecutor;
use crate::tools::bash::output::{
    BASH_COMMAND_TIMEOUT, BASH_READ_CHUNK_BYTES, MAX_BASH_OUTPUT_BYTES, SUPERVISOR_POLL_INTERVAL,
    TERMINATION_GRACE_PERIOD, TerminationReason,
};
use crate::tools::bash::validate::command_contains_op;
use crate::tools::permissions::{CommandPermission, PermissionManager};
use crate::tools::utils::{MAX_TOOL_OUTPUT_TOKENS, TruncationKind, truncate_for_context};
use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

impl BashExecutor {
    pub fn new(workspace: PathBuf, interactive: bool, has_morph: bool) -> Result<Self> {
        // Resolve symlinks on the workspace itself so that the path-escape
        // check in `check_workspace_relative_escape` compares two
        // canonical paths. Without this, macOS workspaces under
        // `/var/folders/...` would compare against canonical
        // `/private/var/folders/...` and every workspace-relative
        // resolution would look like an escape.
        let workspace = std::fs::canonicalize(&workspace).unwrap_or(workspace);
        Ok(Self {
            workspace,
            interactive,
            has_morph,
            session_allowed: Arc::new(Mutex::new(HashSet::new())),
            session_denied: Arc::new(Mutex::new(HashSet::new())),
            bash_path_session_allowed: Arc::new(Mutex::new(HashSet::new())),
            bash_path_session_denied: Arc::new(Mutex::new(HashSet::new())),
            interrupt_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn execute(&self, command: &str) -> Result<String> {
        let normalized = format!("Bash({})", command.trim());

        // Check session-scoped decisions first (for "allow once" / "deny once")
        if let Ok(allowed) = self.session_allowed.lock() {
            if allowed.contains(&normalized) {
                // Previously allowed this session, skip permission check
                return self.execute_after_permission_check(command);
            }
        }
        if let Ok(denied) = self.session_denied.lock() {
            if denied.contains(&normalized) {
                return Err(SofosError::ToolExecution(format!(
                    "User already declined '{}' earlier this session. \
                     Propose a different approach or ask the user to clarify \
                     rather than retrying the same command.",
                    command
                )));
            }
        }

        let mut permission_manager = PermissionManager::new(self.workspace.clone())?;
        let permission = permission_manager.check_command_permission(command)?;

        match permission {
            CommandPermission::Allowed => {
                // Command is in allowed list, execute directly
            }
            CommandPermission::Denied => {
                return Err(SofosError::ToolExecution(
                    self.get_rejection_reason(command),
                ));
            }
            CommandPermission::Ask => {
                let (allowed, remember) = permission_manager.ask_user_permission(command)?;
                if !allowed {
                    if !remember {
                        // Store session-scoped denial
                        if let Ok(mut denied) = self.session_denied.lock() {
                            denied.insert(normalized);
                        }
                    }
                    return Err(SofosError::ToolExecution(format!(
                        "User declined '{}'. Propose a different approach or \
                         ask the user to clarify rather than retrying the same \
                         command.",
                        command
                    )));
                }
                if !remember {
                    // Store session-scoped allowance
                    if let Ok(mut allowed) = self.session_allowed.lock() {
                        allowed.insert(normalized);
                    }
                }
            }
        }

        self.execute_after_permission_check(command)
    }

    fn execute_after_permission_check(&self, command: &str) -> Result<String> {
        let mut permission_manager = PermissionManager::new(self.workspace.clone())?;

        // Enforce read permissions on paths referenced in the command
        self.enforce_read_permissions(&permission_manager, command)?;

        // Non-path structural safety checks (parent traversal, redirection, git restrictions)
        if !self.is_safe_command_structure(command) {
            return Err(SofosError::ToolExecution(
                self.get_rejection_reason(command),
            ));
        }

        // Commands that aren't destructive enough to hard-deny but
        // mutate working-tree state in a way the user should see before
        // it happens — e.g. `git checkout <branch>` switches branches,
        // `git checkout HEAD~N` detaches HEAD, `git checkout -- <path>`
        // overwrites uncommitted changes. Fires AFTER the structural
        // hard-deny above so `git checkout -f` / `git checkout -b`
        // stay hard-blocked instead of being askable.
        self.confirm_askable_command(command)?;

        // Check external paths in command — ask user for paths not covered by Bash path grants
        self.check_bash_external_paths(command, &mut permission_manager)?;

        let outcome = self.spawn_supervised(command)?;

        if let Some(reason) = outcome.terminated_for {
            return match reason {
                TerminationReason::StdoutCapExceeded | TerminationReason::StderrCapExceeded => {
                    let stream = if reason == TerminationReason::StdoutCapExceeded {
                        "output"
                    } else {
                        "error output"
                    };
                    Err(SofosError::ToolExecution(format!(
                        "Command {} too large (exceeded {} MB cap). The process was terminated.",
                        stream,
                        MAX_BASH_OUTPUT_BYTES / (1024 * 1024)
                    )))
                }
                TerminationReason::Timeout => Err(SofosError::ToolExecution(format!(
                    "Command exceeded the {} second time limit and was terminated.",
                    BASH_COMMAND_TIMEOUT.as_secs()
                ))),
                TerminationReason::Interrupt => Err(SofosError::ToolExecution(
                    "Command was interrupted by the user before it finished.".to_string(),
                )),
            };
        }

        let stdout = String::from_utf8_lossy(&outcome.stdout);
        let stderr = String::from_utf8_lossy(&outcome.stderr);

        if !outcome.status.success() {
            let exit_info = match outcome.status.code() {
                Some(code) => format!("exit code: {}", code),
                None => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        match outcome.status.signal() {
                            Some(sig) => format!(
                                "signal: {} ({})",
                                sig,
                                crate::tools::bash::output::signal_name(sig)
                            ),
                            None => "unknown termination".to_string(),
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        "unknown termination".to_string()
                    }
                }
            };
            let error_output = format!(
                "Command failed with {}\nSTDOUT:\n{}\nSTDERR:\n{}",
                exit_info, stdout, stderr
            );
            return Ok(truncate_for_context(
                &error_output,
                MAX_TOOL_OUTPUT_TOKENS,
                TruncationKind::BashOutput,
            ));
        }

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str("STDOUT:\n");
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str("STDERR:\n");
            result.push_str(&stderr);
        }

        if result.is_empty() {
            result = "Command executed successfully (no output)".to_string();
        }

        Ok(truncate_for_context(
            &result,
            MAX_TOOL_OUTPUT_TOKENS,
            TruncationKind::BashOutput,
        ))
    }

    fn spawn_supervised(&self, command: &str) -> Result<SupervisedOutput> {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| SofosError::ToolExecution(format!("Failed to execute command: {}", e)))?;

        let stdout = child.stdout.take().ok_or_else(|| {
            SofosError::ToolExecution("Failed to capture command stdout".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            SofosError::ToolExecution("Failed to capture command stderr".to_string())
        })?;

        let stdout_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let stderr_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let stdout_overflow = Arc::new(AtomicBool::new(false));
        let stderr_overflow = Arc::new(AtomicBool::new(false));

        let stdout_handle = spawn_capped_reader(
            stdout,
            Arc::clone(&stdout_buf),
            Arc::clone(&stdout_overflow),
        );
        let stderr_handle = spawn_capped_reader(
            stderr,
            Arc::clone(&stderr_buf),
            Arc::clone(&stderr_overflow),
        );

        let start = Instant::now();
        let mut termination: Option<TerminationReason> = None;

        let mut try_wait_error: Option<std::io::Error> = None;

        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {}
                Err(e) => {
                    try_wait_error = Some(e);
                    break;
                }
            }

            if self.interrupt_flag.load(Ordering::SeqCst) {
                termination = Some(TerminationReason::Interrupt);
                break;
            }
            if start.elapsed() > BASH_COMMAND_TIMEOUT {
                termination = Some(TerminationReason::Timeout);
                break;
            }
            if stdout_overflow.load(Ordering::SeqCst) {
                termination = Some(TerminationReason::StdoutCapExceeded);
                break;
            }
            if stderr_overflow.load(Ordering::SeqCst) {
                termination = Some(TerminationReason::StderrCapExceeded);
                break;
            }

            thread::sleep(SUPERVISOR_POLL_INTERVAL);
        }

        // Always tear the child tree down on every error or termination
        // path before returning. Without this, a `try_wait` failure
        // would leave the child orphaned and the reader threads alive,
        // and a `wait` failure would prevent the reader threads from
        // unblocking even though we have already moved on.
        if termination.is_some() || try_wait_error.is_some() {
            terminate_child_tree(&mut child);
        }

        let wait_result = child.wait();
        let _ = stdout_handle.join();
        let _ = stderr_handle.join();

        // Readers drain the pipe to EOF, so the overflow flag now
        // reflects the full output. Recheck because a child that
        // exits faster than one supervisor tick breaks the loop via
        // try_wait before the in-loop overflow check has run.
        if termination.is_none() {
            if stdout_overflow.load(Ordering::SeqCst) {
                termination = Some(TerminationReason::StdoutCapExceeded);
            } else if stderr_overflow.load(Ordering::SeqCst) {
                termination = Some(TerminationReason::StderrCapExceeded);
            }
        }

        if let Some(e) = try_wait_error {
            return Err(SofosError::ToolExecution(format!(
                "Failed to wait on command: {}",
                e
            )));
        }
        let status = wait_result
            .map_err(|e| SofosError::ToolExecution(format!("Failed to reap command: {}", e)))?;

        let stdout = drain_into_vec(stdout_buf);
        let stderr = drain_into_vec(stderr_buf);

        Ok(SupervisedOutput {
            stdout,
            stderr,
            status,
            terminated_for: termination,
        })
    }

    /// Prompt the user before running commands that mutate working-tree
    /// state in a way that's easy to overlook. Currently just
    /// `git checkout <anything>` — plain branch switches, detached-HEAD
    /// checkouts, and `git checkout -- <path>` file recovery all land
    /// here. Hard-denied variants (`git checkout -f`, `git checkout -b`)
    /// are filtered out earlier by `is_safe_command_structure`.
    ///
    /// Declining the prompt aborts the command. Accepting is scoped to
    /// this one invocation — the user has to confirm each `git
    /// checkout` explicitly, matching `confirm_destructive`'s policy of
    /// "no remember button for working-tree mutations".
    fn confirm_askable_command(&self, command: &str) -> Result<()> {
        const ASKABLE_PREFIXES: &[&str] = &["git checkout"];

        let matches = ASKABLE_PREFIXES
            .iter()
            .any(|prefix| command_contains_op(command, prefix));
        if !matches {
            return Ok(());
        }

        if !self.interactive {
            return Err(SofosError::ToolExecution(format!(
                "Command '{}' requires interactive confirmation\n\
                 Hint: `git checkout` prompts before running because it switches branches \
                 (or overwrites working-tree files). Run sofos interactively to confirm.",
                command
            )));
        }

        let prompt = format!("Run bash command: {}", command);
        if !crate::tools::utils::confirm_destructive(&prompt)? {
            return Err(SofosError::ToolExecution(format!(
                "User declined '{}'. Propose a different approach or ask \
                 the user to clarify rather than retrying the same command.",
                command
            )));
        }
        Ok(())
    }
}

struct SupervisedOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: ExitStatus,
    terminated_for: Option<TerminationReason>,
}

fn spawn_capped_reader<R>(
    reader: R,
    buf: Arc<Mutex<Vec<u8>>>,
    overflow: Arc<AtomicBool>,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || read_capped(reader, &buf, &overflow))
}

fn read_capped<R: Read>(mut reader: R, buf: &Mutex<Vec<u8>>, overflow: &AtomicBool) {
    let mut chunk = [0u8; BASH_READ_CHUNK_BYTES];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                if overflow.load(Ordering::Relaxed) {
                    continue;
                }
                let Ok(mut stored) = buf.lock() else {
                    return;
                };
                let remaining = MAX_BASH_OUTPUT_BYTES.saturating_sub(stored.len());
                if remaining == 0 {
                    overflow.store(true, Ordering::SeqCst);
                    continue;
                }
                let take = read.min(remaining);
                stored.extend_from_slice(&chunk[..take]);
                if take < read {
                    overflow.store(true, Ordering::SeqCst);
                }
            }
            Err(_) => break,
        }
    }
}

fn drain_into_vec(buf: Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    Arc::try_unwrap(buf)
        .map(|inner| inner.into_inner().unwrap_or_default())
        .unwrap_or_else(|shared| shared.lock().map(|guard| guard.clone()).unwrap_or_default())
}

#[cfg(unix)]
fn terminate_child_tree(child: &mut Child) {
    let pid = child.id() as i32;
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    thread::sleep(TERMINATION_GRACE_PERIOD);
    if let Ok(None) = child.try_wait() {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn terminate_child_tree(child: &mut Child) {
    let _ = child.kill();
}
