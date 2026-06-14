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

use crate::config::SandboxMode;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct BashExecutor {
    pub(super) workspace: PathBuf,
    /// Whether interactive prompts (stdin) are available
    pub(super) interactive: bool,
    /// Whether `morph_edit_file` is exposed (drives error-message hints)
    pub(super) has_morph: bool,
    /// Access mode in effect. In workspace mode, commands the permission
    /// gate would otherwise refuse are run confined by the sandbox.
    pub(super) mode: SandboxMode,
    /// Session-scoped temporary permissions (not persisted to config)
    pub(super) session_allowed: Arc<Mutex<HashSet<String>>>,
    pub(super) session_denied: Arc<Mutex<HashSet<String>>>,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SofosError;
    use crate::tools::bash::validate::{
        command_contains_op, detect_command_substitution, has_path_traversal,
    };
    use crate::tools::test_support;

    /// `command_contains_op` gates our forbidden-git detection. A miss
    /// here is a real security bypass: the model could wrap `git push`
    /// in a subshell or command substitution and slip past.
    #[test]
    fn command_contains_op_catches_shell_boundaries() {
        assert!(command_contains_op("git push", "git push"));
        assert!(command_contains_op("ls; git push", "git push"));
        assert!(command_contains_op("ls && git push", "git push"));
        assert!(command_contains_op("ls || git push", "git push"));
        assert!(command_contains_op("ls | git push", "git push"));

        // Shell-substitution boundaries — the regressions from the audit.
        assert!(command_contains_op("echo hi; `git push`", "git push"));
        assert!(command_contains_op("echo $(git push)", "git push"));
        assert!(command_contains_op("(git push)", "git push"));
        assert!(command_contains_op("{ git push; }", "git push"));

        // Genuinely unrelated commands shouldn't trigger.
        assert!(!command_contains_op("rgit push", "git push")); // non-boundary prefix
        assert!(!command_contains_op("ls", "git push"));
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

    /// `~/foo` and bare `~` are still legitimate path forms and pass
    /// the shell-meta check (they're handled by `expand_tilde` below).
    #[test]
    fn tilde_home_paths_are_not_blocked_by_shell_meta_check() {
        let executor = BashExecutor::new(PathBuf::from("."), false, false).unwrap();
        assert!(executor.is_safe_command_structure("ls ~/Documents"));
        assert!(executor.is_safe_command_structure("ls ~"));
    }

    /// In workspace mode a command the permission gate would otherwise
    /// prompt for runs confined instead — no prompt — and can write
    /// inside the workspace. This is the friction fix: unknown commands
    /// just run, bounded by the sandbox. The trailing redirection also
    /// shows the structural check is skipped on the confined path.
    #[cfg(target_os = "macos")]
    #[test]
    fn workspace_mode_runs_unknown_command_confined() {
        let (_temp, path) = test_support::workspace();
        let mut executor = BashExecutor::new(path.clone(), false, false).unwrap();
        executor.set_sandbox_mode(crate::config::SandboxMode::Workspace);

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
    /// confined in workspace mode and writes inside the project, instead
    /// of being refused outright.
    #[cfg(target_os = "macos")]
    #[test]
    fn workspace_mode_runs_redirection_confined() {
        let (_temp, path) = test_support::workspace();
        let mut executor = BashExecutor::new(path.clone(), false, false).unwrap();
        executor.set_sandbox_mode(crate::config::SandboxMode::Workspace);

        let result = executor.execute("echo confined > out.txt");

        assert!(
            result.is_ok(),
            "expected the redirection to run confined, got {result:?}"
        );
        assert!(path.join("out.txt").is_file());
    }

    /// The redirection relaxation must not weaken the real defences:
    /// traversal, hidden subcommands, and dangerous git stay refused in
    /// workspace mode even when the command also redirects to a file.
    #[test]
    fn workspace_mode_still_refuses_dangerous_structures() {
        let (_temp, path) = test_support::workspace();
        let mut executor = BashExecutor::new(path, false, false).unwrap();
        executor.set_sandbox_mode(crate::config::SandboxMode::Workspace);

        for cmd in [
            "cat ../secret > out.txt",
            "echo $(whoami) > out.txt",
            "git push origin main > out.txt",
        ] {
            assert!(
                executor.execute(cmd).is_err(),
                "workspace mode must still refuse `{cmd}`"
            );
        }
    }
}
