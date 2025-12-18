use crate::error::{Result, SofosError};
use crate::tools::utils::confirm_action;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

const SETTINGS_FILE: &str = ".sofos/settings.local.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionSettings {
    pub permissions: Permissions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Permissions {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
}

impl Default for PermissionSettings {
    fn default() -> Self {
        Self {
            permissions: Permissions {
                allow: Vec::new(),
                deny: Vec::new(),
                ask: Vec::new(),
            },
        }
    }
}

pub struct PermissionManager {
    settings: PermissionSettings,
    settings_path: PathBuf,
    allowed_commands: HashSet<String>,
    forbidden_commands: HashSet<String>,
}

impl PermissionManager {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        let settings_path = workspace.join(SETTINGS_FILE);
        let settings = Self::load_settings(&settings_path)?;

        let allowed_commands = [
            // Build tools
            "cargo",
            "rustc",
            "npm",
            "yarn",
            "pnpm",
            "node",
            "python",
            "python3",
            "pip",
            "go",
            "make",
            "cmake",
            "gcc",
            "g++",
            "javac",
            "java",
            "mvn",
            "gradle",
            // Read-only file operations
            "ls",
            "cat",
            "head",
            "tail",
            "less",
            "more",
            "grep",
            "egrep",
            "fgrep",
            "rg",
            "ag",
            "ack",
            "find",
            "file",
            "stat",
            "wc",
            "diff",
            "cmp",
            // System info (read-only)
            "pwd",
            "whoami",
            "date",
            "hostname",
            "uname",
            "arch",
            "env",
            "printenv",
            "echo",
            "printf",
            "which",
            "whereis",
            "type",
            // Safe git commands (read-only)
            "git",
            // Process info (read-only)
            "ps",
            "top",
            "htop",
            // Compression/archiving (read-only extraction)
            "tar",
            "gzip",
            "gunzip",
            "bzip2",
            "bunzip2",
            "unzip",
            "xz",
            // Text processing
            "sed",
            "awk",
            "cut",
            "sort",
            "uniq",
            "tr",
            "expand",
            "unexpand",
            "column",
            "paste",
            "join",
            // Other safe commands
            "test",
            "true",
            "false",
            "seq",
            "timeout",
            "time",
            "basename",
            "dirname",
            "realpath",
            "readlink",
            "hexdump",
            "od",
            "strings",
            "base64",
            "sha256sum",
            "sha512sum",
            "md5sum",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let forbidden_commands = [
            // File deletion/modification
            "rm",
            "rmdir",
            "mv",
            "cp",
            "touch",
            "ln",
            "mkdir",
            // Permissions
            "chmod",
            "chown",
            "chgrp",
            // Disk operations
            "dd",
            "mkfs",
            "fdisk",
            "parted",
            "mkswap",
            "swapon",
            "swapoff",
            "mount",
            "umount",
            // System control
            "shutdown",
            "reboot",
            "halt",
            "poweroff",
            "systemctl",
            "service",
            // User management
            "useradd",
            "userdel",
            "usermod",
            "groupadd",
            "groupdel",
            "passwd",
            // Process control
            "kill",
            "killall",
            "pkill",
            // Privilege escalation
            "sudo",
            "su",
            // Directory navigation (breaks sandbox)
            "cd",
            "pushd",
            "popd",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        Ok(Self {
            settings,
            settings_path,
            allowed_commands,
            forbidden_commands,
        })
    }

    fn load_settings(path: &PathBuf) -> Result<PermissionSettings> {
        if path.exists() {
            let content = fs::read_to_string(path).map_err(|e| {
                SofosError::ToolExecution(format!("Failed to read settings file: {}", e))
            })?;

            serde_json::from_str(&content).map_err(|e| {
                SofosError::ToolExecution(format!("Failed to parse settings file: {}", e))
            })
        } else {
            Ok(PermissionSettings::default())
        }
    }

    fn save_settings(&self) -> Result<()> {
        // Ensure directory exists
        if let Some(parent) = self.settings_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                SofosError::ToolExecution(format!("Failed to create settings directory: {}", e))
            })?;
        }

        let content = serde_json::to_string_pretty(&self.settings).map_err(|e| {
            SofosError::ToolExecution(format!("Failed to serialize settings: {}", e))
        })?;

        fs::write(&self.settings_path, content).map_err(|e| {
            SofosError::ToolExecution(format!("Failed to write settings file: {}", e))
        })?;

        Ok(())
    }

    fn normalize_command(command: &str) -> String {
        format!("Bash({})", command.trim())
    }

    fn extract_base_command(command: &str) -> &str {
        let command = command.trim();
        
        // Handle common prefixes
        let without_prefix = if let Some(stripped) = command.strip_prefix("Bash(") {
            if let Some(end) = stripped.find(')') {
                &stripped[..end]
            } else {
                stripped
            }
        } else {
            command
        };

        // Get first word (the actual command)
        without_prefix
            .split_whitespace()
            .next()
            .unwrap_or(without_prefix)
    }

    pub fn check_command_permission(&mut self, command: &str) -> Result<CommandPermission> {
        let normalized = Self::normalize_command(command);
        let base_command = Self::extract_base_command(command);

        if self.settings.permissions.allow.contains(&normalized) {
            return Ok(CommandPermission::Allowed);
        }

        if self.settings.permissions.deny.contains(&normalized) {
            return Ok(CommandPermission::Denied);
        }

        // Check wildcard matches (e.g., "Bash(cargo:*)")
        let wildcard_pattern = format!("Bash({}:*)", base_command);
        if self.settings.permissions.allow.contains(&wildcard_pattern) {
            return Ok(CommandPermission::Allowed);
        }

        if self.settings.permissions.deny.contains(&wildcard_pattern) {
            return Ok(CommandPermission::Denied);
        }

        if self.allowed_commands.contains(base_command) {
            return Ok(CommandPermission::Allowed);
        }

        if self.forbidden_commands.contains(base_command) {
            return Ok(CommandPermission::Denied);
        }

        // Unknown command - ask user
        Ok(CommandPermission::Ask)
    }

    pub fn ask_user_permission(&mut self, command: &str) -> Result<bool> {
        let normalized = Self::normalize_command(command);

        let prompt = format!(
            "Allow command `{}`?",
            command
        );

        let confirmed = confirm_action(&prompt)?;
        let remember = confirm_action("Remember this decision?")?;

        if remember {
            if confirmed {
                self.settings.permissions.allow.push(normalized);
            } else {
                self.settings.permissions.deny.push(normalized);
            }
            self.save_settings()?;
        } else {
            self.settings.permissions.ask.push(normalized);
        }

        Ok(confirmed)
    }
}

