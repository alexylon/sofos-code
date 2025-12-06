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
}
