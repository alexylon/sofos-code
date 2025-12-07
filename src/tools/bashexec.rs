use crate::error::{Result, SofosError};
use std::path::PathBuf;
use std::process::Command;

const MAX_OUTPUT_SIZE: usize = 10 * 1024 * 1024; // 10MB limit

pub struct BashExecutor {
    workspace: PathBuf,
}

impl BashExecutor {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        Ok(Self { workspace })
    }

    pub fn execute(&self, command: &str) -> Result<String> {
        if !self.is_safe_command(command) {
            return Err(SofosError::ToolExecution(
                "Command is not allowed. Only read-only commands are permitted.".to_string(),
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
            return Ok(format!(
                "Command failed with exit code: {}\nSTDOUT:\n{}\nSTDERR:\n{}",
                output.status.code().unwrap_or(-1),
                stdout,
                stderr
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

    fn is_safe_command(&self, command: &str) -> bool {
        let command_lower = command.to_lowercase();

        if command_lower.starts_with("sudo") || command_lower.contains(" sudo ") {
            return false;
        }

        if command.contains("..") {
            return false;
        }

        let directory_commands = [
            "cd ", "cd\t", "cd;", "cd&", "cd|", "pushd ", "pushd\t", "pushd;", "pushd&", "pushd|",
            "popd ", "popd\t", "popd;", "popd&", "popd|",
        ];
        for cmd in &directory_commands {
            if command_lower.starts_with(cmd.trim_end())
                || command_lower.contains(&format!(" {}", cmd.trim_end()))
                || command_lower.contains(&format!(";{}", cmd.trim_end()))
                || command_lower.contains(&format!("&&{}", cmd.trim_end()))
                || command_lower.contains(&format!("||{}", cmd.trim_end()))
                || command_lower.contains(&format!("|{}", cmd.trim_end()))
            {
                return false;
            }
        }

        if command.starts_with('/') {
            return false;
        }

        // Catches: cat /etc/passwd, ls /tmp, etc.
        if command.contains(" /") {
            return false;
        }

        // Check absolute paths after pipes, semicolons, and logical operators
        if command.contains("|/")
            || command.contains(";/")
            || command.contains("&&/")
            || command.contains("||/")
        {
            return false;
        }

        if !self.is_safe_git_command(&command_lower) {
            return false;
        }

        let forbidden_commands = [
            "rm",
            "mv",
            "cp",
            "chmod",
            "chown",
            "chgrp",
            "mkdir",
            "rmdir",
            "touch",
            "ln",
            "dd",
            "mkfs",
            "mount",
            "umount",
            "kill",
            "killall",
            "pkill",
            "shutdown",
            "reboot",
            "halt",
            "poweroff",
            "useradd",
            "userdel",
            "groupadd",
            "groupdel",
            "passwd",
            "systemctl",
            "service",
            "fdisk",
            "parted",
            "mkswap",
            "swapon",
            "swapoff",
        ];

        for forbidden in &forbidden_commands {
            if command_lower.starts_with(forbidden)
                || command_lower.starts_with(&format!("{} ", forbidden))
            {
                return false;
            }
            // Check chained commands
            if command_lower.contains(&format!("| {}", forbidden))
                || command_lower.contains(&format!("; {}", forbidden))
                || command_lower.contains(&format!("&& {}", forbidden))
                || command_lower.contains(&format!("|| {}", forbidden))
            {
                return false;
            }
        }

        if command.contains('>') || command.contains(">>") {
            return false;
        }

        // Here-doc can be used for malicious input
        if command.contains("<<") {
            return false;
        }

        true
    }

    fn is_safe_git_command(&self, command: &str) -> bool {
        if !command.starts_with("git ") && !command.contains(" git ") && !command.contains(";git ")
            && !command.contains("&&git ") && !command.contains("||git ") && !command.contains("|git ") {
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
            "git stash",  // Blocks all stash operations except list/show (checked above)
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

        // Allow safe read-only git commands:
        // git status, git log, git diff, git show, git branch (list only),
        // git remote -v (view only), git config --list, git ls-files, etc.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_commands() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        assert!(executor.is_safe_command("ls -la"));
        assert!(executor.is_safe_command("cat file.txt"));
        assert!(executor.is_safe_command("grep pattern file.txt"));
        assert!(executor.is_safe_command("cargo test"));
        assert!(executor.is_safe_command("cargo build"));
        assert!(executor.is_safe_command("echo hello"));
        assert!(executor.is_safe_command("pwd"));
    }

    #[test]
    fn test_unsafe_commands() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        assert!(!executor.is_safe_command("sudo ls"));
        assert!(!executor.is_safe_command("rm file.txt"));
        assert!(!executor.is_safe_command("mv file1 file2"));
        assert!(!executor.is_safe_command("chmod 777 file"));
        assert!(!executor.is_safe_command("echo hello > file.txt"));
        assert!(!executor.is_safe_command("cat file.txt >> output.txt"));
        assert!(!executor.is_safe_command("ls | rm file.txt"));
        assert!(!executor.is_safe_command("ls && rm file.txt"));
    }

    #[test]
    fn test_path_traversal_blocked() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        assert!(!executor.is_safe_command("cat ../file.txt"));
        assert!(!executor.is_safe_command("ls ../../etc"));
        assert!(!executor.is_safe_command("cat ../../../etc/passwd"));
        assert!(!executor.is_safe_command("cat file.txt && ls .."));
        assert!(!executor.is_safe_command("ls | cat ../secret"));
    }

    #[test]
    fn test_absolute_paths_blocked() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        assert!(!executor.is_safe_command("/bin/ls"));
        assert!(!executor.is_safe_command("/etc/passwd"));
        assert!(!executor.is_safe_command("cat /etc/passwd"));
        assert!(!executor.is_safe_command("ls /tmp"));
        assert!(!executor.is_safe_command("cat /home/user/secret"));
        assert!(!executor.is_safe_command("ls && cat /etc/passwd"));
        assert!(!executor.is_safe_command("echo test || cat /etc/passwd"));
        assert!(!executor.is_safe_command("ls | grep /etc/passwd"));
        assert!(!executor.is_safe_command("true;/bin/bash"));
    }

    #[test]
    fn test_directory_change_blocked() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        assert!(!executor.is_safe_command("cd /tmp"));
        assert!(!executor.is_safe_command("cd .."));
        assert!(!executor.is_safe_command("cd / && ls"));
        assert!(!executor.is_safe_command("ls && cd /tmp"));
        assert!(!executor.is_safe_command("ls | cd /tmp"));
        assert!(!executor.is_safe_command("pushd /tmp"));
        assert!(!executor.is_safe_command("popd"));
        assert!(!executor.is_safe_command("ls && pushd .."));
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
    fn test_safe_git_commands() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        // Safe read-only git commands
        assert!(executor.is_safe_command("git status"));
        assert!(executor.is_safe_command("git log"));
        assert!(executor.is_safe_command("git log --oneline"));
        assert!(executor.is_safe_command("git diff"));
        assert!(executor.is_safe_command("git diff HEAD~1"));
        assert!(executor.is_safe_command("git show"));
        assert!(executor.is_safe_command("git show HEAD"));
        assert!(executor.is_safe_command("git branch"));
        assert!(executor.is_safe_command("git branch -v"));
        assert!(executor.is_safe_command("git branch --list"));
        assert!(executor.is_safe_command("git remote -v"));
        assert!(executor.is_safe_command("git config --list"));
        assert!(executor.is_safe_command("git ls-files"));
        assert!(executor.is_safe_command("git ls-tree HEAD"));
        assert!(executor.is_safe_command("git blame file.txt"));
        assert!(executor.is_safe_command("git grep pattern"));
        assert!(executor.is_safe_command("git rev-parse HEAD"));
        assert!(executor.is_safe_command("git describe --tags"));
        assert!(executor.is_safe_command("git stash list"));
        assert!(executor.is_safe_command("git stash show"));
        assert!(executor.is_safe_command("git stash show stash@{0}"));
    }

    #[test]
    fn test_dangerous_git_commands() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        // Remote operations (data leakage risk)
        assert!(!executor.is_safe_command("git push"));
        assert!(!executor.is_safe_command("git push origin main"));
        assert!(!executor.is_safe_command("git push --force"));
        assert!(!executor.is_safe_command("git pull"));
        assert!(!executor.is_safe_command("git pull origin main"));
        assert!(!executor.is_safe_command("git fetch"));
        assert!(!executor.is_safe_command("git fetch origin"));
        assert!(!executor.is_safe_command("git clone https://example.com/repo.git"));

        // Destructive local operations
        assert!(!executor.is_safe_command("git clean -fd"));
        assert!(!executor.is_safe_command("git reset --hard"));
        assert!(!executor.is_safe_command("git reset --hard HEAD~1"));
        assert!(!executor.is_safe_command("git checkout -f"));
        assert!(!executor.is_safe_command("git checkout -b newbranch"));
        assert!(!executor.is_safe_command("git branch -D branch-name"));
        assert!(!executor.is_safe_command("git branch -d branch-name"));
        assert!(!executor.is_safe_command("git filter-branch"));
        
        // Modifications
        assert!(!executor.is_safe_command("git add ."));
        assert!(!executor.is_safe_command("git add file.txt"));
        assert!(!executor.is_safe_command("git commit -m 'message'"));
        assert!(!executor.is_safe_command("git commit --amend"));
        assert!(!executor.is_safe_command("git rm file.txt"));
        assert!(!executor.is_safe_command("git mv old.txt new.txt"));
        assert!(!executor.is_safe_command("git merge branch"));
        assert!(!executor.is_safe_command("git rebase main"));
        assert!(!executor.is_safe_command("git cherry-pick abc123"));
        assert!(!executor.is_safe_command("git revert abc123"));
        assert!(!executor.is_safe_command("git restore file.txt"));
        assert!(!executor.is_safe_command("git switch main"));

        // Remote configuration changes
        assert!(!executor.is_safe_command("git remote add origin https://evil.com/repo.git"));
        assert!(!executor.is_safe_command("git remote set-url origin https://evil.com/repo.git"));
        assert!(!executor.is_safe_command("git remote remove origin"));

        // Submodules (can fetch from remote)
        assert!(!executor.is_safe_command("git submodule update"));
        assert!(!executor.is_safe_command("git submodule init"));

        // Stash operations (modify state)
        assert!(!executor.is_safe_command("git stash"));
        assert!(!executor.is_safe_command("git stash pop"));
        assert!(!executor.is_safe_command("git stash apply"));
        assert!(!executor.is_safe_command("git stash drop"));
        assert!(!executor.is_safe_command("git stash clear"));

        // Init (creates repository)
        assert!(!executor.is_safe_command("git init"));
        assert!(!executor.is_safe_command("git init new-repo"));
    }

    #[test]
    fn test_git_commands_in_chains() {
        let executor = BashExecutor::new(PathBuf::from(".")).unwrap();

        // Safe commands in chains
        assert!(executor.is_safe_command("git status && git log"));
        assert!(executor.is_safe_command("git diff | grep pattern"));
        assert!(executor.is_safe_command("echo test; git status"));

        // Dangerous commands in chains
        assert!(!executor.is_safe_command("git status && git push"));
        assert!(!executor.is_safe_command("git log | git commit -m 'test'"));
        assert!(!executor.is_safe_command("echo test; git add ."));
        assert!(!executor.is_safe_command("git status || git pull"));
    }
}