#[derive(Debug, PartialEq)]
pub enum CommandPermission {
    Allowed,
    Denied,
    Ask,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_allowed_commands() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

        // Test predefined allowed commands
        assert_eq!(
            manager.check_command_permission("cargo build").unwrap(),
            CommandPermission::Allowed
        );
        assert_eq!(
            manager.check_command_permission("cargo test").unwrap(),
            CommandPermission::Allowed
        );
        assert_eq!(
            manager.check_command_permission("ls -la").unwrap(),
            CommandPermission::Allowed
        );
        assert_eq!(
            manager.check_command_permission("cat file.txt").unwrap(),
            CommandPermission::Allowed
        );
        assert_eq!(
            manager.check_command_permission("git status").unwrap(),
            CommandPermission::Allowed
        );
        assert_eq!(
            manager.check_command_permission("npm test").unwrap(),
            CommandPermission::Allowed
        );
    }

    #[test]
    fn test_forbidden_commands() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

        // Test predefined forbidden commands
        assert_eq!(
            manager.check_command_permission("rm -rf /").unwrap(),
            CommandPermission::Denied
        );
        assert_eq!(
            manager.check_command_permission("sudo ls").unwrap(),
            CommandPermission::Denied
        );
        assert_eq!(
            manager.check_command_permission("chmod 777 file").unwrap(),
            CommandPermission::Denied
        );
        assert_eq!(
            manager.check_command_permission("mv file1 file2").unwrap(),
            CommandPermission::Denied
        );
    }

    #[test]
    fn test_unknown_commands() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

        // Test unknown commands that need user confirmation
        assert_eq!(
            manager.check_command_permission("custom_script.sh").unwrap(),
            CommandPermission::Ask
        );
        assert_eq!(
            manager.check_command_permission("unknown_tool").unwrap(),
            CommandPermission::Ask
        );
    }

    #[test]
    fn test_settings_persistence() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().to_path_buf();

        {
            let mut manager = PermissionManager::new(workspace.clone()).unwrap();
            manager.settings.permissions.allow.push("Bash(custom:*)".to_string());
            manager.save_settings().unwrap();
        }

        // Load again and verify
        let manager = PermissionManager::new(workspace).unwrap();
        assert!(manager.settings.permissions.allow.contains(&"Bash(custom:*)".to_string()));
    }

    #[test]
    fn test_wildcard_matching() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().to_path_buf();
        let mut manager = PermissionManager::new(workspace).unwrap();

        // Add wildcard permission
        manager.settings.permissions.allow.push("Bash(custom:*)".to_string());

        // Should match wildcard
        assert_eq!(
            manager.check_command_permission("custom anything").unwrap(),
            CommandPermission::Allowed
        );
    }

    #[test]
    fn test_exact_match_priority() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().to_path_buf();
        let mut manager = PermissionManager::new(workspace).unwrap();

        // Add exact permission
        manager.settings.permissions.allow.push("Bash(exact command)".to_string());

        // Exact match should be allowed
        assert_eq!(
            manager.check_command_permission("exact command").unwrap(),
            CommandPermission::Allowed
        );
    }
}
