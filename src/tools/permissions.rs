use crate::error::{Result, SofosError};
use crate::tools::utils::{ConfirmationType, confirm_multi_choice};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

const LOCAL_CONFIG_FILE: &str = ".sofos/config.local.toml";
const GLOBAL_CONFIG_FILE: &str = ".sofos/config.toml";

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

impl PermissionSettings {
    /// Merge two permission settings, with other taking precedence for conflicts
    fn merge(&mut self, other: Self) {
        let mut seen = HashSet::new();

        for entry in &other.permissions.allow {
            seen.insert(entry.clone());
        }
        for entry in &other.permissions.deny {
            seen.insert(entry.clone());
        }
        for entry in &other.permissions.ask {
            seen.insert(entry.clone());
        }

        let mut merged_allow = other.permissions.allow.clone();
        for entry in &self.permissions.allow {
            if !seen.contains(entry) {
                merged_allow.push(entry.clone());
            }
        }

        let mut merged_deny = other.permissions.deny.clone();
        for entry in &self.permissions.deny {
            if !seen.contains(entry) {
                merged_deny.push(entry.clone());
            }
        }

        let mut merged_ask = other.permissions.ask.clone();
        for entry in &self.permissions.ask {
            if !seen.contains(entry) {
                merged_ask.push(entry.clone());
            }
        }

        self.permissions.allow = merged_allow;
        self.permissions.deny = merged_deny;
        self.permissions.ask = merged_ask;
    }
}

pub struct PermissionManager {
    settings: PermissionSettings,
    local_settings_path: PathBuf,
    #[allow(dead_code)]
    global_settings_path: Option<PathBuf>,
    allowed_commands: HashSet<String>,
    forbidden_commands: HashSet<String>,
    read_allow_set: GlobSet,
    read_deny_set: GlobSet,
    write_allow_set: GlobSet,
    write_deny_set: GlobSet,
    bash_path_allow_set: GlobSet,
    bash_path_deny_set: GlobSet,
    global_rules: HashSet<String>,
}

impl PermissionManager {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        let local_settings_path = workspace.join(LOCAL_CONFIG_FILE);

