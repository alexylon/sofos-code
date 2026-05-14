//! Process spawn and result-shaping for the bash executor. The
//! permission gate ([`BashExecutor::execute`]) decides whether to run
//! the command at all; [`BashExecutor::execute_after_permission_check`]
//! is the path that actually spawns `sh -c <command>`, applies the
//! per-stream output caps from [`super::output`], and renders the
//! result string the model sees.

use crate::error::{Result, SofosError};
use crate::tools::bash::BashExecutor;
use crate::tools::bash::output::MAX_BASH_OUTPUT_BYTES;
use crate::tools::bash::validate::command_contains_op;
use crate::tools::permissions::{CommandPermission, PermissionManager};
use crate::tools::utils::{MAX_TOOL_OUTPUT_TOKENS, TruncationKind, truncate_for_context};
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

impl BashExecutor {
    pub fn new(workspace: PathBuf, interactive: bool, has_morph: bool) -> Result<Self> {
        Ok(Self {
            workspace,
            interactive,
            has_morph,
            session_allowed: Arc::new(Mutex::new(HashSet::new())),
            session_denied: Arc::new(Mutex::new(HashSet::new())),
            bash_path_session_allowed: Arc::new(Mutex::new(HashSet::new())),
            bash_path_session_denied: Arc::new(Mutex::new(HashSet::new())),
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

        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.workspace)
            .output()
            .map_err(|e| SofosError::ToolExecution(format!("Failed to execute command: {}", e)))?;

        if output.stdout.len() > MAX_BASH_OUTPUT_BYTES {
            return Err(SofosError::ToolExecution(format!(
                "Command output too large ({} bytes). Maximum size is {} MB",
                output.stdout.len(),
                MAX_BASH_OUTPUT_BYTES / (1024 * 1024)
            )));
        }

        if output.stderr.len() > MAX_BASH_OUTPUT_BYTES {
            return Err(SofosError::ToolExecution(format!(
                "Command error output too large ({} bytes). Maximum size is {} MB",
                output.stderr.len(),
                MAX_BASH_OUTPUT_BYTES / (1024 * 1024)
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            let exit_info = match output.status.code() {
                Some(code) => format!("exit code: {}", code),
                None => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        match output.status.signal() {
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
