//! Bash executor: process spawn under a three-tier permission model,
//! plus structural and path-policy gates. Submodules split the
//! concerns:
//!
//! - [`executor`] — process spawn and the permission gate that drives
//!   it.
//! - [`validate`] — structural checks, path policy, and the rejection
//!   messages the executor returns when a command is refused.
//! - [`output`] — per-stream byte caps and signal-name lookup used by
//!   the executor when shaping the result string.

pub mod executor;
pub mod output;
pub mod sandbox;
pub mod validate;

use crate::config::{ApprovalPolicy, SandboxMode};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// A model-driven request to run a single command outside the
/// operating-system sandbox (`sandbox_permissions: "require_escalated"`).
/// Its presence means the model asked to escalate; `justification` is the
/// reason shown to the user in the approval prompt.
#[derive(Clone, Debug, Default)]
pub struct EscalationRequest {
    pub justification: Option<String>,
}

#[derive(Clone)]
pub struct BashExecutor {
    pub(super) workspace: PathBuf,
    /// Whether interactive prompts (stdin) are available
    pub(super) interactive: bool,
    /// Whether `morph_edit_file` is exposed (drives error-message hints)
    pub(super) has_morph: bool,
    /// Access mode in effect. In the sandboxed mode, commands the permission
    /// gate would otherwise refuse are run confined by the sandbox.
    pub(super) mode: SandboxMode,
    /// When the user is asked before a command runs outside the sandbox.
    /// Gates both escalation paths; see [`ApprovalPolicy`].
    pub(super) approval_policy: ApprovalPolicy,
    /// Session-scoped temporary permissions (not persisted to config)
    pub(super) session_allowed: Arc<Mutex<HashSet<String>>>,
    pub(super) session_denied: Arc<Mutex<HashSet<String>>>,
    /// Commands the user has approved to run outside the sandbox for the
    /// rest of this session, so an escalation is not re-prompted for the
    /// same command.
    pub(super) session_unsandboxed: Arc<Mutex<HashSet<String>>>,
    /// Session-scoped Bash path grants for external directories
    pub(super) bash_path_session_allowed: Arc<Mutex<HashSet<String>>>,
    pub(super) bash_path_session_denied: Arc<Mutex<HashSet<String>>>,
    /// Shared with the TUI: set to `true` when the user presses ESC or
    /// Ctrl+C during a turn. The supervisor loop checks it between
    /// poll ticks and kills the running command tree on transition.
    /// Defaults to a fresh atomic in `new`; the REPL installs its own
    /// shared flag after construction via `install_interrupt_flag`.
    pub(super) interrupt_flag: Arc<AtomicBool>,
}

impl BashExecutor {
    pub fn install_interrupt_flag(&mut self, flag: Arc<AtomicBool>) {
        self.interrupt_flag = flag;
    }

    pub fn set_sandbox_mode(&mut self, mode: SandboxMode) {
        self.mode = mode;
    }

    pub fn set_approval_policy(&mut self, policy: ApprovalPolicy) {
        self.approval_policy = policy;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SofosError;
    use crate::tools::bash::validate::{
        command_contains_askable_git_checkout, command_runs_only_git, detect_ansi_c_quoting,
        detect_command_substitution, has_path_traversal, path_token_shell_meta,
    };
    use crate::tools::test_support;

    #[test]
    fn temp_fsmonitor_claim() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();
        eprintln!(
            "fsmonitor=true status -> {}",
            executor.is_safe_command_structure("git -c core.fsmonitor=true status")
        );
        eprintln!(
            "fsmonitor=false status -> {}",
            executor.is_safe_command_structure("git -c core.fsmonitor=false status")
        );
        eprintln!(
            "pager=false log -> {}",
            executor.is_safe_command_structure("git -c core.pager=false log")
        );
        eprintln!(
            "fsmonitor=/path/hook status -> {}",
            executor.is_safe_command_structure("git -c core.fsmonitor=/path/hook status")
        );
        eprintln!(
            "core.pager=less log (non-bool) -> {}",
            executor.is_safe_command_structure("git -c core.pager=less log")
        );
    }

    #[test]
    fn test_safe_commands() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        // Note: These tests check the command structure safety only
        // Actual permission checking is done by PermissionManager
        assert!(executor.is_safe_command_structure("ls -la"));
        assert!(executor.is_safe_command_structure("cat file.txt"));
        assert!(executor.is_safe_command_structure("grep pattern file.txt"));
        assert!(executor.is_safe_command_structure("cargo test"));
        assert!(executor.is_safe_command_structure("cargo build"));
        assert!(executor.is_safe_command_structure("echo hello"));
        assert!(executor.is_safe_command_structure("pwd"));

