//! Process spawn and result-shaping for the bash executor. The
//! permission gate ([`BashExecutor::execute`]) decides whether to run
//! the command at all; [`BashExecutor::execute_after_permission_check`]
//! is the path that actually spawns `sh -c <command>`, applies the
//! per-stream output caps from [`super::output`], and renders the
//! result string the model sees.

use crate::config::SandboxMode;
use crate::error::{Result, SofosError};
use crate::tools::bash::BashExecutor;
#[cfg(unix)]
use crate::tools::bash::output::TERMINATION_GRACE_PERIOD;
use crate::tools::bash::output::{
    BASH_COMMAND_TIMEOUT, BASH_READ_CHUNK_BYTES, MAX_BASH_OUTPUT_BYTES, SUPERVISOR_POLL_INTERVAL,
    TerminationReason,
};
use crate::tools::bash::sandbox::{self, SandboxPolicy};
use crate::tools::bash::validate::{
    command_contains_askable_git_checkout, detect_command_substitution, has_path_traversal,
};
use crate::tools::permissions::{CommandPermission, PermissionManager};
use crate::tools::utils::{
    MAX_TOOL_OUTPUT_TOKENS, TruncationKind, normalize_command_whitespace, truncate_for_context,
};
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

/// Resolved shell interpreter sofos shells out to. `program` is what
/// gets passed to [`Command::new`]; `extra_path_dir` is a directory
/// that must be prepended to the child's `PATH` so the interpreter
/// can find sibling utilities (the GNU `cat`, `sleep`, `seq` shipped
/// alongside Git for Windows's `sh.exe`).
struct ResolvedShell {
    program: OsString,
    extra_path_dir: Option<PathBuf>,
}

/// Locate the `sh` interpreter sofos shells out to. On Unix the plain
/// name is enough (always at `/bin/sh`). On Windows we honour `PATH`
/// first, then fall back to standard Git for Windows install locations —
/// IDE-integrated terminals (JetBrains, VS Code) and the default
/// `PATH` of a stock `cmd` or PowerShell session often expose only
/// `<git>\cmd` (which contains `git.exe`) and not `<git>\usr\bin`
/// (which contains `sh.exe` and the GNU userland). When we fall back
/// to the well-known path we also surface the parent directory so the
/// caller can prepend it to the child's `PATH`, otherwise the running
/// `sh.exe` would have no way to find `cat`, `sleep`, `seq`, etc.
fn resolve_shell() -> ResolvedShell {
    #[cfg(windows)]
    {
        if let Some(path_env) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path_env) {
                if dir.join("sh.exe").is_file() {
                    return ResolvedShell {
                        program: OsString::from("sh"),
                        extra_path_dir: None,
                    };
                }
            }
        }
        for candidate in WINDOWS_SHELL_FALLBACKS {
            let path = std::path::Path::new(candidate);
            if path.is_file() {
                return ResolvedShell {
                    program: OsString::from(candidate),
                    extra_path_dir: path.parent().map(PathBuf::from),
                };
            }
        }
        ResolvedShell {
            program: OsString::from("sh"),
            extra_path_dir: None,
        }
    }
    #[cfg(not(windows))]
    {
        ResolvedShell {
            program: OsString::from("sh"),
            extra_path_dir: None,
        }
    }
}

