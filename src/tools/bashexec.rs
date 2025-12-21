use crate::error::{Result, SofosError};
use crate::tools::permissions::{CommandPermission, PermissionManager};
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

const MAX_OUTPUT_SIZE: usize = 10 * 1024 * 1024; // 10MB limit

/// Convert Unix signal number to human-readable name
#[cfg(unix)]
fn signal_name(sig: i32) -> &'static str {
    match sig {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => "unknown",
    }
}

#[derive(Clone)]
pub struct BashExecutor {
    workspace: PathBuf,
    /// Session-scoped temporary permissions (not persisted to config)
    session_allowed: Arc<Mutex<HashSet<String>>>,
    session_denied: Arc<Mutex<HashSet<String>>>,
}

impl BashExecutor {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        Ok(Self {
            workspace,
            session_allowed: Arc::new(Mutex::new(HashSet::new())),
            session_denied: Arc::new(Mutex::new(HashSet::new())),
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
                    "Command blocked (denied earlier this session): '{}'",
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
                        "Command blocked by user: '{}'",
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
        let permission_manager = PermissionManager::new(self.workspace.clone())?;

        // Enforce read permissions on paths referenced in the command
        self.enforce_read_permissions(&permission_manager, command)?;

        // Additional safety checks (absolute paths, parent traversal, git restrictions)
        if !self.is_safe_command_structure(command) {
            return Err(SofosError::ToolExecution(
                self.get_rejection_reason(command),
            ));
        }

        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.workspace)
            .output()
            .map_err(|e| SofosError::ToolExecution(format!("Failed to execute command: {}", e)))?;

        if output.stdout.len() > MAX_OUTPUT_SIZE {
            return Err(SofosError::ToolExecution(format!(
                "Command output too large ({} bytes). Maximum size is {} MB",
                output.stdout.len(),
                MAX_OUTPUT_SIZE / (1024 * 1024)
            )));
        }

        if output.stderr.len() > MAX_OUTPUT_SIZE {
            return Err(SofosError::ToolExecution(format!(
                "Command error output too large ({} bytes). Maximum size is {} MB",
                output.stderr.len(),
                MAX_OUTPUT_SIZE / (1024 * 1024)
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
                            Some(sig) => format!("signal: {} ({})", sig, signal_name(sig)),
                            None => "unknown termination".to_string(),
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        "unknown termination".to_string()
                    }
                }
            };
            return Ok(format!(
                "Command failed with {}\nSTDOUT:\n{}\nSTDERR:\n{}",
                exit_info, stdout, stderr
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

        Ok(result)
    }

    fn enforce_read_permissions(
        &self,
        permission_manager: &PermissionManager,
        command: &str,
    ) -> Result<()> {
        // Heuristic-based detection of file paths in commands:
        // - Paths with '/' or starting with '.' or '~'
        // - Simple filenames (no shell metacharacters like '$', '`', '*', '?', '[')
        //
        // IMPORTANT: Bash commands are ALWAYS restricted to workspace, even if
        // paths are in the Read allow list. Allow list only applies to read_file tool.
        for token in command.split_whitespace().skip(1) {
            let cleaned = token
                .trim_matches('"')
                .trim_matches('\'')
                .trim_matches(';')
                .trim();

            if cleaned.is_empty() || cleaned.starts_with('-') {
                continue;
            }

            let is_path = cleaned.contains('/')
                || cleaned.starts_with('.')
                || cleaned.starts_with('~')
                || (!cleaned.contains('$')
                    && !cleaned.contains('`')
                    && !cleaned.contains('*')
                    && !cleaned.contains('?')
                    && !cleaned.contains('['));

            if is_path {
                // For deny rules: check if explicitly denied
                let (perm, matched_rule) =
                    permission_manager.check_read_permission_with_source(cleaned);
                match perm {
                    CommandPermission::Allowed => {}
                    CommandPermission::Denied => {
                        let config_source = if let Some(ref rule) = matched_rule {
                            permission_manager.get_rule_source(rule)
                        } else {
                            ".sofos/config.local.toml or ~/.sofos/config.toml".to_string()
                        };
                        return Err(SofosError::ToolExecution(format!(
                            "Read blocked by deny rule in {} for path '{}' in command.",
                            config_source, cleaned
                        )));
                    }
                    CommandPermission::Ask => {
                        return Err(SofosError::ToolExecution(format!(
                            "Path '{}' requires confirmation per config file. \
                            Move it to allow or deny list.",
                            cleaned
                        )));
                    }
                }
            }
        }

        Ok(())
    }

    fn is_safe_command_structure(&self, command: &str) -> bool {
        if command.contains("..") {
            return false;
        }

        // Check for absolute paths
        if command.starts_with('/') {
            return false;
        }

        if command.contains(" /") {
            return false;
        }

        if command.contains("|/")
            || command.contains(";/")
            || command.contains("&&/")
            || command.contains("||/")
        {
            return false;
        }

        if command.contains("~/") || command.starts_with('~') {
            return false;
        }

        // Allow "2>&1" (stderr to stdout redirection) but block file output redirection
        let command_without_stderr_redirect = command.replace("2>&1", "");

        if command_without_stderr_redirect.contains('>')
            || command_without_stderr_redirect.contains(">>")
        {
            return false;
        }

        if command.contains("<<") {
            return false;
        }

        if !self.is_safe_git_command(&command.to_lowercase()) {
            return false;
        }

        true
    }

    fn is_safe_git_command(&self, command: &str) -> bool {
        if !command.starts_with("git ")
            && !command.contains(" git ")
            && !command.contains(";git ")
            && !command.contains("&&git ")
            && !command.contains("||git ")
            && !command.contains("|git ")
        {
            return true;
        }

        // Allow safe git stash read-only operations
        if command.contains("git stash list") || command.contains("git stash show") {
            return true;
        }

        // Dangerous git operations that are completely blocked
        let dangerous_git_ops = [
            "git push",
            "git pull",
            "git fetch",
            "git clone",
            "git clean",
            "git reset --hard",
            "git reset --mixed",
            "git checkout -f",
            "git checkout -b",
            "git checkout --",
            "git branch -d",
            "git branch -D",
            "git branch -m",
            "git branch -M",
            "git remote add",
            "git remote set-url",
            "git remote remove",
            "git remote rm",
            "git submodule",
            "git filter-branch",
            "git gc",
            "git prune",
            "git update-ref",
            "git send-email",
            "git apply",
            "git am",
            "git cherry-pick",
            "git revert",
            "git commit",
            "git merge",
            "git rebase",
            "git tag -d",
            "git stash",
            "git init",
            "git add",
            "git rm",
            "git mv",
            "git restore",
            "git switch",
        ];

        for dangerous_op in &dangerous_git_ops {
            if command.starts_with(dangerous_op)
                || command.contains(&format!(" {}", dangerous_op))
                || command.contains(&format!(";{}", dangerous_op))
                || command.contains(&format!("&&{}", dangerous_op))
                || command.contains(&format!("||{}", dangerous_op))
                || command.contains(&format!("|{}", dangerous_op))
            {
                return false;
            }
        }

        true
    }

    fn get_rejection_reason(&self, command: &str) -> String {
        let command_lower = command.to_lowercase();

        // Parent directory traversal
        if command.contains("..") {
            return format!(
                "Command blocked: '{}'\nReason: Contains '..' (parent directory traversal).\nAll operations must stay within the current workspace directory.",
                command
            );
        }

        // Absolute paths
        if command.starts_with('/')
            || command.contains(" /")
            || command.contains("|/")
            || command.contains(";/")
            || command.contains("&&/")
            || command.contains("||/")
        {
            return format!(
                "Command blocked: '{}'\nReason: Contains absolute paths (starting with '/').\nOnly relative paths within the workspace are allowed.",
                command
            );
        }

        if command.contains("~/") || command.starts_with('~') {
            return format!(
                "Command blocked: '{}'\nReason: Contains tilde paths ('~').\nBash commands are restricted to workspace only. Use read_file/list_directory tools for outside access.",
                command
            );
        }

        if !self.is_safe_git_command(&command_lower) {
            return self.get_git_rejection_reason(command);
        }

        // Output redirection (check after removing 2>&1)
        let command_without_stderr_redirect = command.replace("2>&1", "");
        if command_without_stderr_redirect.contains('>')
            || command_without_stderr_redirect.contains(">>")
        {
            return format!(
                "Command blocked: '{}'\nReason: Contains output redirection ('>' or '>>').\nNote: '2>&1' is allowed for combining stderr and stdout.\nUse the write_file tool to create or modify files instead.",
                command
            );
        }

        // Here-doc
        if command.contains("<<") {
            return format!(
                "Command blocked: '{}'\nReason: Contains here-doc ('<<').\nUse the write_file tool to create files instead.",
                command
            );
        }

        format!(
            "Command blocked: '{}'\nReason: Command is in the forbidden list (destructive or violates sandbox).\nUse appropriate file operation tools instead.",
            command
        )
    }

    fn get_git_rejection_reason(&self, command: &str) -> String {
        let command_lower = command.to_lowercase();

        if command_lower.contains("git push") {
            return format!(
                "Command blocked: '{}'\nReason: 'git push' sends data to remote repositories (network operation).\nAllowed: Use 'git status', 'git log', 'git diff' to view changes.",
                command
            );
        }

        if command_lower.contains("git pull") || command_lower.contains("git fetch") {
            return format!(
                "Command blocked: '{}'\nReason: '{}' fetches data from remote repositories (network operation).\nAllowed: Use 'git status', 'git log', 'git diff' to view local changes.",
                command,
                if command_lower.contains("git pull") { "git pull" } else { "git fetch" }
            );
        }

        if command_lower.contains("git clone") {
            return format!(
                "Command blocked: '{}'\nReason: 'git clone' downloads repositories (network operation and creates directories).\nClone repositories manually outside of Sofos.",
                command
            );
        }

        if command_lower.contains("git commit") || command_lower.contains("git add") {
            return format!(
                "Command blocked: '{}'\nReason: '{}' modifies the git repository.\nAllowed: Use 'git status', 'git diff' to view changes. Create commits manually.",
                command,
                if command_lower.contains("git commit") { "git commit" } else { "git add" }
            );
        }

        if command_lower.contains("git reset") || command_lower.contains("git clean") {
            return format!(
                "Command blocked: '{}'\nReason: '{}' is a destructive operation that discards changes.\nAllowed: Use 'git status', 'git log', 'git diff' to view repository state.",
                command,
                if command_lower.contains("git reset") { "git reset" } else { "git clean" }
            );
        }

        if command_lower.contains("git checkout") || command_lower.contains("git switch") {
            return format!(
                "Command blocked: '{}'\nReason: '{}' changes branches or modifies working directory.\nAllowed: Use 'git branch' to list branches, 'git status' to see current branch.",
                command,
                if command_lower.contains("git checkout") { "git checkout" } else { "git switch" }
            );
        }

        if command_lower.contains("git merge") || command_lower.contains("git rebase") {
            return format!(
                "Command blocked: '{}'\nReason: '{}' modifies git history and repository state.\nPerform merges/rebases manually outside of Sofos.",
                command,
                if command_lower.contains("git merge") { "git merge" } else { "git rebase" }
            );
        }

        if command_lower.contains("git stash")
            && !command_lower.contains("git stash list")
            && !command_lower.contains("git stash show")
        {
            return format!(
                "Command blocked: '{}'\nReason: 'git stash' (without list/show) modifies repository state.\nAllowed: Use 'git stash list' or 'git stash show' to view stashed changes.",
                command
            );
        }

        if command_lower.contains("git remote add") || command_lower.contains("git remote set-url")
        {
            return format!(
                "Command blocked: '{}'\nReason: Modifying git remotes could redirect pushes to unauthorized servers.\nAllowed: Use 'git remote -v' to view configured remotes.",
                command
            );
        }

        if command_lower.contains("git submodule") {
            return format!(
                "Command blocked: '{}'\nReason: 'git submodule' can fetch from remote repositories (network operation).\nManage submodules manually outside of Sofos.",
                command
            );
        }

        format!(
            "Command blocked: '{}'\nReason: Contains git operation that modifies repository or accesses network.\nAllowed git commands: status, log, diff, show, branch, remote -v, grep, blame, stash list/show",
            command
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_commands() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

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
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        // Test structural safety issues (not permission-based)
        assert!(!executor.is_safe_command_structure("echo hello > file.txt"));
        assert!(!executor.is_safe_command_structure("cat file.txt >> output.txt"));

        // These should still be blocked (file redirection even with 2>&1)
        assert!(!executor.is_safe_command_structure("echo hello > file.txt 2>&1"));
        assert!(!executor.is_safe_command_structure("cargo build 2>&1 > output.txt"));
    }

    #[test]
    fn test_path_traversal_blocked() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        assert!(!executor.is_safe_command_structure("cat ../file.txt"));
        assert!(!executor.is_safe_command_structure("ls ../../etc"));
        assert!(!executor.is_safe_command_structure("cat ../../../etc/passwd"));
        assert!(!executor.is_safe_command_structure("cat file.txt && ls .."));
        assert!(!executor.is_safe_command_structure("ls | cat ../secret"));
    }

    #[test]
    fn test_absolute_paths_blocked() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        assert!(!executor.is_safe_command_structure("/bin/ls"));
        assert!(!executor.is_safe_command_structure("/etc/passwd"));
        assert!(!executor.is_safe_command_structure("cat /etc/passwd"));
        assert!(!executor.is_safe_command_structure("ls /tmp"));
        assert!(!executor.is_safe_command_structure("cat /home/user/secret"));
        assert!(!executor.is_safe_command_structure("ls && cat /etc/passwd"));
        assert!(!executor.is_safe_command_structure("echo test || cat /etc/passwd"));
        assert!(!executor.is_safe_command_structure("ls | grep /etc/passwd"));
        assert!(!executor.is_safe_command_structure("true;/bin/bash"));
    }

    #[test]
    fn test_output_size_limit() {
        use tempfile;

        let temp_dir = tempfile::tempdir().unwrap();
        let executor = BashExecutor::new(temp_dir.path().to_path_buf()).unwrap();

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
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();

        // Write deny config for test folder reads
        let config_dir = temp_dir.path().join(".sofos");
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

        let executor = BashExecutor::new(temp_dir.path().to_path_buf()).unwrap();

        // Even without creating the file, permission check should block before execution
        let result = executor.execute("cat ./test/secret.txt");

        assert!(result.is_err());
        if let Err(SofosError::ToolExecution(msg)) = result {
            assert!(msg.contains("Read blocked"));
        } else {
            panic!("Expected ToolExecution error");
        }
    }

    #[test]
    fn test_safe_git_commands() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

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
    }

    #[test]
    fn test_dangerous_git_commands() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

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
        assert!(!executor.is_safe_command_structure("git restore file.txt"));
        assert!(!executor.is_safe_command_structure("git switch main"));

        // Remote configuration changes
        assert!(
            !executor.is_safe_command_structure("git remote add origin https://evil.com/repo.git")
        );
        assert!(!executor
            .is_safe_command_structure("git remote set-url origin https://evil.com/repo.git"));
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
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

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
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        let reason = executor.get_git_rejection_reason("git push origin main");
        assert!(reason.contains("git push origin main"));
        assert!(reason.contains("network operation"));
        assert!(reason.contains("git status"));

        let reason = executor.get_rejection_reason("cd /tmp");
        assert!(reason.contains("cd /tmp"));
        assert!(reason.contains("absolute paths"));
    }

    #[test]
    fn test_tilde_paths_blocked_in_bash() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        assert!(!executor.is_safe_command_structure("ls ~/tmp"));
        assert!(!executor.is_safe_command_structure("cat ~/file.txt"));
        assert!(!executor.is_safe_command_structure("grep pattern ~/docs/file.txt"));
        assert!(!executor.is_safe_command_structure("echo test && ls ~/dir"));

        let reason = executor.get_rejection_reason("ls ~/tmp/allowed");
        assert!(reason.contains("tilde paths"));
        assert!(reason.contains("read_file"));
        assert!(reason.contains("workspace only"));
    }

    #[test]
    fn test_session_scoped_permissions_persist() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

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
        let executor1 = BashExecutor::new(PathBuf::from(".")).unwrap();
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
}