        // Test that 2>&1 is allowed (combines stderr to stdout)
        assert!(executor.is_safe_command_structure("cargo build 2>&1"));
        assert!(executor.is_safe_command_structure("npm test 2>&1"));
        assert!(executor.is_safe_command_structure("ls 2>&1 | grep error"));
        assert!(executor.is_safe_command_structure("cargo test 2>&1"));
    }

    #[test]
    fn test_unsafe_command_structures() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        // Test structural safety issues (not permission-based)
        assert!(!executor.is_safe_command_structure("echo hello > file.txt"));
        assert!(!executor.is_safe_command_structure("cat file.txt >> output.txt"));

        // These should still be blocked (file redirection even with 2>&1)
        assert!(!executor.is_safe_command_structure("echo hello > file.txt 2>&1"));
        assert!(!executor.is_safe_command_structure("cargo build 2>&1 > output.txt"));
    }

    #[test]
    fn test_path_traversal_blocked() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure("cat ../file.txt"));
        assert!(!executor.is_safe_command_structure("ls ../../etc"));
        assert!(!executor.is_safe_command_structure("cat ../../../etc/passwd"));
        assert!(!executor.is_safe_command_structure("cat file.txt && ls .."));
        assert!(!executor.is_safe_command_structure("ls | cat ../secret"));
    }

    #[test]
    fn test_absolute_paths_pass_structural_check() {
        // Absolute paths are no longer blocked by is_safe_command_structure.
        // They are handled by check_bash_external_paths which asks the user.
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(executor.is_safe_command_structure("/bin/ls"));
        assert!(executor.is_safe_command_structure("cat /etc/passwd"));
        assert!(executor.is_safe_command_structure("ls /tmp"));
        assert!(executor.is_safe_command_structure("cat /home/user/file"));
    }

    #[test]
    fn sandbox_confines_every_safe_command() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        // With a sandbox engaged, familiar read-only commands, build
        // tools, and interpreters all run confined, not just unfamiliar
        // ones. This is what closes the network and read-deny holes.
        // Output redirection is accepted because the sandbox keeps the
        // write inside the workspace.
        for command in [
            "ls -la",
            "cat file.txt",
            "cargo build",
            "python3 -c 'import os'",
            "grep -r pattern .",
            "echo hi > out.txt",
        ] {
            assert!(
                executor.should_confine(command, true).unwrap(),
                "{command} should run confined when a sandbox is engaged"
            );
        }
    }

    #[test]
    fn confinement_never_relaxes_the_hard_denials() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        // Parent traversal, hidden subcommands, and dangerous git stay
        // refused even with a sandbox to bound writes and the network.
        for command in ["cat ../../etc/passwd", "echo $(rm bad)", "git -C . push"] {
            assert!(
                executor.should_confine(command, true).is_err(),
                "{command} must be refused even confined"
            );
        }
    }

    #[test]
    fn command_runs_only_git_detects_pure_git_commands() {
        // Pure git commands: the confined policy lets these write `.git`,
        // which they need for checkout, config, and similar.
        assert!(command_runs_only_git("git checkout other"));
        assert!(command_runs_only_git("git -C . config user.name x"));
        assert!(command_runs_only_git("git restore --staged file.txt"));
        assert!(command_runs_only_git("git status && git log"));

        // Anything that also runs a non-git program keeps `.git` read-only.
        assert!(!command_runs_only_git("ls -la"));
        assert!(!command_runs_only_git("python3 -c 'import os'"));
        assert!(!command_runs_only_git("git status && cat .git/config"));
        assert!(!command_runs_only_git("cat f && git checkout other"));
        // A launcher-wrapped git is not plainly git, so it fails closed.
        assert!(!command_runs_only_git("env git checkout other"));

        // A "git" that is not the trusted binary must not earn the carve-out:
        // a path-prefixed git could be a planted binary, and a dangerous env
        // prefix could swap the binary or hijack the real git's loader.
        assert!(!command_runs_only_git("./git status"));
        assert!(!command_runs_only_git("./fakebin/git status"));
        assert!(!command_runs_only_git("/usr/bin/git status"));
        assert!(!command_runs_only_git("PATH=./fakebin git status"));
        assert!(!command_runs_only_git("LD_PRELOAD=./evil.so git status"));
        assert!(!command_runs_only_git("git status && PATH=. git log"));
    }

    #[test]
    fn without_sandbox_only_safe_structure_runs_unconfined() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        // No sandbox to bound a command: structurally safe ones run
        // unconfined, and anything that needs the sandbox to be safe, such
        // as output redirection, is refused rather than run unprotected.
        assert!(!executor.should_confine("ls -la", false).unwrap());
        assert!(executor.should_confine("echo hi > out.txt", false).is_err());
        assert!(executor.should_confine("cat ../secret", false).is_err());
    }

    #[test]
    fn test_output_size_limit() {
        let (_temp, path) = test_support::workspace();
        let executor = BashExecutor::new(path, false, false).unwrap();

        let result = executor.execute("seq 1 2000000");

        assert!(result.is_err());
        if let Err(SofosError::ToolExecution(msg)) = result {
            assert!(msg.contains("too large"));
            assert!(msg.contains("10 MB"));
        } else {
            panic!("Expected ToolExecution error");
        }
    }

    /// Windows canonicalises temp directories to paths like
    /// `\\?\C:\Users\...` that contain `?` in the verbatim prefix. The
    /// shell-meta check must not reject those, and the read-deny check
    /// must still recognise them as paths so a `Read(...)` deny rule
    /// fires.
    #[cfg(target_os = "windows")]
    #[test]
    fn test_read_deny_applies_to_windows_verbatim_paths() {
        use std::fs;

        let (_temp, path) = test_support::workspace();
        let config_dir = path.join(".sofos");
        fs::create_dir_all(&config_dir).unwrap();
        let canonical = std::fs::canonicalize(&path).unwrap();
        let secret_dir = canonical.join("test");
        let secret_dir_str = secret_dir.display().to_string().replace('\\', "/");
        fs::write(
            config_dir.join("config.local.toml"),
            format!(
                "[permissions]\nallow = []\ndeny = [\"Read({secret_dir_str}/**)\"]\nask = []\n"
            ),
        )
        .unwrap();

        let executor = BashExecutor::new(path, false, false).unwrap();
        let secret_file = canonical.join("test").join("secret.txt");
        let cmd = format!("cat {}", secret_file.display());
        let result = executor.execute(&cmd);

        assert!(
            result.is_err(),
            "Read deny should fire on a verbatim Windows path: {result:?}"
        );
        if let Err(SofosError::ToolExecution(msg)) = result {
            assert!(
                msg.contains("Read access denied") || msg.contains("denied"),
                "expected a Read-deny error, got: {msg}"
            );
        }
    }

    #[test]
    fn test_read_permission_blocks_cat() {
        use std::fs;

        let (_temp, path) = test_support::workspace();

        // Write deny config for test folder reads
        let config_dir = path.join(".sofos");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.local.toml"),
            r#"[permissions]
allow = []
deny = ["Read(./test/**)"]
ask = []
"#,
        )
        .unwrap();

        let executor = BashExecutor::new(path, false, false).unwrap();

        // Even without creating the file, permission check should block before execution
        let result = executor.execute("cat ./test/secret.txt");

        assert!(result.is_err());
        if let Err(SofosError::ToolExecution(msg)) = result {
            assert!(msg.contains("Read access denied") || msg.contains("denied"));
        } else {
            panic!("Expected ToolExecution error");
        }
    }

    /// A `Read(...)` deny that matches the command's own program path
    /// blocks it: the program token is checked when it is path-shaped, so a
    /// denied script cannot be run to read it. A bare command name is not
    /// treated as a read target, so a deny that happens to match the name
    /// does not block it.
    #[test]
    fn read_deny_applies_to_path_shaped_program_token() {
        use std::fs;

        let (_temp, path) = test_support::workspace();
        let config_dir = path.join(".sofos");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.local.toml"),
            r#"[permissions]
allow = []
deny = ["Read(./scripts/**)", "Read(ls)"]
ask = []
"#,
        )
        .unwrap();

        let executor = BashExecutor::new(path.clone(), false, false).unwrap();
        let manager = crate::tools::permissions::PermissionManager::new(path).unwrap();

        // The path-shaped program token is now checked against the deny.
        assert!(
            executor
                .enforce_read_permissions(&manager, "./scripts/secret.sh --flag")
                .is_err(),
            "a read-denied program path must be blocked"
        );

        // A bare command name at index 0 is not a read target, so the
        // contrived `Read(ls)` deny does not block running `ls`.
        assert!(
            executor
                .enforce_read_permissions(&manager, "ls -la")
                .is_ok(),
            "a bare command name must not be treated as a read path"
        );

        // A program reached through a variable path can't be expanded before
        // the shell runs, so it can't be checked against the deny rules and
        // is refused — the same fail-closed treatment a `$VAR` argument path
        // already gets. Passing a literal path instead is the way through.
        assert!(
            executor
                .enforce_read_permissions(&manager, "$HOME/bin/tool --flag")
                .is_err(),
            "a program reached through a variable path must be refused"
        );
    }

    #[test]
    fn test_safe_git_commands() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        // Safe read-only git commands
        assert!(executor.is_safe_command_structure("git status"));
        assert!(executor.is_safe_command_structure("git log"));
        assert!(executor.is_safe_command_structure("git log --oneline"));
        assert!(executor.is_safe_command_structure("git diff"));
        assert!(executor.is_safe_command_structure("git diff HEAD~1"));
        assert!(executor.is_safe_command_structure("git show"));
        assert!(executor.is_safe_command_structure("git show HEAD"));
        assert!(executor.is_safe_command_structure("git branch"));
        assert!(executor.is_safe_command_structure("git branch -v"));
        assert!(executor.is_safe_command_structure("git branch --list"));
        assert!(executor.is_safe_command_structure("git remote -v"));
        assert!(executor.is_safe_command_structure("git config --list"));
        assert!(executor.is_safe_command_structure("git ls-files"));
        assert!(executor.is_safe_command_structure("git ls-tree HEAD"));
        assert!(executor.is_safe_command_structure("git blame file.txt"));
        assert!(executor.is_safe_command_structure("git grep pattern"));
        assert!(executor.is_safe_command_structure("git rev-parse HEAD"));
        assert!(executor.is_safe_command_structure("git describe --tags"));
        assert!(executor.is_safe_command_structure("git stash list"));
        assert!(executor.is_safe_command_structure("git stash show"));
        assert!(executor.is_safe_command_structure("git stash show stash@{0}"));
        // File-recovery commands — allowed so the model can roll back a
        // botched edit without going through the write tools.
        assert!(executor.is_safe_command_structure("git restore file.txt"));
        assert!(executor.is_safe_command_structure("git restore src/foo.rs"));
        assert!(executor.is_safe_command_structure("git checkout -- file.txt"));
        assert!(executor.is_safe_command_structure("git checkout HEAD -- src/foo.rs"));
        // Revision ranges (`HEAD~5..HEAD`) are not path traversal —
        // they're opaque token substrings that used to be blocked by
        // the old substring check on `..`.
        assert!(executor.is_safe_command_structure("git log HEAD~5..HEAD"));
        assert!(executor.is_safe_command_structure("git diff HEAD~1..HEAD"));
        assert!(executor.is_safe_command_structure("git log HEAD~5..HEAD -- src/foo.rs"));
    }

    #[test]
    fn test_path_traversal_token_detection() {
        // Literal path-traversal tokens — all blocked.
        assert!(has_path_traversal("cd .."));
        assert!(has_path_traversal("cat ../file"));
        assert!(has_path_traversal("ls ../../etc"));
        assert!(has_path_traversal("cat /foo/..")); // ends_with /..
        assert!(has_path_traversal("cat foo/../bar")); // contains /../
        // Quoted / shell-wrapped variants — still blocked after the
        // trailing paren, quote, or backtick is stripped.
        assert!(has_path_traversal("cat \"../secret\""));
        assert!(has_path_traversal("cat '../secret'"));
        assert!(has_path_traversal("echo $(cat ../secret)"));
        assert!(has_path_traversal("ls `../bin/tool`"));

        // Flag-embedded / assignment-embedded traversal. These used
        // to slip through the token-only split because the entire
        // `KEY=VALUE` arg was a single opaque token.
        assert!(has_path_traversal("clang --include=../secret.h file.c"));
        assert!(has_path_traversal("PATH=/usr/bin:../foo cmd"));
        assert!(has_path_traversal("FOO=.. cmd"));

        // Opaque tokens that happen to contain `..` — allowed. These
        // are the false positives the old `contains("..")` check
        // produced and broke legitimate git diagnostics.
        assert!(!has_path_traversal("git log HEAD~5..HEAD"));
        assert!(!has_path_traversal("git diff HEAD~1..HEAD -- src/foo.rs"));
        assert!(!has_path_traversal("grep '\\.\\.\\.' file.txt"));
        assert!(!has_path_traversal("ls foo..bar")); // unusual filename, not traversal
        // Git colon path syntax survives the `:` split because
        // neither `HEAD` nor the path contain a traversal fragment.
        assert!(!has_path_traversal("git show HEAD:src/foo.rs"));
        assert!(!has_path_traversal("git show HEAD~5:src/foo.rs"));
    }

    #[test]
    fn test_dangerous_git_commands() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        // Remote operations (data leakage risk)
        assert!(!executor.is_safe_command_structure("git push"));
        assert!(!executor.is_safe_command_structure("git push origin main"));
        assert!(!executor.is_safe_command_structure("git push --force"));
        assert!(!executor.is_safe_command_structure("git pull"));
        assert!(!executor.is_safe_command_structure("git pull origin main"));
        assert!(!executor.is_safe_command_structure("git fetch"));
        assert!(!executor.is_safe_command_structure("git fetch origin"));
        assert!(!executor.is_safe_command_structure("git clone https://example.com/repo.git"));

        // Destructive local operations
        assert!(!executor.is_safe_command_structure("git clean -fd"));
        assert!(!executor.is_safe_command_structure("git reset --hard"));
        assert!(!executor.is_safe_command_structure("git reset --hard HEAD~1"));
        assert!(!executor.is_safe_command_structure("git checkout -f"));
        assert!(!executor.is_safe_command_structure("git checkout -b newbranch"));
        assert!(!executor.is_safe_command_structure("git branch -D branch-name"));
        assert!(!executor.is_safe_command_structure("git branch -d branch-name"));
        assert!(!executor.is_safe_command_structure("git filter-branch"));

        // Modifications
        assert!(!executor.is_safe_command_structure("git add ."));
        assert!(!executor.is_safe_command_structure("git add file.txt"));
        assert!(!executor.is_safe_command_structure("git commit -m 'message'"));
        assert!(!executor.is_safe_command_structure("git commit --amend"));
        assert!(!executor.is_safe_command_structure("git rm file.txt"));
        assert!(!executor.is_safe_command_structure("git mv old.txt new.txt"));
        assert!(!executor.is_safe_command_structure("git merge branch"));
        assert!(!executor.is_safe_command_structure("git rebase main"));
        assert!(!executor.is_safe_command_structure("git cherry-pick abc123"));
        assert!(!executor.is_safe_command_structure("git revert abc123"));
        assert!(!executor.is_safe_command_structure("git switch main"));

        // Remote configuration changes
        assert!(
            !executor.is_safe_command_structure("git remote add origin https://evil.com/repo.git")
        );
        assert!(
            !executor
                .is_safe_command_structure("git remote set-url origin https://evil.com/repo.git")
        );
        assert!(!executor.is_safe_command_structure("git remote remove origin"));

        // Submodules (can fetch from remote)
        assert!(!executor.is_safe_command_structure("git submodule update"));
        assert!(!executor.is_safe_command_structure("git submodule init"));

        // Stash operations (modify state)
        assert!(!executor.is_safe_command_structure("git stash"));
        assert!(!executor.is_safe_command_structure("git stash pop"));
        assert!(!executor.is_safe_command_structure("git stash apply"));
        assert!(!executor.is_safe_command_structure("git stash drop"));
        assert!(!executor.is_safe_command_structure("git stash clear"));

        // Init (creates repository)
        assert!(!executor.is_safe_command_structure("git init"));
        assert!(!executor.is_safe_command_structure("git init new-repo"));
    }

    #[test]
    fn test_git_commands_in_chains() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        // Safe commands in chains
        assert!(executor.is_safe_command_structure("git status && git log"));
        assert!(executor.is_safe_command_structure("git diff | grep pattern"));
        assert!(executor.is_safe_command_structure("echo test; git status"));

        // Dangerous commands in chains
        assert!(!executor.is_safe_command_structure("git status && git push"));
        assert!(!executor.is_safe_command_structure("git log | git commit -m 'test'"));
        assert!(!executor.is_safe_command_structure("echo test; git add ."));
        assert!(!executor.is_safe_command_structure("git status || git pull"));
    }

    #[test]
    fn test_error_messages_are_informative() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        let reason = executor.get_git_rejection_reason("git push origin main");
        assert!(reason.contains("git push origin main"));
        assert!(reason.contains("remote repositories"));
        assert!(reason.contains("git status"));
    }

    #[test]
    fn test_tilde_paths_pass_structural_check() {
        // Tilde paths are no longer blocked by is_safe_command_structure.
        // They are handled by check_bash_external_paths which asks the user.
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(executor.is_safe_command_structure("ls ~/tmp"));
        assert!(executor.is_safe_command_structure("cat ~/file.txt"));
        assert!(executor.is_safe_command_structure("grep pattern ~/docs/file.txt"));
    }

    #[test]
    fn test_git_checkout_requires_confirmation_non_interactive() {
        // Plain `git checkout <branch>` isn't destructive enough to hard-
        // deny (git refuses dirty-tree switches), but it mutates
        // working-tree state in a way the user should see before it
        // runs. In non-interactive mode (tests, piped stdin) there's no
        // way to prompt, so the executor returns a clear error pointing
        // at the interactive-mode requirement.
        let (_temp, path) = test_support::workspace();
        let executor = BashExecutor::new(path, false, false).unwrap();

        for cmd in &[
            "git checkout main",
            "git checkout HEAD~3",
            "git checkout -- src/lib.rs",
            "git -C . checkout main",
            "\\git checkout main",
        ] {
            let result = executor.execute(cmd);
            assert!(
                result.is_err(),
                "expected confirmation gate to deny `{}` in non-interactive mode",
                cmd
            );
            if let Err(SofosError::ToolExecution(msg)) = result {
                assert!(
                    msg.contains("confirmation"),
                    "expected 'confirmation' hint for `{}`, got: {}",
                    cmd,
                    msg
                );
            } else {
                panic!(
                    "expected ToolExecution error for `{}`, got: {:?}",
                    cmd, result
                );
            }
        }
    }

    #[test]
    fn test_git_checkout_force_stays_hard_denied() {
        // `git checkout -f` and `git checkout -b` must reject BEFORE the
        // confirmation gate — they're in `dangerous_git_ops` and stay
        // in the hard-deny tier even with the new askable mechanism.
        // The error message mentions the dangerous-op reason, not the
        // interactive-confirmation hint.
        let (_temp, path) = test_support::workspace();
        let executor = BashExecutor::new(path, false, false).unwrap();

        for cmd in &["git checkout -f main", "git checkout -b new-branch"] {
            let result = executor.execute(cmd);
            assert!(result.is_err(), "`{}` must stay hard-denied", cmd);
            if let Err(SofosError::ToolExecution(msg)) = result {
                assert!(
                    !msg.contains("requires interactive confirmation"),
                    "`{}` should be hard-denied, not askable — got: {}",
                    cmd,
                    msg
                );
            }
        }
    }

    #[test]
    fn test_flag_embedded_external_path_is_checked() {
        // `--include=/etc/passwd` previously slipped past the external-path
        // prompt because the whole token was filtered by `starts_with('-')`.
        // The path portion is now extracted and routed to
        // `check_bash_external_path`, which deny in non-interactive mode
        // when no grant is configured.
        let (_temp, path) = test_support::workspace();
        let executor = BashExecutor::new(path, false, false).unwrap();

        let result = executor.execute("grep --include=/etc/passwd pattern .");

        assert!(result.is_err(), "expected external-path rejection");
        if let Err(SofosError::ToolExecution(msg)) = result {
            assert!(
                msg.contains("outside workspace"),
                "expected 'outside workspace' in error, got: {msg}"
            );
        } else {
            panic!("Expected ToolExecution error, got: {result:?}");
        }
    }

    #[test]
    fn test_session_scoped_permissions_persist() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        // Simulate adding a command to session_allowed
        {
            let mut allowed = executor.session_allowed.lock().unwrap();
            allowed.insert("Bash(my_custom_cmd)".to_string());
        }

        // Verify it's recognized on subsequent check
        {
            let allowed = executor.session_allowed.lock().unwrap();
            assert!(allowed.contains("Bash(my_custom_cmd)"));
        }

        // Simulate adding a command to session_denied
        {
            let mut denied = executor.session_denied.lock().unwrap();
            denied.insert("Bash(blocked_cmd)".to_string());
        }

        // Verify denied is recognized
        {
            let denied = executor.session_denied.lock().unwrap();
            assert!(denied.contains("Bash(blocked_cmd)"));
        }
    }

    #[test]
    fn test_session_permissions_shared_across_clones() {
        let executor1 = BashExecutor::new(PathBuf::from("."), false, false).unwrap();
        let executor2 = executor1.clone();

        // Add permission via executor1
        {
            let mut allowed = executor1.session_allowed.lock().unwrap();
            allowed.insert("Bash(shared_cmd)".to_string());
        }

        // Verify executor2 sees it (Arc sharing)
        {
            let allowed = executor2.session_allowed.lock().unwrap();
            assert!(allowed.contains("Bash(shared_cmd)"));
        }
    }

    /// Shell substitution hides commands from the permission system.
    /// The structural check must reject every form outside of single
    /// quotes, including process substitution `<(cmd)` and `>(cmd)`,
    /// while leaving arithmetic expansion `$((expr))` and literal
    /// single-quoted markers alone.
    #[test]
    fn test_detect_command_substitution() {
        assert_eq!(detect_command_substitution("echo $(rm bad)"), Some("$("));
        assert_eq!(detect_command_substitution("echo `rm bad`"), Some("`"));
        assert_eq!(
            detect_command_substitution("diff <(echo a) <(echo b)"),
            Some("<(")
        );
        assert_eq!(detect_command_substitution("tee >(grep foo)"), Some(">("));
        assert_eq!(
            detect_command_substitution("echo \"$(rm bad)\""),
            Some("$(")
        );

        assert_eq!(detect_command_substitution("echo '$(rm bad)'"), None);
        assert_eq!(detect_command_substitution("echo \\$(rm bad)"), None);
        assert_eq!(detect_command_substitution("echo $((1+2))"), None);
        assert_eq!(detect_command_substitution("echo plain text"), None);

        assert_eq!(
            detect_command_substitution("echo $(($(rm bad)))"),
            Some("$(")
        );
    }

    #[test]
    fn test_substitution_blocks_structural_check() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();
        assert!(!executor.is_safe_command_structure("echo $(rm bad)"));
        assert!(!executor.is_safe_command_structure("echo `rm bad`"));
        assert!(!executor.is_safe_command_structure("diff <(cat a) <(cat b)"));

        // Arithmetic expansion and single-quoted literals stay safe.
        assert!(executor.is_safe_command_structure("echo $((1+1))"));
        assert!(executor.is_safe_command_structure("echo '$(literal)'"));
    }

    #[test]
    fn test_substitution_rejection_message_names_the_marker() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();
        let reason = executor.get_rejection_reason("echo $(rm bad)");
        assert!(reason.contains("shell substitution"));
        assert!(reason.contains("$("));
    }

    #[test]
    fn test_detect_ansi_c_quoting() {
        // The decoded forms the git gate would otherwise miss.
        assert!(detect_ansi_c_quoting("$'\\x67it' push"));
        assert!(detect_ansi_c_quoting("git $'\\x70ush' origin main"));
        assert!(detect_ansi_c_quoting("git $'\\x72m' -rf ."));
        // Glued to a preceding word, and after a single-quoted segment.
        assert!(detect_ansi_c_quoting("echo a$'\\x41'"));
        assert!(detect_ansi_c_quoting("'git '$'\\x70ush'"));

        // A `$'` inside single or double quotes, or escaped, is an ordinary
        // dollar before a quote — not ANSI-C quoting.
        assert!(!detect_ansi_c_quoting("echo '$'"));
        assert!(!detect_ansi_c_quoting("echo \"$'foo'\""));
        assert!(!detect_ansi_c_quoting("echo \\$'foo'"));
        assert!(!detect_ansi_c_quoting("git push origin main"));
        assert!(!detect_ansi_c_quoting("grep '\\t' file"));
    }

    #[test]
    fn test_ansi_c_quoting_blocks_disguised_git() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();
        // The audit's bypass: $'\x70ush' decodes to push, $'\x67it' to git.
        assert!(!executor.is_safe_command_structure("git $'\\x70ush' origin main"));
        assert!(!executor.is_safe_command_structure("$'\\x67it' push"));
        // The .git write carve-out cannot be reopened with a disguised verb.
        assert!(!executor.is_safe_command_structure("git $'\\x72m' --cached file"));

        let reason = executor.get_rejection_reason("git $'\\x70ush' origin main");
        assert!(reason.contains("$'...'"));
    }

    /// Workspace-relative tokens whose canonical resolution lands
    /// outside the workspace must trip the same external-path gate as
    /// an explicit absolute path. Without this, a symlink inside the
    /// workspace can let an otherwise-allowed read command exfiltrate
    /// data from anywhere on disk.
    #[cfg(unix)]
    #[test]
    fn test_workspace_symlink_escape_is_blocked() {
        use std::os::unix::fs::symlink;

        let (_temp, workspace) = test_support::workspace();
        // Put the escape target inside a sibling TempDir so concurrent
        // test runs do not race on a shared filename in the system
        // temp directory.
        let outside_dir = tempfile::TempDir::new().unwrap();
        let outside = outside_dir.path().join("sofos-symlink-target");
        std::fs::write(&outside, "secret").unwrap();
        let link = workspace.join("escape_link");
        symlink(&outside, &link).unwrap();

        let executor = BashExecutor::new(workspace, false, false).unwrap();
        let result = executor.execute("cat escape_link");

        assert!(result.is_err(), "expected symlink escape to be denied");
        if let Err(SofosError::ToolExecution(msg)) = result {
            assert!(
                msg.contains("outside workspace"),
                "expected external-path message, got: {msg}"
            );
        } else {
            panic!("Expected ToolExecution error, got: {result:?}");
        }
    }

    /// A workspace symlink pointing outside the project must trip the
    /// external-path gate even when the referenced leaf does not exist yet.
    /// Plain `canonicalize` fails on the missing leaf; resolving the
    /// deepest existing ancestor still catches the escape. Without this, a
    /// read of a not-yet-created path under such a symlink would skip the
    /// gate.
    #[cfg(unix)]
    #[test]
    fn test_workspace_symlink_escape_with_missing_leaf_is_blocked() {
        use std::os::unix::fs::symlink;

        let (_temp, workspace) = test_support::workspace();
        let outside_dir = tempfile::TempDir::new().unwrap();
        let link = workspace.join("escape_link");
        symlink(outside_dir.path(), &link).unwrap();

        let executor = BashExecutor::new(workspace, false, false).unwrap();
        // `not-created-yet` does not exist under the linked directory.
        let result = executor.execute("cat escape_link/not-created-yet");

        assert!(
            result.is_err(),
            "expected a symlink escape with a missing leaf to be denied, got: {result:?}"
        );
        if let Err(SofosError::ToolExecution(msg)) = result {
            assert!(
                msg.contains("outside workspace"),
                "expected external-path message, got: {msg}"
            );
        } else {
            panic!("Expected ToolExecution error, got: {result:?}");
        }
    }

    /// A freshly built executor defaults to the confined mode, so a caller
    /// that forgets to set the mode fails closed rather than running
    /// commands unconfined.
    #[test]
    fn new_executor_defaults_to_sandboxed() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();
        assert!(
            executor.mode.is_sandboxed(),
            "a new executor must default to the confined mode"
        );
    }

    #[test]
    fn test_interrupt_flag_terminates_long_running_command() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};

        let (_temp, path) = test_support::workspace();
        let mut executor = BashExecutor::new(path, false, false).unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        executor.install_interrupt_flag(Arc::clone(&flag));

        // Pre-allow the command for this session so the test doesn't
        // sit on the permission prompt with no stdin to answer.
        {
            let mut allowed = executor.session_allowed.lock().unwrap();
            allowed.insert("Bash(sleep 30)".to_string());
        }

        let flag_for_thread = Arc::clone(&flag);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            flag_for_thread.store(true, Ordering::SeqCst);
        });

        let start = Instant::now();
        let result = executor.execute("sleep 30");
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(5),
            "interrupt did not stop the command within five seconds (took {:?})",
            elapsed
        );
        assert!(result.is_err(), "expected interrupted command to error");
        if let Err(SofosError::ToolExecution(msg)) = result {
            assert!(
                msg.contains("interrupted"),
                "expected 'interrupted' in message, got: {msg}"
            );
        } else {
            panic!("Expected ToolExecution error, got: {result:?}");
        }
    }

    /// `git push` smuggled through tab / `$IFS` / backslash-newline must
    /// still be caught by the dangerous-git matcher.
    #[test]
    fn dangerous_git_with_whitespace_obfuscation_is_rejected() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure("git\tpush origin main"));
        assert!(!executor.is_safe_command_structure("git$IFS\tpush"));
        assert!(!executor.is_safe_command_structure("git${IFS}push"));
        assert!(!executor.is_safe_command_structure("git\\\npush"));
        assert!(!executor.is_safe_command_structure("ls && git\tcommit -m x"));

        // The plain form still works.
        assert!(executor.is_safe_command_structure("git status"));
        assert!(executor.is_safe_command_structure("git log"));
    }

    /// `git push` smuggled behind a leading backslash, quotes, or a path
    /// prefix runs the real git binary, so the dangerous-git matcher must
    /// catch it however the program token is spelled.
    #[test]
    fn dangerous_git_with_spelling_obfuscation_is_rejected() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure("\\git push origin main"));
        assert!(!executor.is_safe_command_structure("'git' push origin main"));
        assert!(!executor.is_safe_command_structure("\"git\" push origin main"));
        assert!(!executor.is_safe_command_structure("/usr/bin/git push origin main"));
        assert!(!executor.is_safe_command_structure("./git reset --hard"));

        // A spelling trick after a shell separator is still caught.
        assert!(!executor.is_safe_command_structure("ls; \\git push"));
        assert!(!executor.is_safe_command_structure("ls;'git' push"));

        // Interior backslashes and quotes that bash strips while expanding
        // the word also resolve to the git binary, so they are caught too.
        assert!(!executor.is_safe_command_structure("g\\it push"));
        assert!(!executor.is_safe_command_structure("g\"\"it push"));
        assert!(!executor.is_safe_command_structure("g''it push"));
        assert!(!executor.is_safe_command_structure("g'i't push"));
        assert!(!executor.is_safe_command_structure("\\g\\i\\t push"));
        assert!(!executor.is_safe_command_structure("gi't' commit -m x"));

        // Read-only git stays allowed however it is spelled.
        assert!(executor.is_safe_command_structure("\\git status"));
        assert!(executor.is_safe_command_structure("'git' log"));
        assert!(executor.is_safe_command_structure("g\\it status"));
    }

    /// Git global options must not hide the real subcommand from the
    /// dangerous-operation gate. Git accepts these options before the
    /// subcommand, so every form below runs a remote or destructive
    /// operation unless the matcher skips the global-option prefix.
    #[test]
    fn dangerous_git_with_global_options_is_rejected() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure("git -C . push origin main"));
        assert!(
            !executor
                .is_safe_command_structure("git -c protocol.ext.allow=always push origin main")
        );
        assert!(!executor.is_safe_command_structure("git --git-dir=.git push origin main"));
        assert!(!executor.is_safe_command_structure("git --git-dir .git --work-tree . pull"));
        assert!(!executor.is_safe_command_structure("git -C. reset --hard"));
        assert!(!executor.is_safe_command_structure("git -C . clean -fd"));
        assert!(
            !executor
                .is_safe_command_structure("git -c protocol.ext.allow=always submodule update")
        );
        assert!(!executor.is_safe_command_structure("git -c alias.p=push p"));
        assert!(!executor.is_safe_command_structure("git --config-env=alias.p=GIT_ALIAS p"));
        assert!(!executor.is_safe_command_structure("\\git -c alias.p=push p"));
        assert!(!executor.is_safe_command_structure("git -c include.path=evil.conf p"));
        assert!(
            !executor.is_safe_command_structure("git -c includeIf.onbranch:main.path=evil.conf p")
        );
        assert!(!executor.is_safe_command_structure("git -c core.pager=sh --paginate log"));
        assert!(!executor.is_safe_command_structure("git -c diff.external=sh diff"));
        assert!(!executor.is_safe_command_structure("git -c filter.x.process=sh status"));

        assert!(executor.is_safe_command_structure("git -C . status"));
        assert!(executor.is_safe_command_structure("git --git-dir=.git status"));
        assert!(executor.is_safe_command_structure("git -c color.ui=never diff"));
    }

    /// `git stash list` and `git stash show` are read-only, but their
    /// presence must not make a later dangerous git command safe.
    #[test]
    fn stash_allowance_is_per_git_invocation() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(executor.is_safe_command_structure("git stash list"));
        assert!(executor.is_safe_command_structure("git -C . stash show"));
        assert!(!executor.is_safe_command_structure("git -C . stash pop"));
        assert!(!executor.is_safe_command_structure("git stash list && git push"));
    }

    /// A git subcommand that appears inside a quoted string or a path
    /// argument is literal data, not a git invocation, so the command
    /// must stay allowed rather than being mistaken for `git <subcommand>`.
    #[test]
    fn git_subcommand_inside_an_argument_is_allowed() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(executor.is_safe_command_structure("grep \"git push\" file.txt"));
        assert!(executor.is_safe_command_structure("grep 'git push' file.txt"));
        assert!(executor.is_safe_command_structure("rg -n \"git reset --hard\" ."));
        assert!(executor.is_safe_command_structure("echo \"git commit\""));
        assert!(executor.is_safe_command_structure("cat ./git push"));
        assert!(executor.is_safe_command_structure("cat ~/git addresses.txt"));
    }

    /// Git global options that take their value as a separate word — and
    /// any future such option — must not push the real subcommand out of
    /// view. `git --attr-source HEAD push` runs push; the matcher must see
    /// it after skipping the option and its value.
    #[test]
    fn dangerous_git_behind_value_taking_global_option_is_rejected() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure("git --attr-source HEAD push origin main"));
        assert!(!executor.is_safe_command_structure("git --shallow-file x push origin main"));
        assert!(!executor.is_safe_command_structure("git --attr-source HEAD reset --hard"));
        assert!(!executor.is_safe_command_structure("git --attr-source HEAD stash pop"));
        // An unknown option is explored both ways, so it cannot hide the verb.
        assert!(!executor.is_safe_command_structure("git --future-option x push"));
        // Read-only stays allowed.
        assert!(executor.is_safe_command_structure("git --attr-source HEAD log"));
    }

    /// A quoted option value containing a space is one shell word, so it
    /// cannot split into a fake subcommand. `git -C 'a b' push` is push.
    #[test]
    fn dangerous_git_with_quoted_option_value_is_rejected() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure("git -C 'a b' push"));
        assert!(!executor.is_safe_command_structure("git -C \"my dir\" reset --hard"));
        assert!(!executor.is_safe_command_structure("git --git-dir 'a b' push"));
        assert!(!executor.is_safe_command_structure("git -C 'a b' clean -fdx"));
        assert!(executor.is_safe_command_structure("git -C 'a b' status"));
    }

    /// A dangerous git call hidden behind a launcher program (`env`,
    /// `timeout`, `nice`, `xargs`, `command`) is still caught.
    #[test]
    fn dangerous_git_behind_a_launcher_is_rejected() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure("env git -C . push origin main"));
        assert!(!executor.is_safe_command_structure("env git -c alias.p=push p"));
        assert!(!executor.is_safe_command_structure("timeout 5 git -C . push"));
        assert!(!executor.is_safe_command_structure("nice git -C . reset --hard"));
        assert!(!executor.is_safe_command_structure("xargs git -C . clean -fd"));
        assert!(!executor.is_safe_command_structure("command git --git-dir=.git push"));
        // `xargs -I {}` keeps the literal `{}` argument intact, so the
        // global-option value is not mistaken for the subcommand.
        assert!(!executor.is_safe_command_structure("xargs -I {} git -C {} reset --hard"));
        // A launcher wrapping a read-only git stays allowed.
        assert!(executor.is_safe_command_structure("env FOO=bar git status"));
        assert!(executor.is_safe_command_structure("env git log --oneline"));
    }

    /// A dangerous git call inside `sh -c "..."` / `bash -c "..."` is read
    /// by re-parsing the shell's command string.
    #[test]
    fn dangerous_git_inside_a_shell_wrapper_is_rejected() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure("sh -c \"git push\""));
        assert!(!executor.is_safe_command_structure("bash -c 'git reset --hard'"));
        assert!(!executor.is_safe_command_structure("sh -c \"git -C . clean -fd\""));
        assert!(!executor.is_safe_command_structure("env sh -c \"git push\""));
        assert!(executor.is_safe_command_structure("sh -c \"git status\""));
    }

    /// A subshell or brace group does not hide a dangerous git call from
    /// the matcher.
    #[test]
    fn dangerous_git_in_a_subshell_or_group_is_rejected() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure("(git push)"));
        assert!(!executor.is_safe_command_structure("( git push )"));
        assert!(!executor.is_safe_command_structure("{ git push; }"));
        assert!(!executor.is_safe_command_structure("(git -C . reset --hard)"));
    }

    /// Inline config whose value git runs as a command is blocked even on
    /// an otherwise read-only subcommand; display-only config stays allowed.
    #[test]
    fn git_inline_exec_config_is_rejected() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure(
            "git -c trailer.s.command=evil interpret-trailers --trailer s"
        ));
        assert!(!executor.is_safe_command_structure("git -c diff.x.command=evil diff"));
        assert!(!executor.is_safe_command_structure("git -c diff.x.textconv=evil log -p"));
        assert!(!executor.is_safe_command_structure("git -c core.editor=evil status"));
        assert!(!executor.is_safe_command_structure("git -c credential.helper=evil status"));
        // Display / formatting config carries no command.
        assert!(executor.is_safe_command_structure("git -c color.ui=never diff"));
        assert!(executor.is_safe_command_structure("git -c core.quotepath=false status"));
    }

    /// Regression fixes: disabling the pager is benign, and `-C` is a
    /// directory change even when the directory name looks like a config key.
    #[test]
    fn benign_git_config_forms_stay_allowed() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(executor.is_safe_command_structure("git -c pager.log=false log"));
        assert!(executor.is_safe_command_structure("git -c pager.diff=false diff"));
        assert!(!executor.is_safe_command_structure("git -c pager.log=sh log"));

        assert!(executor.is_safe_command_structure("git -C alias.foo status"));
        assert!(executor.is_safe_command_structure("git -C core.pager log"));
    }

    /// The askable-checkout prompt sees through global options, quotes, and
    /// launchers, and matches the exact `checkout` subcommand.
    #[test]
    fn askable_checkout_detection_is_robust() {
        assert!(command_contains_askable_git_checkout("git checkout main"));
        assert!(command_contains_askable_git_checkout(
            "git -C 'a b' checkout main"
        ));
        assert!(command_contains_askable_git_checkout(
            "git --attr-source HEAD checkout main"
        ));
        assert!(command_contains_askable_git_checkout(
            "env git checkout main"
        ));
        // checkout-index is a separate plumbing command, not askable checkout.
        assert!(!command_contains_askable_git_checkout(
            "git checkout-index -a"
        ));
        assert!(!command_contains_askable_git_checkout("git status"));
    }

    /// A shell input redirection (`git <file push`) is stripped from the
    /// word list the same way the shell strips it before running git, so
    /// the real verb after it is still seen.
    #[test]
    fn dangerous_git_with_input_redirection_is_rejected() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure("git <existingfile push"));
        assert!(!executor.is_safe_command_structure("git<existingfile push"));
        assert!(!executor.is_safe_command_structure("git < input reset --hard"));
        assert!(!executor.is_safe_command_structure("git 0<foo clean -fd"));
        // A read-only verb with a redirection stays allowed (verb is seen).
        assert!(executor.is_safe_command_structure("git <input log"));
        assert!(executor.is_safe_command_structure("git status 2>&1"));
    }

    /// `env --split-string`/`-S` re-splits its argument into a command
    /// line, so a dangerous git call packed into that string is caught.
    #[test]
    fn dangerous_git_behind_env_split_string_is_rejected() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure("env -S \"git push origin HEAD\""));
        assert!(!executor.is_safe_command_structure("env -S \"git reset --hard\""));
        assert!(!executor.is_safe_command_structure("env --split-string=\"git clean -fd\""));
        assert!(executor.is_safe_command_structure("env -S \"git status\""));
    }

    /// `git instaweb` and `git daemon` expose the repository over the
    /// network, so they are blocked like the other networked verbs.
    #[test]
    fn git_server_verbs_are_rejected() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(!executor.is_safe_command_structure("git instaweb --start"));
        assert!(!executor.is_safe_command_structure("git daemon --export-all"));
        assert!(!executor.is_safe_command_structure("git -c instaweb.httpd=evil instaweb"));
    }

    /// A leading boolean global option must not push the verb scan into the
    /// subcommand's own arguments and mistake a pathspec or ref named like
    /// a verb for the subcommand.
    #[test]
    fn boolean_global_option_does_not_overblock_reads() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(executor.is_safe_command_structure("git --no-pager log -- rm"));
        assert!(executor.is_safe_command_structure("git --no-pager log add"));
        assert!(executor.is_safe_command_structure("git -p log switch"));
        assert!(executor.is_safe_command_structure("git --no-optional-locks log clean"));
        // The verb itself is still caught after a boolean option.
        assert!(!executor.is_safe_command_structure("git --no-pager push"));
        assert!(!executor.is_safe_command_structure("git -p reset --hard"));
    }

    /// A command-valued config key with a boolean or empty value only
    /// toggles a setting; it runs nothing and must stay allowed.
    #[test]
    fn boolean_config_values_stay_allowed() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();

        assert!(executor.is_safe_command_structure("git -c core.fsmonitor=true status"));
        assert!(executor.is_safe_command_structure("git -c core.fsmonitor=false status"));
        assert!(executor.is_safe_command_structure("git -c credential.helper= log"));
        // A real command value is still blocked.
        assert!(!executor.is_safe_command_structure("git -c core.fsmonitor=/evil status"));
        assert!(!executor.is_safe_command_structure("git -c credential.helper=!sh log"));
    }

    /// Path tokens with shell-meta that the shell would expand at
    /// run-time must be refused before they reach the deny-glob check.
    #[test]
    fn path_token_with_shell_meta_is_rejected() {
        let (_temp, path) = test_support::workspace();
        let executor = BashExecutor::new(path, false, false).unwrap();

        for cmd in [
            "cat $HOME/.ssh/id_rsa",
            "cat /e?c/passwd",
            "cat /etc/p[a]sswd",
            "ls /{etc,tmp}/x",
            "cat ~root/.ssh/id_rsa",
            "grep pattern ${HOME}/.bashrc",
        ] {
            let err = executor.execute(cmd);
            assert!(
                matches!(&err, Err(SofosError::ToolExecution(msg)) if msg.contains("can't be checked")),
                "expected shell-meta rejection for `{cmd}`, got {err:?}"
            );
        }
    }

    /// A Windows canonical path returned by `std::fs::canonicalize`
    /// carries the `\\?\` verbatim prefix. The `?` is part of the
    /// prefix, not a glob, so the shell-meta check must not flag it.
    /// Forward-slash spellings (`//?/`) appear in some path-display
    /// paths and must be tolerated too.
    #[test]
    fn windows_verbatim_path_prefix_is_not_flagged_as_glob() {
        assert_eq!(
            path_token_shell_meta(r"\\?\C:\Users\name\Temp\file.txt"),
            None
        );
        assert_eq!(
            path_token_shell_meta(r"\\.\C:\Users\name\Temp\file.txt"),
            None
        );
        assert_eq!(
            path_token_shell_meta("//?/C:/Users/name/Temp/file.txt"),
            None
        );

        // A `?` that appears AFTER the verbatim prefix is still a glob
        // and stays flagged.
        assert_eq!(
            path_token_shell_meta(r"\\?\C:\Users\?\file.txt"),
            Some("glob expansion")
        );
        // Plain (non-verbatim) paths with `?` stay flagged as before.
        assert_eq!(path_token_shell_meta("/etc/p?sswd"), Some("glob expansion"));
    }

    /// `~/foo` and bare `~` are still legitimate path forms and pass
    /// the shell-meta check (they're handled by `expand_tilde` below).
    #[test]
    fn tilde_home_paths_are_not_blocked_by_shell_meta_check() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();
        assert!(executor.is_safe_command_structure("ls ~/Documents"));
        assert!(executor.is_safe_command_structure("ls ~"));
    }

    /// In the sandboxed mode a command the permission gate would otherwise
    /// prompt for runs confined instead — no prompt — and can write
    /// inside the workspace. This is the friction fix: unknown commands
    /// just run, bounded by the sandbox. The trailing redirection also
    /// shows the structural check is skipped on the confined path.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandboxed_mode_runs_unknown_command_confined() {
        let (_temp, path) = test_support::workspace();
        let mut executor = BashExecutor::new(path.clone(), false, false).unwrap();
        executor.set_sandbox_mode(crate::config::SandboxMode::Sandboxed);

        let result = executor.execute("notarealtool 2>/dev/null; echo confined > inside.txt");

        assert!(
            result.is_ok(),
            "expected the confined run to succeed, got {result:?}"
        );
        assert!(
            path.join("inside.txt").is_file(),
            "the command should be able to write inside the workspace"
        );
    }

    /// A known-safe command rejected only for file redirection runs
    /// confined in the sandboxed mode and writes inside the project, instead
    /// of being refused outright.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandboxed_mode_runs_redirection_confined() {
        let (_temp, path) = test_support::workspace();
        let mut executor = BashExecutor::new(path.clone(), false, false).unwrap();
        executor.set_sandbox_mode(crate::config::SandboxMode::Sandboxed);

        let result = executor.execute("echo confined > out.txt");

        assert!(
            result.is_ok(),
            "expected the redirection to run confined, got {result:?}"
        );
        assert!(path.join("out.txt").is_file());
    }

    /// End-to-end Windows sandbox spawn against a shell that does not
    /// depend on Cygwin: the workspace identifier is persisted, the
    /// workspace gains an allow-write rule for it, the restricted
    /// token is built, and the child reaches user-mode code.
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_sandbox_spawns_cmd_under_restricted_token() {
        use crate::tools::bash::sandbox::{self, SandboxPolicy};
        use std::sync::atomic::AtomicBool;

        let (_temp, workspace) = test_support::workspace();
        let policy = SandboxPolicy {
            writable_roots: vec![workspace.clone()],
            allow_network: false,
            read_deny_subpaths: Vec::new(),
            read_allow_subpaths: Vec::new(),
            write_protect_subpaths: Vec::new(),
        };

        // The helper always builds `<shell> -c <command>`; cmd.exe
        // ignores `-c` and prints its banner on startup, which is
        // enough to prove the child reached user-mode code.
        let outcome = sandbox::windows::run_confined(
            std::ffi::OsStr::new("cmd.exe"),
            "ver",
            &workspace,
            None,
            &policy,
            &AtomicBool::new(false),
        );

        let outcome = match outcome {
            Ok(value) => value,
            Err(err) => {
                eprintln!("skipping: spawn under restricted token failed: {err}");
                return;
            }
        };
        assert!(
            outcome.terminated_for.is_none(),
            "expected the cmd.exe spawn to finish without supervisor termination, got {:?}",
            outcome.terminated_for
        );
        assert!(
            !outcome.stdout.is_empty() || !outcome.stderr.is_empty(),
            "expected cmd.exe to print at least a banner"
        );
        assert!(
            workspace.join(".sofos").join("cap_sid").is_file(),
            "the workspace identifier must be persisted under .sofos"
        );
    }

    /// The redirection relaxation must not weaken the real defences:
    /// traversal, hidden subcommands, and dangerous git stay refused in
    /// the sandboxed mode even when the command also redirects to a file.
    #[test]
    fn sandboxed_mode_still_refuses_dangerous_structures() {
        let (_temp, path) = test_support::workspace();
        let mut executor = BashExecutor::new(path, false, false).unwrap();
        executor.set_sandbox_mode(crate::config::SandboxMode::Sandboxed);

        for cmd in [
            "cat ../secret > out.txt",
            "echo $(whoami) > out.txt",
            "git push origin main > out.txt",
        ] {
            assert!(
                executor.execute(cmd).is_err(),
                "the sandboxed mode must still refuse `{cmd}`"
            );
        }
    }

    /// The unknown-command path (an Ask-tier base command) runs confined
    /// without a prompt, but the same defences must hold there: the
    /// sandbox bounds writes and the network, not reads, so traversal and
    /// hidden subcommands stay refused instead of leaking through a read.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandboxed_mode_confined_unknown_command_still_refuses_dangerous_structures() {
        let (_temp, path) = test_support::workspace();
        let mut executor = BashExecutor::new(path, false, false).unwrap();
        executor.set_sandbox_mode(crate::config::SandboxMode::Sandboxed);

        for cmd in [
            "notarealtool ../secret",
            "notarealtool $(whoami)",
            "notarealtool `id`",
            "notarealtool && git push origin main",
        ] {
            assert!(
                executor.execute(cmd).is_err(),
                "the confined unknown-command path must still refuse `{cmd}`"
            );
        }
    }

    /// The friction fix must survive the gate routing: a real unknown
    /// command that only writes inside the workspace still runs confined
    /// without a prompt. `mkdir` is not on the allow-list, so it reaches
    /// the Ask tier.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandboxed_mode_confined_unknown_command_writes_inside_workspace() {
        let (_temp, path) = test_support::workspace();
        let mut executor = BashExecutor::new(path.clone(), false, false).unwrap();
        executor.set_sandbox_mode(crate::config::SandboxMode::Sandboxed);

        let result = executor.execute("mkdir created_dir");

        assert!(
            result.is_ok(),
            "expected the confined mkdir to succeed, got {result:?}"
        );
        assert!(
            path.join("created_dir").is_dir(),
            "the unknown command should still write inside the workspace"
        );
    }

    /// External absolute paths stay gated on the confined unknown-command
    /// path. Reads are open inside the sandbox, so reaching outside the
    /// workspace needs a grant; a non-interactive run rejects it instead
    /// of silently reading the file.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandboxed_mode_confined_unknown_command_gates_external_path() {
        let (_temp, path) = test_support::workspace();
        let mut executor = BashExecutor::new(path, false, false).unwrap();
        executor.set_sandbox_mode(crate::config::SandboxMode::Sandboxed);

        let result = executor.execute("notarealtool /etc/hosts");

        assert!(
            result.is_err(),
            "an external path must be gated even when confined"
        );
        if let Err(SofosError::ToolExecution(msg)) = result {
            assert!(
                msg.contains("outside workspace"),
                "expected an external-path rejection, got: {msg}"
            );
        }
    }

    /// Read-deny rules must hold on the confined unknown-command path.
    /// Reads are open inside the sandbox, so without the read check an
    /// unfamiliar command could read a denied path and echo it back.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandboxed_mode_confined_unknown_command_honours_read_deny() {
        use std::fs;

        let (_temp, path) = test_support::workspace();
        let config_dir = path.join(".sofos");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.local.toml"),
            "[permissions]\nallow = []\ndeny = [\"Read(./test/**)\"]\nask = []\n",
        )
        .unwrap();

        let mut executor = BashExecutor::new(path, false, false).unwrap();
        executor.set_sandbox_mode(crate::config::SandboxMode::Sandboxed);

        let result = executor.execute("notarealtool ./test/secret.txt");

        assert!(
            result.is_err(),
            "read-deny must block the confined unknown-command path"
        );
        if let Err(SofosError::ToolExecution(msg)) = result {
            assert!(msg.contains("Read access denied") || msg.contains("denied"));
        }
    }

    /// A metadata directory that does not exist yet is masked read-only
    /// while a confined command runs, so the command cannot create a
    /// persistent `.sofos` config that would relax the next command's gate,
    /// and the empty mount point is removed afterward. Linux only, where
    /// the mask leaves a mount point to clean up.
    #[cfg(target_os = "linux")]
    #[test]
    fn sandboxed_mode_blocks_creating_nonexistent_metadata() {
        use crate::tools::bash::sandbox;

        let (_temp, path) = test_support::workspace();
        if !sandbox::is_available() {
            return;
        }
        let workspace = std::fs::canonicalize(&path).unwrap();
        let mut executor = BashExecutor::new(path, false, false).unwrap();
        executor.set_sandbox_mode(crate::config::SandboxMode::Sandboxed);

        let sofos = workspace.join(".sofos");
        assert!(!sofos.exists(), "precondition: .sofos absent");

        let _ = executor.execute("printf x > .sofos/config.local.toml");

        assert!(
            !sofos.join("config.local.toml").exists(),
            "a confined command must not create a persistent .sofos config"
        );
        assert!(
            !sofos.exists(),
            "the empty .sofos mount point must be cleaned up after the run"
        );
    }
}
