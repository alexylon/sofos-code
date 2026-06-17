//! Process spawn and result-shaping for the bash executor. The
//! permission gate ([`BashExecutor::execute`]) decides whether to run
//! the command at all; [`BashExecutor::execute_after_permission_check`]
//! is the path that actually spawns `sh -c <command>`, applies the
//! per-stream output caps from [`super::output`], and renders the
//! result string the model sees.

use crate::config::{ApprovalPolicy, SandboxMode};
use crate::error::{Result, SofosError};
#[cfg(unix)]
use crate::tools::bash::output::TERMINATION_GRACE_PERIOD;
use crate::tools::bash::output::{
    BASH_COMMAND_TIMEOUT, BASH_READ_CHUNK_BYTES, MAX_BASH_OUTPUT_BYTES, SUPERVISOR_POLL_INTERVAL,
    TerminationReason,
};
use crate::tools::bash::sandbox::{self, SandboxPolicy};
use crate::tools::bash::validate::{
    command_contains_askable_git_checkout, command_runs_only_git, detect_command_substitution,
    has_path_traversal,
};
use crate::tools::bash::{BashExecutor, EscalationRequest};
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
            approval_policy: ApprovalPolicy::default(),
            session_allowed: Arc::new(Mutex::new(HashSet::new())),
            session_denied: Arc::new(Mutex::new(HashSet::new())),
            session_unsandboxed: Arc::new(Mutex::new(HashSet::new())),
            bash_path_session_allowed: Arc::new(Mutex::new(HashSet::new())),
            bash_path_session_denied: Arc::new(Mutex::new(HashSet::new())),
            interrupt_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn execute(&self, command: &str) -> Result<String> {
        self.execute_with_escalation(command, None)
    }

    /// Like [`Self::execute`], but `escalation` carries a model request to
    /// run the command outside the sandbox. Forbidden commands are still
    /// refused; a granted escalation runs this one command unsandboxed
    /// after explicit user approval (see [`Self::run_model_escalation`]).
    pub fn execute_with_escalation(
        &self,
        command: &str,
        escalation: Option<EscalationRequest>,
    ) -> Result<String> {
        let mut permission_manager = PermissionManager::new(self.workspace.clone())?;
        let normalized = PermissionManager::normalize_command_key(command);

        // Check session-scoped decisions first (for "allow once" / "deny once")
        if let Ok(allowed) = self.session_allowed.lock() {
            if allowed.contains(&normalized) {
                // Previously allowed this session, skip permission check
                return self.execute_after_permission_check(command, &mut permission_manager);
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

        // Forbidden commands are refused regardless of any escalation request.
        if matches!(permission, CommandPermission::Denied) {
            return Err(SofosError::ToolExecution(
                self.get_rejection_reason(command),
            ));
        }

        // A model-driven escalation request runs this one command outside
        // the sandbox after explicit user approval. There is only something
        // to escalate out of when a sandbox is engaged; otherwise fall
        // through to the normal flow, where the command already runs
        // unconfined.
        if let Some(escalation) = &escalation {
            if self.sandbox_active() {
                return self.run_model_escalation(
                    command,
                    &normalized,
                    escalation,
                    &mut permission_manager,
                );
            }
        }

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
                    return self.execute_after_permission_check(command, &mut permission_manager);
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

        self.execute_after_permission_check(command, &mut permission_manager)
    }

    /// True when shell commands are confined here: workspace mode plus a
    /// usable OS sandbox on this machine.
    fn sandbox_active(&self) -> bool {
        self.mode.is_sandboxed() && sandbox::is_available()
    }

    /// Run the read, structural, and external-path gates, then spawn the
    /// command, confined to the workspace when a sandbox is engaged.
    fn execute_after_permission_check(
        &self,
        command: &str,
        permission_manager: &mut PermissionManager,
    ) -> Result<String> {
        // Enforce read permissions on paths referenced in the command
        self.enforce_read_permissions(permission_manager, command)?;

        let confine = self.should_confine(command, self.sandbox_active())?;

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

        let normalized = PermissionManager::normalize_command_key(command);
        self.run_and_shape(command, confine, &normalized)
    }

    /// Decide how a command runs: `Ok(true)` to confine it to the
    /// workspace, `Ok(false)` to run it directly without the sandbox, and
    /// `Err` to refuse it.
    ///
    /// When a sandbox is engaged (`sandbox_active`), every command runs
    /// confined, so writes stay inside the workspace and temporary
    /// directories and the network is closed — for familiar build tools
    /// and interpreters just as for unfamiliar commands. Output
    /// redirection and here-documents are accepted here because the
    /// sandbox keeps the write inside the workspace. Without a sandbox
    /// there is nothing to bound a command, so only structurally safe
    /// commands run, unconfined, and the rest are refused. Parent
    /// traversal, hidden subcommands, and dangerous git operations are
    /// refused either way.
    pub(super) fn should_confine(&self, command: &str, sandbox_active: bool) -> Result<bool> {
        if sandbox_active {
            if self.is_confinement_safe(command) {
                return Ok(true);
            }
        } else if self.is_safe_command_structure(command) {
            return Ok(false);
        }
        Err(SofosError::ToolExecution(
            self.get_rejection_reason(command),
        ))
    }

    /// Whether `command` is safe to run confined. File output redirection
    /// and here-documents are allowed because the sandbox keeps the write
    /// inside the workspace; parent-directory traversal, hidden
    /// subcommands, and dangerous git operations are refused even confined.
    fn is_confinement_safe(&self, command: &str) -> bool {
        if has_path_traversal(command) || detect_command_substitution(command).is_some() {
            return false;
        }
        self.is_safe_git_command(&normalize_command_whitespace(command))
    }

    /// Run a command (optionally confined to the workspace) and turn the
    /// supervised outcome into the string the model sees. The command is
    /// assumed to have already cleared whatever permission and structural
    /// checks apply to it.
    fn run_and_shape(&self, command: &str, confine: bool, normalized: &str) -> Result<String> {
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
            let mut error_output = format!(
                "Command failed with {}\nSTDOUT:\n{}\nSTDERR:\n{}",
                exit_info, stdout, stderr
            );
            if confine {
                if let Some(escalated) =
                    self.maybe_escalate_unsandboxed(command, normalized, &outcome)?
                {
                    return Ok(escalated);
                }
                error_output.push_str("\n\n");
                error_output.push_str(confined_command_failure_note());
            }
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

    /// On a confined failure that looks like a sandbox denial, offer to
    /// retry the same command without the sandbox. Returns
    /// `Ok(Some(output))` when the user approved (or a session grant
    /// already applied) — `output` is the unsandboxed run's shaped result,
    /// success or failure — and `Ok(None)` when escalation does not apply
    /// or the user declined, leaving the caller to return the original
    /// confined failure with its note.
    fn maybe_escalate_unsandboxed(
        &self,
        command: &str,
        normalized: &str,
        output: &SupervisedOutput,
    ) -> Result<Option<String>> {
        if !should_prompt_unsandboxed(self.approval_policy, self.interactive, true, output) {
            return Ok(None);
        }

        // Already approved for this session: rerun unsandboxed without
        // asking again.
        let cached = self
            .session_unsandboxed
            .lock()
            .map(|set| set.contains(normalized))
            .unwrap_or(false);
        if cached {
            return self.run_and_shape(command, false, normalized).map(Some);
        }

        let prompt = format!(
            "Command failed under the workspace sandbox and looks like a sandbox \
             denial (network, socket, or a protected write). Retry without the sandbox?\n  {command}"
        );
        let (approved, remember) = confirm_with_remember(&prompt)?;
        if !approved {
            return Ok(None);
        }
        if remember {
            if let Ok(mut set) = self.session_unsandboxed.lock() {
                set.insert(normalized.to_string());
            }
        }
        self.run_and_shape(command, false, normalized).map(Some)
    }

    /// Handle a model-driven request to run `command` outside the sandbox.
    /// The caller has already refused forbidden commands. Returns the
    /// command's output when it runs (escalation approved, unsandboxed), or
    /// an error the model can act on when the policy takes no escalation
    /// requests, the session is non-interactive, the structure is unsafe,
    /// or the user declines.
    fn run_model_escalation(
        &self,
        command: &str,
        normalized: &str,
        escalation: &EscalationRequest,
        permission_manager: &mut PermissionManager,
    ) -> Result<String> {
        if !self.approval_policy.allows_model_escalation_request() {
            return Err(SofosError::ToolExecution(format!(
                "Approval policy is '{}', which does not take escalation requests. \
                 Reissue the command without requesting escalated permissions.",
                self.approval_policy.label()
            )));
        }
        if !self.interactive {
            return Err(SofosError::ToolExecution(
                "Cannot approve out-of-sandbox execution in a non-interactive session. \
                 Rework the command to run inside the workspace, or ask the user to \
                 run with /unrestricted."
                    .to_string(),
            ));
        }

        // Escalation lifts the sandbox, not the other gates: parent
        // traversal, hidden subcommands, and dangerous git stay refused,
        // and the read-deny, git-checkout, and external-path rules apply
        // just as they would for a confined run.
        if !self.is_confinement_safe(command) {
            return Err(SofosError::ToolExecution(
                self.get_rejection_reason(command),
            ));
        }
        self.enforce_read_permissions(permission_manager, command)?;
        self.confirm_askable_command(command)?;
        self.check_bash_external_paths(command, permission_manager)?;

        // Already approved for this session: run unsandboxed without asking.
        let cached = self
            .session_unsandboxed
            .lock()
            .map(|set| set.contains(normalized))
            .unwrap_or(false);
        if cached {
            return self.run_and_shape(command, false, normalized);
        }

        let reason = escalation
            .justification
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let prompt = match reason {
            Some(reason) => format!(
                "The model wants to run this command outside the workspace sandbox.\n  \
                 {command}\n  Reason: {reason}\nAllow?"
            ),
            None => format!(
                "The model wants to run this command outside the workspace sandbox.\n  \
                 {command}\nAllow?"
            ),
        };
        let (approved, remember) = confirm_with_remember(&prompt)?;
        if !approved {
            return Err(SofosError::ToolExecution(format!(
                "User declined running '{}' outside the sandbox. Rework it to run inside \
                 the workspace, or ask the user to switch to /unrestricted.",
                command
            )));
        }
        if remember {
            if let Ok(mut set) = self.session_unsandboxed.lock() {
                set.insert(normalized.to_string());
            }
        }
        self.run_and_shape(command, false, normalized)
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
            let policy = self.confined_policy(command);
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
    ///
    /// A command that runs only git is allowed to write the project's
    /// `.git` directory, which it needs for operations like `checkout` and
    /// `config`; every other command keeps `.git` read-only so it cannot
    /// plant a Git hook there.
    fn confined_policy(&self, command: &str) -> SandboxPolicy {
        let mut policy = SandboxPolicy::for_workspace(&self.workspace);
        if command_runs_only_git(command) {
            let git_dir = self.workspace.join(sandbox::GIT_METADATA_DIR);
            policy
                .write_protect_subpaths
                .retain(|path| path != &git_dir);
        }
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
            self.confined_policy(command)
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
        // Normalise whitespace before matching so whitespace tricks,
        // spelling tricks, quoted values, global options, and launchers do
        // not skip the prompt. Case is kept so `-C` is not read as `-c`.
        let matches = command_contains_askable_git_checkout(&normalize_command_whitespace(command));
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

fn confined_command_failure_note() -> &'static str {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        "Workspace mode note: this command ran under the operating-system sandbox. Permission, network, socket, mount, or container engine errors can be caused by workspace mode: writes are limited to the workspace and temporary directories, project metadata folders are read-only, and network access, including local daemon sockets such as Docker, is closed. If no workspace-safe alternative can finish the task, rerun the command with sandbox_permissions set to \"require_escalated\" so the user can approve running it outside the sandbox, or tell the user they can switch to /unrestricted for this trusted operation."
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "Workspace mode note: this command ran under the operating-system sandbox. Permission, network, socket, mount, or container engine errors can be caused by workspace mode. If no workspace-safe alternative can finish the task, rerun the command with sandbox_permissions set to \"require_escalated\" so the user can approve running it outside the sandbox, or tell the user they can switch to /unrestricted for this trusted operation."
    }
}

/// Ask the user to approve an action with a three-way choice — Yes,
/// Yes and remember (for the rest of the session), or No (the default).
/// Returns `(approved, remember)`.
fn confirm_with_remember(prompt: &str) -> Result<(bool, bool)> {
    let idx = crate::tools::utils::confirm_multi_choice(
        prompt,
        &["Yes", "Yes and remember", "No"],
        2,
        crate::tools::utils::ConfirmationType::Permission,
    )?;
    Ok(match idx {
        0 => (true, false),
        1 => (true, true),
        _ => (false, false),
    })
}

/// Best-effort guess that a *confined* command failed because the
/// operating-system sandbox denied it — a blocked network connection or
/// socket, a write to a protected path, or a seccomp kill — rather than
/// failing on its own merits. There is no deterministic signal (a command
/// can print "permission denied" for unrelated reasons), so this matches
/// well-known denial wording and, on Linux, a seccomp SIGSYS. Port of
/// codex's `is_likely_sandbox_denied`.
fn is_likely_sandbox_denied(confined: bool, output: &SupervisedOutput) -> bool {
    if !confined || output.status.code() == Some(0) {
        return false;
    }

    const SANDBOX_DENIED_KEYWORDS: [&str; 7] = [
        "operation not permitted",
        "permission denied",
        "read-only file system",
        "seccomp",
        "sandbox",
        "landlock",
        "failed to write file",
    ];
    let has_keyword = [
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    ]
    .iter()
    .any(|section| {
        let lower = section.to_lowercase();
        SANDBOX_DENIED_KEYWORDS
            .iter()
            .any(|needle| lower.contains(needle))
    });
    if has_keyword {
        return true;
    }

    // Well-known non-sandbox shell exit codes: 2 (builtin misuse),
    // 126 (not executable), 127 (not found). Checked after the keyword
    // scan, so a denial that happens to surface one of these codes with
    // sandbox wording is still caught.
    if matches!(output.status.code(), Some(2) | Some(126) | Some(127)) {
        return false;
    }

    // A seccomp-blocked syscall kills the process with SIGSYS, which the
    // shell reports either as the 128+signal exit convention or as a
    // direct signal death. SIGSYS is sandbox-specific, so either form is
    // a denial. Linux only — seccomp is engaged only when confined there.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::ExitStatusExt;
        const EXIT_CODE_SIGNAL_BASE: i32 = 128;
        if output.status.code() == Some(EXIT_CODE_SIGNAL_BASE + libc::SIGSYS)
            || output.status.signal() == Some(libc::SIGSYS)
        {
            return true;
        }
    }

    false
}

/// Whether a confined-command failure should prompt the user to retry it
/// outside the sandbox. Kept pure so the gating is unit-testable without a
/// real sandbox: it fires only under a policy that wants on-failure
/// escalation, in an interactive session, when the failure looks
/// sandbox-caused.
fn should_prompt_unsandboxed(
    policy: ApprovalPolicy,
    interactive: bool,
    confined: bool,
    output: &SupervisedOutput,
) -> bool {
    policy.wants_no_sandbox_approval() && interactive && is_likely_sandbox_denied(confined, output)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn output_with(code: i32, stdout: &str, stderr: &str) -> SupervisedOutput {
        use std::os::unix::process::ExitStatusExt;
        SupervisedOutput {
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            status: ExitStatus::from_raw(code << 8),
            terminated_for: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_denial_needs_confinement_and_failure() {
        // Not confined, or a clean exit, is never a sandbox denial.
        assert!(!is_likely_sandbox_denied(
            false,
            &output_with(1, "", "operation not permitted")
        ));
        assert!(!is_likely_sandbox_denied(
            true,
            &output_with(0, "", "operation not permitted")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_denial_matches_known_keywords() {
        for needle in [
            "Operation not permitted",
            "permission denied",
            "read-only file system",
            "seccomp",
            "sandbox",
            "landlock",
            "failed to write file",
        ] {
            assert!(
                is_likely_sandbox_denied(true, &output_with(1, "", needle)),
                "stderr keyword `{needle}` should read as a sandbox denial"
            );
        }
        // A keyword on stdout counts too.
        assert!(is_likely_sandbox_denied(
            true,
            &output_with(1, "Operation not permitted", "")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_denial_ignores_plain_failures() {
        // An ordinary failure with no denial wording is not a denial.
        assert!(!is_likely_sandbox_denied(
            true,
            &output_with(1, "", "error: missing semicolon")
        ));
        // The well-known shell codes are rejected without a keyword.
        for code in [2, 126, 127] {
            assert!(!is_likely_sandbox_denied(
                true,
                &output_with(code, "", "boom")
            ));
        }
        // But a denial keyword overrides the quick-reject codes.
        assert!(is_likely_sandbox_denied(
            true,
            &output_with(126, "", "permission denied")
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sandbox_denial_catches_seccomp_sigsys() {
        use std::os::unix::process::ExitStatusExt;
        // The 128+signal exit convention.
        let by_code = SupervisedOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            status: ExitStatus::from_raw((128 + libc::SIGSYS) << 8),
            terminated_for: None,
        };
        assert!(is_likely_sandbox_denied(true, &by_code));
        // A direct SIGSYS signal death.
        let by_signal = SupervisedOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            status: ExitStatus::from_raw(libc::SIGSYS),
            terminated_for: None,
        };
        assert!(is_likely_sandbox_denied(true, &by_signal));
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    #[test]
    fn non_linux_ignores_sigsys_exit_code_without_keyword() {
        // Off Linux there is no seccomp branch, so a 128+SIGSYS exit code
        // with no denial wording is treated as an ordinary failure.
        assert!(!is_likely_sandbox_denied(
            true,
            &output_with(128 + libc::SIGSYS, "", "boom")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unsandboxed_prompt_gated_by_policy_and_interactivity() {
        let denial = output_with(1, "", "operation not permitted");
        // Reactive policies prompt when interactive and the failure looks
        // sandbox-caused.
        assert!(should_prompt_unsandboxed(
            ApprovalPolicy::OnFailure,
            true,
            true,
            &denial
        ));
        // OnRequest/Never never take the reactive path.
        assert!(!should_prompt_unsandboxed(
            ApprovalPolicy::OnRequest,
            true,
            true,
            &denial
        ));
        assert!(!should_prompt_unsandboxed(
            ApprovalPolicy::Never,
            true,
            true,
            &denial
        ));
        // Non-interactive never prompts.
        assert!(!should_prompt_unsandboxed(
            ApprovalPolicy::OnFailure,
            false,
            true,
            &denial
        ));
        // A non-denial failure never prompts.
        assert!(!should_prompt_unsandboxed(
            ApprovalPolicy::OnFailure,
            true,
            true,
            &output_with(1, "", "error")
        ));
    }

    #[test]
    fn confined_command_failure_note_mentions_unrestricted_mode() {
        let note = confined_command_failure_note();
        assert!(note.contains("Workspace mode note"));
        assert!(note.contains("/unrestricted"));
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        assert!(note.contains("Docker"));
    }
}