        let global_settings_path =
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(GLOBAL_CONFIG_FILE));

        let mut settings = if let Some(ref global_path) = global_settings_path {
            Self::load_settings(global_path)?
        } else {
            PermissionSettings::default()
        };

        let mut global_rules = HashSet::new();
        for entry in &settings.permissions.allow {
            global_rules.insert(entry.clone());
        }
        for entry in &settings.permissions.deny {
            global_rules.insert(entry.clone());
        }
        for entry in &settings.permissions.ask {
            global_rules.insert(entry.clone());
        }

        let local_settings = Self::load_settings(&local_settings_path)?;
        settings.merge(local_settings);

        let (read_allow_set, read_deny_set) =
            Self::build_scope_globs(&settings, Self::extract_read_pattern)?;
        let (write_allow_set, write_deny_set) =
            Self::build_scope_globs(&settings, Self::extract_write_pattern)?;
        let (bash_path_allow_set, bash_path_deny_set) =
            Self::build_scope_globs(&settings, Self::extract_bash_path_pattern)?;

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
            // File deletion/modification. `cp`, `mv`, `mkdir` are NOT on
            // this list so the model can move files around for recovery
            // and scaffolding. The source-path read check still applies
            // via `enforce_read_permissions`; destination writes go
            // unchecked, which is the conscious tradeoff for letting
            // the model repair its own mistakes without interrupting
            // the turn. `rm` / `rmdir` stay blocked because losing a
            // file to an overzealous `rm` is strictly worse than a
            // botched edit.
            "rm",
            "rmdir",
            "touch",
            "ln",
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
            local_settings_path,
            global_settings_path,
            allowed_commands,
            forbidden_commands,
            read_allow_set,
            read_deny_set,
            write_allow_set,
            write_deny_set,
            bash_path_allow_set,
            bash_path_deny_set,
            global_rules,
        })
    }

    pub fn get_rule_source(&self, rule: &str) -> String {
        if self.global_rules.contains(rule) {
            "~/.sofos/config.toml or .sofos/config.local.toml".to_string()
        } else {
            ".sofos/config.local.toml".to_string()
        }
    }

    fn build_scope_globs(
        settings: &PermissionSettings,
        extract_fn: fn(&str) -> Option<&str>,
    ) -> Result<(GlobSet, GlobSet)> {
        let mut allow_builder = GlobSetBuilder::new();
        let mut deny_builder = GlobSetBuilder::new();

        let add_patterns = |builder: &mut GlobSetBuilder, entries: &[String]| -> Result<()> {
            for entry in entries {
                if let Some(pattern) = extract_fn(entry) {
                    let expanded_pattern = Self::expand_tilde(pattern);
                    let glob = Glob::new(&expanded_pattern).map_err(|e| {
                        SofosError::ToolExecution(format!(
                            "Invalid glob pattern '{}': {}",
                            pattern, e
                        ))
                    })?;
                    builder.add(glob);

                    // For patterns ending with /**, also allow the base directory itself.
                    // e.g. Read(/some/path/**) should also match /some/path for list_directory.
                    if expanded_pattern.ends_with("/**") {
                        let base = &expanded_pattern[..expanded_pattern.len() - 3];
                        let base_glob = Glob::new(base).map_err(|e| {
                            SofosError::ToolExecution(format!(
                                "Invalid glob base pattern '{}': {}",
                                base, e
                            ))
                        })?;
                        builder.add(base_glob);
                    }
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

    fn extract_write_pattern(entry: &str) -> Option<&str> {
        let trimmed = entry.trim();
        if let Some(rest) = trimmed.strip_prefix("Write(") {
            if let Some(end) = rest.rfind(')') {
                return Some(&rest[..end]);
            }
        }
        None
    }

    /// Extract path patterns from Bash() entries.
    /// Only treats entries as path grants if the content starts with / or ~ AND contains a glob char.
    /// Plain commands like Bash(npm test) are not path patterns.
    fn extract_bash_path_pattern(entry: &str) -> Option<&str> {
        let trimmed = entry.trim();
        if let Some(rest) = trimmed.strip_prefix("Bash(") {
            if let Some(end) = rest.rfind(')') {
                let content = &rest[..end];
                if (content.starts_with('/') || content.starts_with('~')) && content.contains('*') {
                    return Some(content);
                }
            }
        }
        None
    }

    fn save_settings(&self) -> Result<()> {
        if let Some(parent) = self.local_settings_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                SofosError::ToolExecution(format!("Failed to create config directory: {}", e))
            })?;
        }

        let content = toml::to_string_pretty(&self.settings)
            .map_err(|e| SofosError::ToolExecution(format!("Failed to serialize config: {}", e)))?;

        fs::write(&self.local_settings_path, content).map_err(|e| {
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

    fn normalize_write(path: &str) -> String {
        format!("Write({})", path.trim())
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

    /// Check read permission and return the matched rule if denied
    pub fn check_read_permission_with_source(
        &self,
        path: &str,
    ) -> (CommandPermission, Option<String>) {
        let result = self.check_read_permission(path);

        if result == CommandPermission::Denied {
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
                if self.settings.permissions.deny.contains(&normalized) {
                    return (result, Some(normalized));
                }
            }

            // Glob pattern match (can't identify specific pattern easily)
            (result, Some("Read(pattern)".to_string()))
        } else {
            (result, None)
        }
    }

    pub fn check_read_permission(&self, path: &str) -> CommandPermission {
        self.check_scope_permission(
            path,
            Self::normalize_read,
            &self.read_allow_set,
            &self.read_deny_set,
        )
    }

    #[allow(dead_code)]
    pub fn check_write_permission(&self, path: &str) -> CommandPermission {
        self.check_scope_permission(
            path,
            Self::normalize_write,
            &self.write_allow_set,
            &self.write_deny_set,
        )
    }

    /// Returns true only if path is explicitly in allow list (not default allow)
    pub fn is_read_explicit_allow(&self, path: &str) -> bool {
        self.is_scope_explicit_allow(path, Self::normalize_read, &self.read_allow_set)
    }

    #[allow(dead_code)]
    pub fn is_read_explicit_allow_both_forms(&self, original: &str, canonical: &str) -> bool {
        self.is_read_explicit_allow(original) || self.is_read_explicit_allow(canonical)
    }

    pub fn is_write_explicit_allow(&self, path: &str) -> bool {
        self.is_scope_explicit_allow(path, Self::normalize_write, &self.write_allow_set)
    }

    #[allow(dead_code)]
    pub fn is_write_explicit_allow_both_forms(&self, original: &str, canonical: &str) -> bool {
        self.is_write_explicit_allow(original) || self.is_write_explicit_allow(canonical)
    }

    /// Check if a path is covered by a Bash(/path/**) grant
    pub fn is_bash_path_allowed(&self, path: &str) -> bool {
        self.is_scope_explicit_allow(path, Self::normalize_command, &self.bash_path_allow_set)
    }

    /// Check if a path is blocked by a Bash(/path/**) deny rule
    pub fn is_bash_path_denied(&self, path: &str) -> bool {
        let expanded = Self::expand_tilde(path);
        let trimmed = expanded.trim();
        let stripped = trimmed.strip_prefix("./").unwrap_or(trimmed);

        // Check exact deny entries
        let candidates = [trimmed, stripped];
        for candidate in &candidates {
            let normalized = Self::normalize_command(candidate);
            if self.settings.permissions.deny.contains(&normalized) {
                return true;
            }
        }

        // Check glob deny
        for candidate in &candidates {
            if !self.bash_path_deny_set.is_empty() && self.bash_path_deny_set.is_match(*candidate) {
                return true;
            }
        }

        false
    }

    // --- Generic scope helpers ---

    fn check_scope_permission(
        &self,
        path: &str,
        normalize_fn: fn(&str) -> String,
        allow_set: &GlobSet,
        deny_set: &GlobSet,
    ) -> CommandPermission {
        let expanded = Self::expand_tilde(path);
        let trimmed = expanded.trim();
        let stripped = trimmed.strip_prefix("./").unwrap_or(trimmed);
        let with_prefix = if stripped.starts_with("./") {
            stripped.to_string()
        } else {
            format!("./{}", stripped)
        };

        let candidates = [trimmed, stripped, with_prefix.as_str()];

        // Exact deny takes highest priority
        for candidate in &candidates {
            let normalized = normalize_fn(candidate);
            if self.settings.permissions.deny.contains(&normalized) {
                return CommandPermission::Denied;
            }
        }

        // Exact allow next
        for candidate in &candidates {
            let normalized = normalize_fn(candidate);
            if self.settings.permissions.allow.contains(&normalized) {
                return CommandPermission::Allowed;
            }
        }

        // Glob deny before glob allow (deny is more specific/important)
        for candidate in &candidates {
            if !deny_set.is_empty() && deny_set.is_match(*candidate) {
                return CommandPermission::Denied;
            }
        }

        for candidate in &candidates {
            if !allow_set.is_empty() && allow_set.is_match(*candidate) {
                return CommandPermission::Allowed;
            }
        }

        CommandPermission::Allowed
    }

    fn is_scope_explicit_allow(
        &self,
        path: &str,
        normalize_fn: fn(&str) -> String,
        allow_set: &GlobSet,
    ) -> bool {
        let expanded = Self::expand_tilde(path);
        let trimmed = expanded.trim();
        let stripped = trimmed.strip_prefix("./").unwrap_or(trimmed);
        let with_prefix = if stripped.starts_with("./") {
            stripped.to_string()
        } else {
            format!("./{}", stripped)
        };

        let candidates = [trimmed, stripped, with_prefix.as_str()];

        for candidate in &candidates {
            let normalized = normalize_fn(candidate);
            if self.settings.permissions.allow.contains(&normalized) {
                return true;
            }
            if !allow_set.is_empty() && allow_set.is_match(*candidate) {
                return true;
            }
        }

        false
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

    pub fn ask_user_permission(&mut self, command: &str) -> Result<(bool, bool)> {
        let normalized = Self::normalize_command(command);
        let prompt = format!("Allow command `{}`?", command);
        let (confirmed, remember) = Self::ask_three_way(&prompt)?;

        if remember {
            if confirmed {
                self.settings.permissions.allow.push(normalized);
            } else {
                self.settings.permissions.deny.push(normalized);
            }
            self.save_settings()?;
            self.rebuild_all_globs()?;
        }

        Ok((confirmed, remember))
    }

    /// Ask user for path-scoped permission (Read, Write, or Bash access to a directory).
    /// `scope` is "Read", "Write", or "Bash". `dir` is the directory to grant access to.
    /// Returns (allowed, remembered).
    pub fn ask_user_path_permission(&mut self, scope: &str, dir: &str) -> Result<(bool, bool)> {
        let grant = format!("{}({}/**)", scope, dir);
        let prompt = format!("Allow {} access to `{}/**`?", scope.to_lowercase(), dir);
        let (confirmed, remember) = Self::ask_three_way(&prompt)?;

        if remember {
            if confirmed {
                self.settings.permissions.allow.push(grant);
            } else {
                self.settings.permissions.deny.push(grant);
            }
            self.save_settings()?;
            self.rebuild_all_globs()?;
        }

        Ok((confirmed, remember))
    }

    /// Ask the user a single permission question with four options — one
    /// modal — instead of two sequential Y/N prompts. Returns
    /// `(confirmed, remember)` so the two flags can be consumed the same
    /// way as before. The "and remember" variants persist the decision
    /// in the allow / deny lists for future commands.
    fn ask_three_way(prompt: &str) -> Result<(bool, bool)> {
        let choices = ["Yes", "Yes and remember", "No", "No and remember"];
        // Default ("No") is the safe option used when the user cancels.
        let idx = confirm_multi_choice(prompt, &choices, 2, ConfirmationType::Permission)?;
        Ok(match idx {
            0 => (true, false),
            1 => (true, true),
            2 => (false, false),
            _ => (false, true),
        })
    }

    fn rebuild_all_globs(&mut self) -> Result<()> {
        let (ra, rd) = Self::build_scope_globs(&self.settings, Self::extract_read_pattern)?;
        self.read_allow_set = ra;
        self.read_deny_set = rd;
        let (wa, wd) = Self::build_scope_globs(&self.settings, Self::extract_write_pattern)?;
        self.write_allow_set = wa;
        self.write_deny_set = wd;
        let (ba, bd) = Self::build_scope_globs(&self.settings, Self::extract_bash_path_pattern)?;
        self.bash_path_allow_set = ba;
        self.bash_path_deny_set = bd;
        Ok(())
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
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Tests that mutate HOME must not run in parallel
    static HOME_MUTEX: Mutex<()> = Mutex::new(());

    fn create_test_manager(settings: PermissionSettings, temp_dir: &TempDir) -> PermissionManager {
        let (read_allow, read_deny) = PermissionManager::build_scope_globs(
            &settings,
            PermissionManager::extract_read_pattern,
        )
        .unwrap();
        let (write_allow, write_deny) = PermissionManager::build_scope_globs(
            &settings,
            PermissionManager::extract_write_pattern,
        )
        .unwrap();
        let (bash_allow, bash_deny) = PermissionManager::build_scope_globs(
            &settings,
            PermissionManager::extract_bash_path_pattern,
        )
        .unwrap();

        PermissionManager {
            settings,
            local_settings_path: temp_dir.path().join(".sofos/config.local.toml"),
            global_settings_path: None,
            allowed_commands: HashSet::new(),
            forbidden_commands: HashSet::new(),
            read_allow_set: read_allow,
            read_deny_set: read_deny,
            write_allow_set: write_allow,
            write_deny_set: write_deny,
            bash_path_allow_set: bash_allow,
            bash_path_deny_set: bash_deny,
            global_rules: HashSet::new(),
        }
    }

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

        let manager = create_test_manager(settings, &temp_dir);

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

        let manager = create_test_manager(settings, &temp_dir);

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

        let manager = create_test_manager(settings, &temp_dir);

        assert!(manager.is_read_explicit_allow("/outside/secret.txt"));
        assert!(!manager.is_read_explicit_allow("/other/secret.txt"));
    }

    #[test]
    fn test_is_read_explicit_allow_glob_matches_base_directory() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Read(/outside/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        // /** should match the base directory itself (for list_directory)
        assert!(manager.is_read_explicit_allow("/outside"));
        // and still match children
        assert!(manager.is_read_explicit_allow("/outside/secret.txt"));
        assert!(manager.is_read_explicit_allow("/outside/sub/deep.txt"));
        // but not unrelated paths
        assert!(!manager.is_read_explicit_allow("/other"));
    }

    #[test]
    fn test_is_read_explicit_allow_absolute_path_glob() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Read(/Users/alex/test/images/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        assert!(manager.is_read_explicit_allow("/Users/alex/test/images/test.jpg"));
        assert!(manager.is_read_explicit_allow("/Users/alex/test/images/subdir/photo.png"));
        assert!(!manager.is_read_explicit_allow("/Users/alex/other/test.jpg"));
    }

    #[test]
    fn test_read_prefix_variants_match_globs() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .deny
            .push("Read(./test/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        assert_eq!(
            manager.check_read_permission("test/file.txt"),
            CommandPermission::Denied
        );
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

        let manager = create_test_manager(settings, &temp_dir);

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

        let manager = create_test_manager(settings, &temp_dir);

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
        // `mv` / `cp` / `mkdir` are intentionally NOT forbidden — they
        // fall through to `Ask` so the user is prompted, which lets the
        // model recover corrupted files without blanket permission.
        assert_eq!(
            manager.check_command_permission("mv file1 file2").unwrap(),
            CommandPermission::Ask
        );
        assert_eq!(
            manager.check_command_permission("cp file1 file2").unwrap(),
            CommandPermission::Ask
        );
        assert_eq!(
            manager.check_command_permission("mkdir subdir").unwrap(),
            CommandPermission::Ask
        );
    }

    #[test]
    fn test_unknown_commands() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

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
        assert!(
            manager
                .settings
                .permissions
                .allow
                .contains(&"Bash(custom:*)".to_string())
        );
    }

    #[test]
    fn test_wildcard_matching() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().to_path_buf();
        let mut manager = PermissionManager::new(workspace).unwrap();

        manager
            .settings
            .permissions
            .allow
            .push("Bash(custom:*)".to_string());

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

        manager
            .settings
            .permissions
            .allow
            .push("Bash(exact command)".to_string());

        assert_eq!(
            manager.check_command_permission("exact command").unwrap(),
            CommandPermission::Allowed
        );
    }

    #[test]
    fn test_flexible_toml_format() {
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
        let _lock = HOME_MUTEX.lock().unwrap();
        let _temp_dir = TempDir::new().unwrap();

        let original_home = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/testuser");

        let expanded = PermissionManager::expand_tilde_pub("~/file.txt");
        let expanded_dir = PermissionManager::expand_tilde_pub("~");
        let not_tilde = PermissionManager::expand_tilde_pub("./file.txt");

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(expanded, "/home/testuser/file.txt");
        assert_eq!(expanded_dir, "/home/testuser");
        assert_eq!(not_tilde, "./file.txt");
    }

    #[test]
    fn test_tilde_in_allow_rules() {
        let _lock = HOME_MUTEX.lock().unwrap();
        let temp_dir = TempDir::new().unwrap();

        let original_home = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/testuser");

        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Read(~/.zshrc)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        // HOME must still be /home/testuser for expand_tilde in is_read_explicit_allow
        let check_tilde = manager.is_read_explicit_allow("~/.zshrc");
        let check_abs = manager.is_read_explicit_allow("/home/testuser/.zshrc");

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(check_tilde);
        assert!(check_abs);
    }

    #[test]
    fn test_glob_patterns_recursive() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .deny
            .push("Read(./secrets/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

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

        let manager = create_test_manager(settings, &temp_dir);

        assert_eq!(
            manager.check_read_permission("./secrets/exception.txt"),
            CommandPermission::Allowed
        );
        assert_eq!(
            manager.check_read_permission("./secrets/blocked.txt"),
            CommandPermission::Denied
        );
    }

    #[test]
    fn test_settings_merge_local_overrides_global() {
        let mut global = PermissionSettings::default();
        global
            .permissions
            .allow
            .push("Bash(global_cmd)".to_string());
        global
            .permissions
            .allow
            .push("Bash(shared_cmd)".to_string());
        global
            .permissions
            .deny
            .push("Read(./global_secret)".to_string());

        let mut local = PermissionSettings::default();
        local.permissions.allow.push("Bash(local_cmd)".to_string());
        local.permissions.allow.push("Bash(shared_cmd)".to_string());
        local
            .permissions
            .deny
            .push("Read(./local_secret)".to_string());

        global.merge(local);

        assert_eq!(global.permissions.allow[0], "Bash(local_cmd)");
        assert_eq!(global.permissions.allow[1], "Bash(shared_cmd)");
        assert_eq!(global.permissions.allow[2], "Bash(global_cmd)");

        assert_eq!(global.permissions.deny.len(), 2);
        assert!(
            global
                .permissions
                .deny
                .contains(&"Read(./local_secret)".to_string())
        );
        assert!(
            global
                .permissions
                .deny
                .contains(&"Read(./global_secret)".to_string())
        );
    }

    #[test]
    fn test_settings_merge_handles_empty_configs() {
        let mut global = PermissionSettings::default();
        global
            .permissions
            .allow
            .push("Bash(global_cmd)".to_string());

        let local = PermissionSettings::default();
        global.merge(local);

        assert_eq!(global.permissions.allow.len(), 1);
        assert_eq!(global.permissions.allow[0], "Bash(global_cmd)");
    }

    #[test]
    fn test_global_config_supplements_local() {
        let _lock = HOME_MUTEX.lock().unwrap();
        use std::fs;
        let temp_dir = TempDir::new().unwrap();

        let home_dir = temp_dir.path().join("home");
        fs::create_dir_all(home_dir.join(".sofos")).unwrap();
        fs::write(
            home_dir.join(".sofos/config.toml"),
            r#"[permissions]
allow = ["Bash(global_allowed)"]
deny = ["Read(./global_denied)"]
ask = []
"#,
        )
        .unwrap();

        let workspace = temp_dir.path().join("workspace");
        fs::create_dir_all(workspace.join(".sofos")).unwrap();
        fs::write(
            workspace.join(".sofos/config.local.toml"),
            r#"[permissions]
allow = ["Bash(local_allowed)"]
deny = []
ask = []
"#,
        )
        .unwrap();

        let original_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home_dir);

        let manager = PermissionManager::new(workspace.clone()).unwrap();

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(
            manager
                .settings
                .permissions
                .allow
                .contains(&"Bash(local_allowed)".to_string())
        );
        assert!(
            manager
                .settings
                .permissions
                .allow
                .contains(&"Bash(global_allowed)".to_string())
        );
        assert!(
            manager
                .settings
                .permissions
                .deny
                .contains(&"Read(./global_denied)".to_string())
        );
    }

    #[test]
    fn test_rule_source_detection() {
        let _lock = HOME_MUTEX.lock().unwrap();
        use std::fs;
        let temp_dir = TempDir::new().unwrap();

        let home_dir = temp_dir.path().join("home");
        fs::create_dir_all(home_dir.join(".sofos")).unwrap();
        fs::write(
            home_dir.join(".sofos/config.toml"),
            r#"[permissions]
allow = []
deny = ["Read(./global_denied)"]
ask = []
"#,
        )
        .unwrap();

        let workspace = temp_dir.path().join("workspace");
        fs::create_dir_all(workspace.join(".sofos")).unwrap();
        fs::write(
            workspace.join(".sofos/config.local.toml"),
            r#"[permissions]
allow = []
deny = ["Read(./local_denied)"]
ask = []
"#,
        )
        .unwrap();

        let original_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home_dir);

        let manager = PermissionManager::new(workspace.clone()).unwrap();

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        let global_rule_source = manager.get_rule_source("Read(./global_denied)");
        assert!(global_rule_source.contains("~/.sofos/config.toml"));

        let local_rule_source = manager.get_rule_source("Read(./local_denied)");
        assert_eq!(local_rule_source, ".sofos/config.local.toml");
    }

    // --- Write scope tests ---

    #[test]
    fn test_write_explicit_allow_glob() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Write(/tmp/output/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        assert!(manager.is_write_explicit_allow("/tmp/output/file.txt"));
        assert!(manager.is_write_explicit_allow("/tmp/output/sub/deep.txt"));
        // Base directory itself
        assert!(manager.is_write_explicit_allow("/tmp/output"));
        // Not allowed outside the grant
        assert!(!manager.is_write_explicit_allow("/tmp/other/file.txt"));
        assert!(!manager.is_write_explicit_allow("/etc/passwd"));
    }

    #[test]
    fn test_write_scope_independent_of_read() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        // Only Read grant, no Write
        settings
            .permissions
            .allow
            .push("Read(/data/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        assert!(manager.is_read_explicit_allow("/data/file.txt"));
        assert!(!manager.is_write_explicit_allow("/data/file.txt"));
    }

    // --- Bash path scope tests ---

    #[test]
    fn test_bash_path_allowed_with_glob() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Bash(/var/log/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        assert!(manager.is_bash_path_allowed("/var/log/syslog"));
        assert!(manager.is_bash_path_allowed("/var/log/nginx/access.log"));
        // Base directory
        assert!(manager.is_bash_path_allowed("/var/log"));
        // Not allowed
        assert!(!manager.is_bash_path_allowed("/var/other/file"));
        assert!(!manager.is_bash_path_allowed("/etc/passwd"));
    }

    #[test]
    fn test_bash_path_pattern_requires_glob_char() {
        // Bash(/tmp/) without * should NOT be treated as a path pattern
        assert!(PermissionManager::extract_bash_path_pattern("Bash(/tmp/**)").is_some());
        assert!(PermissionManager::extract_bash_path_pattern("Bash(~/docs/*)").is_some());
        // No glob char — treated as command, not path
        assert!(PermissionManager::extract_bash_path_pattern("Bash(/tmp/)").is_none());
        assert!(PermissionManager::extract_bash_path_pattern("Bash(/usr/bin/ls)").is_none());
        // Not a path (no / or ~)
        assert!(PermissionManager::extract_bash_path_pattern("Bash(npm test)").is_none());
        assert!(PermissionManager::extract_bash_path_pattern("Bash(cargo:*)").is_none());
    }

    #[test]
    fn test_bash_path_independent_of_read_and_write() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Read(/data/**)".to_string());
        settings
            .permissions
            .allow
            .push("Write(/data/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        // Read and Write allowed, but Bash path is NOT
        assert!(manager.is_read_explicit_allow("/data/file.txt"));
        assert!(manager.is_write_explicit_allow("/data/file.txt"));
        assert!(!manager.is_bash_path_allowed("/data/file.txt"));
    }

    #[test]
    fn test_extract_write_pattern() {
        assert_eq!(
            PermissionManager::extract_write_pattern("Write(/tmp/**)"),
            Some("/tmp/**")
        );
        assert_eq!(
            PermissionManager::extract_write_pattern("Write(~/docs/file.txt)"),
            Some("~/docs/file.txt")
        );
        // Not a Write pattern
        assert!(PermissionManager::extract_write_pattern("Read(/tmp/**)").is_none());
        assert!(PermissionManager::extract_write_pattern("Bash(ls)").is_none());
    }

    #[test]
    fn test_glob_deny_overrides_glob_allow() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Read(/data/**)".to_string());
        settings
            .permissions
            .deny
            .push("Read(/data/secret/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        // Allowed by broad glob
        assert_eq!(
            manager.check_read_permission("/data/public/file.txt"),
            CommandPermission::Allowed
        );
        // Denied by narrower deny glob even though broad allow also matches
        assert_eq!(
            manager.check_read_permission("/data/secret/passwords.txt"),
            CommandPermission::Denied
        );
    }

    #[test]
    fn test_write_glob_deny_overrides_glob_allow() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Write(/tmp/**)".to_string());
        settings
            .permissions
            .deny
            .push("Write(/tmp/protected/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        assert_eq!(
            manager.check_write_permission("/tmp/safe/file.txt"),
            CommandPermission::Allowed
        );
        assert_eq!(
            manager.check_write_permission("/tmp/protected/file.txt"),
            CommandPermission::Denied
        );
    }

    #[test]
    fn test_bash_path_deny_overrides_allow() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Bash(/data/**)".to_string());
        settings
            .permissions
            .deny
            .push("Bash(/data/secret/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        assert!(manager.is_bash_path_allowed("/data/public/file.txt"));
        assert!(manager.is_bash_path_denied("/data/secret/file.txt"));
        // Also verify that allowed still reports true for the broad pattern
        // (is_bash_path_allowed doesn't check deny — that's done separately in the handler)
        assert!(manager.is_bash_path_allowed("/data/secret/file.txt"));
    }

    #[test]
    fn test_exact_allow_still_overrides_glob_deny() {
        // Critical: if you specifically allow a file, it should override a glob deny
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .allow
            .push("Read(/data/secret/exception.txt)".to_string());
        settings
            .permissions
            .deny
            .push("Read(/data/secret/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        // Exact allow beats glob deny
        assert_eq!(
            manager.check_read_permission("/data/secret/exception.txt"),
            CommandPermission::Allowed
        );
        // But other files under secret are still denied
        assert_eq!(
            manager.check_read_permission("/data/secret/other.txt"),
            CommandPermission::Denied
        );
    }
}