/// Standard install locations for the Git for Windows `sh.exe`. Checked
/// in order when `PATH` does not already expose `sh`.
#[cfg(windows)]
const WINDOWS_SHELL_FALLBACKS: &[&str] = &[
    r"C:\Program Files\Git\usr\bin\sh.exe",
    r"C:\Program Files (x86)\Git\usr\bin\sh.exe",
];

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
            mode: SandboxMode::Unrestricted,
            session_allowed: Arc::new(Mutex::new(HashSet::new())),
            session_denied: Arc::new(Mutex::new(HashSet::new())),
            bash_path_session_allowed: Arc::new(Mutex::new(HashSet::new())),
            bash_path_session_denied: Arc::new(Mutex::new(HashSet::new())),
            interrupt_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn execute(&self, command: &str) -> Result<String> {
        let mut permission_manager = PermissionManager::new(self.workspace.clone())?;
        let normalized = PermissionManager::normalize_command_key(command);

        // Check session-scoped decisions first (for "allow once" / "deny once")
        if let Ok(allowed) = self.session_allowed.lock() {
            if allowed.contains(&normalized) {
                // Previously allowed this session, skip permission check
                return self.execute_after_permission_check(
                    command,
                    &mut permission_manager,
                    false,
                );
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
                if self.sandbox_active() {
                    // Run it confined instead of prompting. The sandbox
                    // bounds writes and the network but not reads, so the
                    // gates in execute_after_permission_check still run.
                    return self.execute_after_permission_check(
                        command,
                        &mut permission_manager,
                        true,
                    );
                }
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

        self.execute_after_permission_check(command, &mut permission_manager, false)
    }

    /// True when shell commands are confined here: workspace mode plus a
    /// usable OS sandbox on this machine.
    fn sandbox_active(&self) -> bool {
        self.mode.is_sandboxed() && sandbox::is_available()
    }

    /// Run the read, structural, and external-path gates, then spawn the
    /// command. `force_confine` skips the unconfined fast path so an
    /// unknown command always runs inside the sandbox.
    fn execute_after_permission_check(
        &self,
        command: &str,
        permission_manager: &mut PermissionManager,
        force_confine: bool,
    ) -> Result<String> {
        // Enforce read permissions on paths referenced in the command
        self.enforce_read_permissions(permission_manager, command)?;

        // Structural safety checks (parent traversal, redirection,
        // substitution, git restrictions). In workspace mode a command
        // rejected only for writing to a file — redirection or a
        // here-document — can still run confined, since the sandbox keeps
        // the write inside the workspace. Traversal, hidden subcommands,
        // and dangerous git operations are never relaxed.
        let confine = if !force_confine && self.is_safe_command_structure(command) {
            false
        } else if self.sandbox_active() && self.is_confinement_safe(command) {
            true
        } else {
            return Err(SofosError::ToolExecution(
                self.get_rejection_reason(command),
            ));
        };

        // Commands that aren't destructive enough to hard-deny but
        // mutate working-tree state in a way the user should see before
        // it happens — e.g. `git checkout <branch>` switches branches,
        // `git checkout HEAD~N` detaches HEAD, `git checkout -- <path>`
        // overwrites uncommitted changes. Fires AFTER the structural
        // hard-deny above so `git checkout -f` / `git checkout -b`
        // stay hard-blocked instead of being askable.
        self.confirm_askable_command(command)?;

        // Check external paths in command — ask user for paths not covered by Bash path grants
        self.check_bash_external_paths(command, permission_manager)?;

        self.run_and_shape(command, confine)
    }

    /// Whether a command that failed [`Self::is_safe_command_structure`]
    /// is safe to run confined. The only structural problems allowed here
    /// are file output redirection and here-documents, both of which the
    /// sandbox keeps inside the workspace. Parent-directory traversal,
    /// hidden subcommands, and dangerous git operations are never relaxed.
    fn is_confinement_safe(&self, command: &str) -> bool {
        if has_path_traversal(command) || detect_command_substitution(command).is_some() {
            return false;
        }
        let matcher_input = normalize_command_whitespace(command).to_lowercase();
        self.is_safe_git_command(&matcher_input)
    }

    /// Run a command (optionally confined to the workspace) and turn the
    /// supervised outcome into the string the model sees. The command is
    /// assumed to have already cleared whatever permission and structural
    /// checks apply to it.
    fn run_and_shape(&self, command: &str, confine: bool) -> Result<String> {
        let outcome = self.spawn_supervised(command, confine)?;

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

    /// Resolve the program and arguments to spawn: the bare shell, or the
    /// shell wrapped by the operating-system sandbox when `confine` is
    /// set. When a command must be confined but the sandbox cannot be
    /// prepared for it — for example a protected path the profile cannot
    /// express safely — this refuses the command rather than running it
    /// unconfined.
    fn shell_invocation(
        &self,
        shell: &ResolvedShell,
        command: &str,
        confine: bool,
    ) -> Result<(OsString, Vec<OsString>)> {
        if confine {
            let policy = self.confined_policy();
            return sandbox::confined_invocation(&shell.program, command, &policy).ok_or_else(
                || {
                    SofosError::ToolExecution(
                        "This command must run confined to the workspace, but the sandbox \
                     could not be prepared for it. This can happen when a path the \
                     sandbox protects contains an unusual control character.\n\
                     Hint: rename that path, or switch to unrestricted mode if you \
                     trust the command."
                            .to_string(),
                    )
                },
            );
        }
        Ok((
            shell.program.clone(),
            vec![OsString::from("-c"), OsString::from(command)],
        ))
    }

    /// The sandbox policy for a confined command: writes bounded to the
    /// workspace, the network closed, and the workspace's `Read(...)` deny
    /// rules enforced by the kernel.
    fn confined_policy(&self) -> SandboxPolicy {
        let policy = SandboxPolicy::for_workspace(&self.workspace);
        match PermissionManager::new(self.workspace.clone()) {
            Ok(manager) => {
                let (deny, allow) = manager.sandbox_read_rules();
                policy.with_read_rules(&self.workspace, &deny, &allow)
            }
            Err(_) => policy,
        }
    }

    fn spawn_supervised(&self, command: &str, confine: bool) -> Result<SupervisedOutput> {
        #[cfg(windows)]
        if confine && sandbox::is_available() {
            return self.spawn_supervised_windows(command);
        }

        let shell = resolve_shell();
        let (program, args) = self.shell_invocation(&shell, command, confine)?;

        // A confined Linux command masks any not-yet-existing metadata
        // directory with a read-only tmpfs, which bwrap materialises as an
        // empty mount point on the real workspace. Record the absent ones
        // now so the empty leftovers can be removed once the command exits.
        #[cfg(target_os = "linux")]
        let metadata_cleanup: Vec<PathBuf> = if confine {
            self.confined_policy()
                .write_protect_subpaths
                .into_iter()
                .filter(|path| !path.exists())
                .collect()
        } else {
            Vec::new()
        };

        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(extra) = shell.extra_path_dir.as_ref() {
            let original = std::env::var_os("PATH").unwrap_or_default();
            let mut dirs: Vec<PathBuf> = vec![extra.clone()];
            dirs.extend(std::env::split_paths(&original));
            if let Ok(joined) = std::env::join_paths(dirs) {
                cmd.env("PATH", joined);
            }
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // A confined Linux command also gets the network seccomp
            // filter, installed in the child so bubblewrap and everything
            // it spawns inherit it. The program is built here in the
            // parent because the child must not allocate after `fork`.
            #[cfg(target_os = "linux")]
            let seccomp = confine.then(sandbox::network_seccomp_program).flatten();
            unsafe {
                cmd.pre_exec(move || {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    #[cfg(target_os = "linux")]
                    if let Some(program) = &seccomp {
                        sandbox::apply_network_seccomp(program)?;
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

        // Remove the empty mount points the read-only tmpfs masks left for
        // metadata directories that did not exist before the run. remove_dir
        // deletes only an empty directory, so anything that already held
        // content is left untouched.
        #[cfg(target_os = "linux")]
        for path in &metadata_cleanup {
            let _ = std::fs::remove_dir(path);
        }

        Ok(SupervisedOutput {
            stdout,
            stderr,
            status,
            terminated_for: termination,
        })
    }

    /// Spawn the shell under the Windows workspace sandbox and produce
    /// the same [`SupervisedOutput`] the Unix branch returns.
    #[cfg(windows)]
    fn spawn_supervised_windows(&self, command: &str) -> Result<SupervisedOutput> {
        use std::os::windows::process::ExitStatusExt;

        let shell = resolve_shell();
        let policy = SandboxPolicy::for_workspace(&self.workspace);
        let extra_path = shell.extra_path_dir.as_deref();
        let outcome = sandbox::windows::run_confined(
            &shell.program,
            command,
            &self.workspace,
            extra_path,
            &policy,
            &self.interrupt_flag,
        )
        .map_err(|e| SofosError::ToolExecution(format!("Failed to execute command: {}", e)))?;
        let status = ExitStatus::from_raw(outcome.exit_code.unwrap_or(1) as u32);
        Ok(SupervisedOutput {
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            status,
            terminated_for: outcome.terminated_for,
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
        // Normalise before semantic Git matching so whitespace tricks,
        // spelling tricks, and Git global options do not skip the prompt.
        let matches = command_contains_askable_git_checkout(
            &normalize_command_whitespace(command).to_lowercase(),
        );
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
    // Windows has no process group, so `child.kill()` alone would
    // terminate only the `sh.exe` wrapper and orphan its grandchildren
    // (e.g. a `sleep` launched by the script). `taskkill /F /T /PID`
    // walks the process tree rooted at the wrapper and kills every
    // descendant, matching the Unix branch above. `child.kill()` runs
    // afterwards as a fallback for environments where `taskkill` is
    // unavailable (locked-down systems, unconventional `PATH`).
    let pid = child.id().to_string();
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
}
