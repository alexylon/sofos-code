use crate::error::{Result, SofosError};
use crate::tools::utils::confirm_action;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

const LOCAL_CONFIG_FILE: &str = ".sofos/config.local.toml";

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
    read_allow_set: GlobSet,
    read_deny_set: GlobSet,
}

impl PermissionManager {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        let settings_path = workspace.join(LOCAL_CONFIG_FILE);
        let settings = Self::load_settings(&settings_path)?;
        let (read_allow_set, read_deny_set) = Self::build_read_globs(&settings)?;

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
            read_allow_set,
            read_deny_set,
        })
    }

    fn build_read_globs(settings: &PermissionSettings) -> Result<(GlobSet, GlobSet)> {
        let mut allow_builder = GlobSetBuilder::new();
        let mut deny_builder = GlobSetBuilder::new();

        let add_patterns = |builder: &mut GlobSetBuilder, entries: &[String]| -> Result<()> {
            for entry in entries {
                if let Some(pattern) = Self::extract_read_pattern(entry) {
                    let expanded_pattern = Self::expand_tilde(pattern);
                    let glob = Glob::new(&expanded_pattern).map_err(|e| {
                        SofosError::ToolExecution(format!(
                            "Invalid Read glob pattern '{}': {}",
                            pattern, e
                        ))
                    })?;
                    builder.add(glob);
                }
            }
            Ok(())
        };

        add_patterns(&mut allow_builder, &settings.permissions.allow)?;
        add_patterns(&mut deny_builder, &settings.permissions.deny)?;

        let allow = allow_builder.build().map_err(|e| {
            SofosError::ToolExecution(format!("Failed to build allow glob set: {}", e))
        })?;
        let deny = deny_builder.build().map_err(|e| {
            SofosError::ToolExecution(format!("Failed to build deny glob set: {}", e))
        })?;

        Ok((allow, deny))
    }

    fn load_settings(path: &PathBuf) -> Result<PermissionSettings> {
        if path.exists() {
            let content = fs::read_to_string(path).map_err(|e| {
                SofosError::ToolExecution(format!("Failed to read config file: {}", e))
            })?;

            let settings: PermissionSettings = toml::from_str(&content).map_err(|e| {
                SofosError::ToolExecution(format!("Failed to parse config file: {}", e))
            })?;

            Ok(settings)
        } else {
            Ok(PermissionSettings::default())
        }
    }

    fn extract_read_pattern(entry: &str) -> Option<&str> {
        let trimmed = entry.trim();
        if let Some(rest) = trimmed.strip_prefix("Read(") {
            if let Some(end) = rest.rfind(')') {
                return Some(&rest[..end]);
            }
        }
        None
    }

    fn save_settings(&self) -> Result<()> {
        if let Some(parent) = self.settings_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                SofosError::ToolExecution(format!("Failed to create config directory: {}", e))
            })?;
        }

        let content = toml::to_string_pretty(&self.settings)
            .map_err(|e| SofosError::ToolExecution(format!("Failed to serialize config: {}", e)))?;

        fs::write(&self.settings_path, content).map_err(|e| {
            SofosError::ToolExecution(format!("Failed to write config file: {}", e))
        })?;

        Ok(())
    }

    fn normalize_command(command: &str) -> String {
        format!("Bash({})", command.trim())
    }

    fn normalize_read(path: &str) -> String {
        format!("Read({})", path.trim())
    }

    fn extract_base_command(command: &str) -> &str {
        let command = command.trim();

        let without_prefix = if let Some(stripped) = command.strip_prefix("Bash(") {
            if let Some(end) = stripped.find(')') {
                &stripped[..end]
            } else {
                stripped
            }
        } else {
            command
        };

        without_prefix
            .split_whitespace()
            .next()
            .unwrap_or(without_prefix)
    }

    fn expand_tilde(path: &str) -> String {
        if let Some(rest) = path.strip_prefix("~/") {
            if let Ok(home) = std::env::var("HOME") {
                return format!("{}/{}", home, rest);
            }
        } else if path == "~" {
            if let Ok(home) = std::env::var("HOME") {
                return home;
            }
        }
        path.to_string()
    }

    pub fn expand_tilde_pub(path: &str) -> String {
        Self::expand_tilde(path)
    }

    pub fn check_read_permission(&self, path: &str) -> CommandPermission {
        let expanded = Self::expand_tilde(path);
        let trimmed = expanded.trim();
        let stripped = trimmed.strip_prefix("./").unwrap_or(trimmed);
        let with_prefix = if stripped.starts_with("./") {
            stripped.to_string()
        } else {
            format!("./{}", stripped)
        };

        self.check_read_permission_with_candidates(trimmed, stripped, Some(with_prefix))
    }

    pub fn check_read_permission_both_forms(
        &self,
        original: &str,
        canonical: &str,
    ) -> CommandPermission {
        let result = self.check_read_permission(original);
        if result == CommandPermission::Denied || result == CommandPermission::Ask {
            return result;
        }
        if result == CommandPermission::Allowed {
            if self.is_read_explicit_allow(original) {
                return CommandPermission::Allowed;
            }
        }

        let canonical_result = self.check_read_permission(canonical);
        if canonical_result == CommandPermission::Denied
            || canonical_result == CommandPermission::Ask
        {
            return canonical_result;
        }

        CommandPermission::Allowed
    }

    /// Returns true only if path is explicitly in allow list (not default allow)
    pub fn is_read_explicit_allow(&self, path: &str) -> bool {
        let expanded = Self::expand_tilde(path);
        let trimmed = expanded.trim();
        let stripped = trimmed.strip_prefix("./").unwrap_or(trimmed);
        let with_prefix = if stripped.starts_with("./") {
            stripped.to_string()
        } else {
            format!("./{}", stripped)
        };

        let candidates = [trimmed, stripped, with_prefix.as_str()];

        for candidate in candidates.iter() {
            let normalized = Self::normalize_read(candidate);
            if self.settings.permissions.allow.contains(&normalized) {
                return true;
            }
            if !self.read_allow_set.is_empty() && self.read_allow_set.is_match(candidate) {
                return true;
            }
        }

        false
    }

    pub fn is_read_explicit_allow_both_forms(&self, original: &str, canonical: &str) -> bool {
        self.is_read_explicit_allow(original) || self.is_read_explicit_allow(canonical)
    }

    fn check_read_permission_with_candidates(
        &self,
        trimmed: &str,
        stripped: &str,
        prefixed: Option<String>,
    ) -> CommandPermission {
        let mut candidates: Vec<&str> = vec![trimmed, stripped];
        if let Some(pref) = prefixed.as_ref() {
            candidates.push(pref);
        }

        for candidate in candidates.iter() {
            let normalized = Self::normalize_read(candidate);
            if self.settings.permissions.allow.contains(&normalized) {
                return CommandPermission::Allowed;
            }
            if self.settings.permissions.deny.contains(&normalized) {
                return CommandPermission::Denied;
            }
        }

        for candidate in candidates.iter() {
            if !self.read_allow_set.is_empty() && self.read_allow_set.is_match(candidate) {
                return CommandPermission::Allowed;
            }
            if !self.read_deny_set.is_empty() && self.read_deny_set.is_match(candidate) {
                return CommandPermission::Denied;
            }
        }

        CommandPermission::Allowed
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

        Ok(CommandPermission::Ask)
    }

    pub fn ask_user_permission(&mut self, command: &str) -> Result<bool> {
        let normalized = Self::normalize_command(command);

        let prompt = format!("Allow command `{}`?", command);

        let confirmed = confirm_action(&prompt)?;
        let remember = confirm_action("Remember this decision?")?;

        if remember {
            if confirmed {
                self.settings.permissions.allow.push(normalized);
            } else {
                self.settings.permissions.deny.push(normalized);
            }
            self.save_settings()?;
            // Rebuild glob sets in case Read entries were dynamically added
            let (allow_set, deny_set) = Self::build_read_globs(&self.settings)?;
            self.read_allow_set = allow_set;
            self.read_deny_set = deny_set;
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
    fn test_read_exact_and_wildcard_matching() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Read(./allowed.txt)".to_string());
        settings
            .permissions
            .deny
            .push("Read(./secrets/*)".to_string());
        settings.permissions.deny.push("Read(./.env.*)".to_string());

        let (allow_set, deny_set) = PermissionManager::build_read_globs(&settings).unwrap();

        let manager = PermissionManager {
            settings,
            settings_path: temp_dir.path().join(".sofos/config.local.toml"),
            allowed_commands: HashSet::new(),
            forbidden_commands: HashSet::new(),
            read_allow_set: allow_set,
            read_deny_set: deny_set,
        };

        assert_eq!(
            manager.check_read_permission("./allowed.txt"),
            CommandPermission::Allowed
        );
        assert_eq!(
            manager.check_read_permission("./secrets/creds.json"),
            CommandPermission::Denied
        );
        assert_eq!(
            manager.check_read_permission("./.env.local"),
            CommandPermission::Denied
        );
        assert_eq!(
            manager.check_read_permission("./other.txt"),
            CommandPermission::Allowed
        );
    }

    #[test]
    fn test_read_allow_overrides_wildcard_deny() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Read(./secrets/allowed.txt)".to_string());
        settings
            .permissions
            .deny
            .push("Read(./secrets/*)".to_string());

        let (allow_set, deny_set) = PermissionManager::build_read_globs(&settings).unwrap();

        let manager = PermissionManager {
            settings,
            settings_path: temp_dir.path().join(".sofos/config.local.toml"),
            allowed_commands: HashSet::new(),
            forbidden_commands: HashSet::new(),
            read_allow_set: allow_set,
            read_deny_set: deny_set,
        };

        // Exact allow should win over wildcard deny
        assert_eq!(
            manager.check_read_permission("./secrets/allowed.txt"),
            CommandPermission::Allowed
        );
    }

    #[test]
    fn test_is_read_explicit_allow_detects_allow_glob() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Read(/outside/**)".to_string());

        let (allow_set, deny_set) = PermissionManager::build_read_globs(&settings).unwrap();

        let manager = PermissionManager {
            settings,
            settings_path: temp_dir.path().join(".sofos/config.local.toml"),
            allowed_commands: HashSet::new(),
            forbidden_commands: HashSet::new(),
            read_allow_set: allow_set,
            read_deny_set: deny_set,
        };

        assert!(manager.is_read_explicit_allow("/outside/secret.txt"));
        assert!(!manager.is_read_explicit_allow("/other/secret.txt"));
    }

    #[test]
    fn test_read_prefix_variants_match_globs() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .deny
            .push("Read(./test/**)".to_string());

        let (allow_set, deny_set) = PermissionManager::build_read_globs(&settings).unwrap();

        let manager = PermissionManager {
            settings,
            settings_path: temp_dir.path().join(".sofos/config.local.toml"),
            allowed_commands: HashSet::new(),
            forbidden_commands: HashSet::new(),
            read_allow_set: allow_set,
            read_deny_set: deny_set,
        };

        // Path without ./ should still be denied
        assert_eq!(
            manager.check_read_permission("test/file.txt"),
            CommandPermission::Denied
        );
        // Path with ./ should also be denied
        assert_eq!(
            manager.check_read_permission("./test/inner/file.txt"),
            CommandPermission::Denied
        );
    }

    #[test]
    fn test_tilde_expansion() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();

        if let Some(home) = std::env::var_os("HOME") {
            let home_path = PathBuf::from(home);
            let zshrc_path = format!("{}/.zshrc", home_path.display());
            settings
                .permissions
                .allow
                .push(format!("Read({})", zshrc_path));
        }

        let (allow_set, deny_set) = PermissionManager::build_read_globs(&settings).unwrap();

        let manager = PermissionManager {
            settings,
            settings_path: temp_dir.path().join(".sofos/config.local.toml"),
            allowed_commands: HashSet::new(),
            forbidden_commands: HashSet::new(),
            read_allow_set: allow_set,
            read_deny_set: deny_set,
        };

        // Tilde should expand and match
        if std::env::var_os("HOME").is_some() {
            assert_eq!(
                manager.check_read_permission("~/.zshrc"),
                CommandPermission::Allowed
            );
        }
    }

    #[test]
    fn test_tilde_in_glob_patterns() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();

        if let Some(home) = std::env::var_os("HOME") {
            let home_path = PathBuf::from(home);
            settings
                .permissions
                .allow
                .push(format!("Read({}/.config/**)", home_path.display()));
        }

        let (allow_set, deny_set) = PermissionManager::build_read_globs(&settings).unwrap();

        let manager = PermissionManager {
            settings,
            settings_path: temp_dir.path().join(".sofos/config.local.toml"),
            allowed_commands: HashSet::new(),
            forbidden_commands: HashSet::new(),
            read_allow_set: allow_set,
            read_deny_set: deny_set,
        };

        if std::env::var_os("HOME").is_some() {
            assert_eq!(
                manager.check_read_permission("~/.config/sofos/test.toml"),
                CommandPermission::Allowed
            );
        }
    }

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
            manager
                .check_command_permission("custom_script.sh")
                .unwrap(),
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
            manager
                .settings
                .permissions
                .allow
                .push("Bash(custom:*)".to_string());
            manager.save_settings().unwrap();
        }

        // Load again and verify
        let manager = PermissionManager::new(workspace).unwrap();
        assert!(manager
            .settings
            .permissions
            .allow
            .contains(&"Bash(custom:*)".to_string()));
    }

    #[test]
    fn test_wildcard_matching() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().to_path_buf();
        let mut manager = PermissionManager::new(workspace).unwrap();

        // Add wildcard permission
        manager
            .settings
            .permissions
            .allow
            .push("Bash(custom:*)".to_string());

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
        manager
            .settings
            .permissions
            .allow
            .push("Bash(exact command)".to_string());

        // Exact match should be allowed
        assert_eq!(
            manager.check_command_permission("exact command").unwrap(),
            CommandPermission::Allowed
        );
    }

    #[test]
    fn test_flexible_toml_format() {
        // Test that various TOML formatting styles work correctly
        let toml_content = r#"
[permissions]
allow = [
  "Bash(custom_command_1)", 
  "Bash(custom_command_2:*)",
]
deny = ["Bash(dangerous_command)"]
ask = []
"#;

        let settings: PermissionSettings =
            toml::from_str(toml_content).expect("Failed to parse flexible TOML format");

        assert_eq!(settings.permissions.allow.len(), 2);
        assert_eq!(settings.permissions.allow[0], "Bash(custom_command_1)");
        assert_eq!(settings.permissions.allow[1], "Bash(custom_command_2:*)");
        assert_eq!(settings.permissions.deny.len(), 1);
        assert_eq!(settings.permissions.deny[0], "Bash(dangerous_command)");
        assert_eq!(settings.permissions.ask.len(), 0);

        // Test inline format too
        let inline_toml = r#"
[permissions]
allow = ["Bash(cmd1)", "Bash(cmd2)"]
deny = ["Bash(bad)"]
ask = []
"#;

        let inline_settings: PermissionSettings =
            toml::from_str(inline_toml).expect("Failed to parse inline TOML format");

        assert_eq!(inline_settings.permissions.allow.len(), 2);
        assert_eq!(inline_settings.permissions.deny.len(), 1);
    }

    #[test]
    fn test_tilde_expansion_in_permissions() {
        let _temp_dir = TempDir::new().unwrap();

        // Set HOME for this test
        std::env::set_var("HOME", "/home/testuser");

        let expanded = PermissionManager::expand_tilde_pub("~/file.txt");
        assert_eq!(expanded, "/home/testuser/file.txt");

        let expanded_dir = PermissionManager::expand_tilde_pub("~");
        assert_eq!(expanded_dir, "/home/testuser");

        let not_tilde = PermissionManager::expand_tilde_pub("./file.txt");
        assert_eq!(not_tilde, "./file.txt");
    }

    #[test]
    fn test_tilde_in_allow_rules() {
        let temp_dir = TempDir::new().unwrap();
        std::env::set_var("HOME", "/home/testuser");

        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Read(~/.zshrc)".to_string());

        let (allow_set, deny_set) = PermissionManager::build_read_globs(&settings).unwrap();

        let manager = PermissionManager {
            settings,
            settings_path: temp_dir.path().join(".sofos/config.local.toml"),
            allowed_commands: HashSet::new(),
            forbidden_commands: HashSet::new(),
            read_allow_set: allow_set,
            read_deny_set: deny_set,
        };

        assert!(manager.is_read_explicit_allow("~/.zshrc"));
        assert!(manager.is_read_explicit_allow("/home/testuser/.zshrc"));
    }

    #[test]
    fn test_glob_patterns_recursive() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .deny
            .push("Read(./secrets/**)".to_string());

        let (allow_set, deny_set) = PermissionManager::build_read_globs(&settings).unwrap();

        let manager = PermissionManager {
            settings,
            settings_path: temp_dir.path().join(".sofos/config.local.toml"),
            allowed_commands: HashSet::new(),
            forbidden_commands: HashSet::new(),
            read_allow_set: allow_set,
            read_deny_set: deny_set,
        };

        assert_eq!(
            manager.check_read_permission("./secrets/file.txt"),
            CommandPermission::Denied
        );
        assert_eq!(
            manager.check_read_permission("./secrets/nested/deep/file.txt"),
            CommandPermission::Denied
        );
    }

    #[test]
    fn test_exact_allow_overrides_glob_deny() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Read(./secrets/exception.txt)".to_string());
        settings
            .permissions
            .deny
            .push("Read(./secrets/**)".to_string());

        let (allow_set, deny_set) = PermissionManager::build_read_globs(&settings).unwrap();

        let manager = PermissionManager {
            settings,
            settings_path: temp_dir.path().join(".sofos/config.local.toml"),
            allowed_commands: HashSet::new(),
            forbidden_commands: HashSet::new(),
            read_allow_set: allow_set,
            read_deny_set: deny_set,
        };

        assert_eq!(
            manager.check_read_permission("./secrets/exception.txt"),
            CommandPermission::Allowed
        );
        assert_eq!(
            manager.check_read_permission("./secrets/blocked.txt"),
            CommandPermission::Denied
        );
    }
}
