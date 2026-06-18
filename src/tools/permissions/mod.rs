pub mod command_parse;
pub mod manager;
pub mod pattern;
pub mod scope;
pub mod settings;

pub use manager::PermissionManager;

use crate::error::{Result, SofosError};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, PartialEq, Eq)]
pub enum CommandPermission {
    Allowed,
    Denied,
    Ask,
}

/// Directory to offer as a `<scope>(<dir>/**)` grant for an external
/// path: the path's parent, so sibling files under the same directory
/// share one grant — but never the filesystem root. A top-level path
/// like `/work` (from `docker -w /work`) has parent `/`, and
/// `<scope>(//**)` would grant the whole machine, so such a path is
/// scoped to itself (`<scope>(/work/**)`). Shared by the bash, read,
/// write, and image external-path gates so all four scope grants the
/// same way.
pub(crate) fn grant_dir_for_path(path: &Path) -> &str {
    let parent = path.parent().and_then(|p| p.to_str()).unwrap_or("");
    if parent.is_empty() || Path::new(parent).parent().is_none() {
        path.to_str().unwrap_or("")
    } else {
        parent
    }
}

/// Gate access to a path that is outside the workspace, sharing one
/// allow / deny set across every caller (filesystem read, filesystem
/// write, bash path arguments, image loading) so a permission granted
/// once in a session covers every other tool that names the same
/// directory. The flow is: session-allowed wins, session-denied
/// rejects, otherwise prompt the user when interactive or surface a
/// config hint when not.
pub fn check_external_path_session_access(
    workspace: &Path,
    scope: &str,
    canonical_path: &str,
    dir_to_grant: &str,
    interactive: bool,
    session_allowed: &Arc<Mutex<HashSet<String>>>,
    session_denied: &Arc<Mutex<HashSet<String>>>,
) -> Result<()> {
    let canonical = Path::new(canonical_path);

    if let Ok(allowed_dirs) = session_allowed.lock() {
        for dir in allowed_dirs.iter() {
            if canonical.starts_with(Path::new(dir)) {
                return Ok(());
            }
        }
    }

    if let Ok(denied_dirs) = session_denied.lock() {
        for dir in denied_dirs.iter() {
            if canonical.starts_with(Path::new(dir)) {
                return Err(SofosError::ToolExecution(format!(
                    "{} access denied for '{}' (denied earlier this session)",
                    scope, canonical_path
                )));
            }
        }
    }

    if !interactive {
        return Err(SofosError::ToolExecution(format!(
            "Path '{}' is outside workspace and not explicitly allowed\n\
             Hint: Add {}({}/**) to 'allow' list in .sofos/config.local.toml",
            canonical_path, scope, dir_to_grant
        )));
    }

    let mut pm = PermissionManager::new(workspace.to_path_buf())?;
    let (allowed, remember) = pm.ask_user_path_permission(scope, dir_to_grant)?;

    if allowed {
        if !remember {
            if let Ok(mut dirs) = session_allowed.lock() {
                dirs.insert(dir_to_grant.to_string());
            }
        }
        Ok(())
    } else {
        if !remember {
            if let Ok(mut dirs) = session_denied.lock() {
                dirs.insert(dir_to_grant.to_string());
            }
        }
        Err(SofosError::ToolExecution(format!(
            "{} access denied by user for '{}'",
            scope, canonical_path
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::permissions::command_parse::is_env_assignment;
    use crate::tools::permissions::settings::PermissionSettings;
    use std::collections::HashSet;
    use std::path::PathBuf;
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
    fn read_glob_single_star_does_not_cross_separator() {
        // Regression: a `Read(./secrets/*)` deny rule used to match
        // files at every depth under `secrets/` because the globset
        // default lets `*` swallow `/`. With `literal_separator(true)`
        // it only matches direct children. Recursive matches still
        // work through `**`, which the second sub-test covers.
        //
        // We exercise this through a deny rule rather than an allow
        // rule because `check_read_permission` returns `Allowed` both
        // when an allow rule matches AND as the default when no rule
        // matches — the deny path is the one with a falsifiable
        // result either way.
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .deny
            .push("Read(./secrets/*)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        assert_eq!(
            manager.check_read_permission("./secrets/creds.json"),
            CommandPermission::Denied,
            "direct child of ./secrets/ must still match the single-star deny"
        );
        assert_eq!(
            manager.check_read_permission("./secrets/nested/creds.json"),
            CommandPermission::Allowed,
            "`*` must not cross `/`, so the nested file is NOT covered by the deny"
        );

        // The recursive form keeps the historical broad behaviour for
        // anyone who actually wants every-depth coverage.
        let mut recursive = PermissionSettings::default();
        recursive
            .permissions
            .deny
            .push("Read(./secrets/**)".to_string());
        let recursive_manager = create_test_manager(recursive, &temp_dir);
        assert_eq!(
            recursive_manager.check_read_permission("./secrets/nested/creds.json"),
            CommandPermission::Denied,
            "`**` must still walk every depth under ./secrets/"
        );
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

    /// `FOO=bar rm -rf /` must classify as `rm`, not as the never-seen
    /// `FOO=bar` command. Regression for the env-prefix permission
    /// bypass flagged in the 2026-04 audit.
    #[test]
    fn env_prefix_does_not_bypass_forbidden_base() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

        assert_eq!(
            manager
                .check_command_permission("FOO=bar rm -rf /")
                .unwrap(),
            CommandPermission::Denied
        );
        assert_eq!(
            manager
                .check_command_permission("A=1 B=2 C=3 sudo ls")
                .unwrap(),
            CommandPermission::Denied
        );
        // A token that looks like `KEY=value` but has a leading digit
        // isn't a valid shell env name — treat it as the base command
        // so we don't accidentally skip it and classify the next token
        // as the command.
        assert_eq!(
            PermissionManager::extract_base_command("1BAD=x rm"),
            "1BAD=x"
        );
    }

    #[test]
    fn is_env_assignment_matches_posix_names_only() {
        assert!(is_env_assignment("FOO=bar"));
        assert!(is_env_assignment("_FOO=bar"));
        assert!(is_env_assignment("FOO_1=bar"));
        assert!(is_env_assignment("FOO="));
        assert!(!is_env_assignment("1FOO=bar")); // leading digit
        assert!(!is_env_assignment("FOO-X=bar")); // hyphen
        assert!(!is_env_assignment("FOO"));
        assert!(!is_env_assignment("=bar"));
        assert!(!is_env_assignment(""));
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

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn test_tilde_expansion_trims_leading_separator_in_remainder() {
        // `PathBuf::push` replaces self when the pushed argument is
        // absolute. If the caller types `~//foo` (double slash after
        // the tilde), the remainder after `~/` starts with `/`, which
        // `push` would treat as absolute and escape the home
        // directory entirely — `~//foo` would resolve to `/foo`,
        // which is not what bash would do. The trim keeps the
        // expansion rooted at the home directory.
        let _lock = HOME_MUTEX.lock().unwrap();
        let _temp_dir = TempDir::new().unwrap();

        let original_home = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/testuser");

        let single = PermissionManager::expand_tilde_pub("~/foo");
        let double = PermissionManager::expand_tilde_pub("~//foo");
        let triple = PermissionManager::expand_tilde_pub("~///foo");

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(single, "/home/testuser/foo");
        assert_eq!(
            double, "/home/testuser/foo",
            "double-slash after tilde must not escape home"
        );
        assert_eq!(
            triple, "/home/testuser/foo",
            "any number of leading slashes must not escape home"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_tilde_expansion_uses_userprofile_on_windows() {
        // Regression: `expand_tilde` used to read `$HOME`, which isn't
        // the canonical home-directory env var on Windows. A user
        // typing `~/docs` got no expansion. The fix reads
        // `%USERPROFILE%` on Windows and joins via `PathBuf::push` so
        // the resulting separator is native (backslash on Windows).
        let _lock = HOME_MUTEX.lock().unwrap();
        let _temp_dir = TempDir::new().unwrap();

        let original = std::env::var_os("USERPROFILE");
        std::env::set_var("USERPROFILE", r"C:\Users\testuser");

        let expanded = PermissionManager::expand_tilde_pub("~/docs/file.txt");
        let expanded_dir = PermissionManager::expand_tilde_pub("~");

        match original {
            Some(home) => std::env::set_var("USERPROFILE", home),
            None => std::env::remove_var("USERPROFILE"),
        }

        assert_eq!(expanded, r"C:\Users\testuser\docs\file.txt");
        assert_eq!(expanded_dir, r"C:\Users\testuser");
    }

    #[cfg(unix)]
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
            manager.check_write_permission("/tmp/readonly/file.txt"),
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

    #[test]
    fn test_volatile_line_args_sed_range() {
        // The user's original example — sed with a numeric address range.
        assert!(PermissionManager::command_has_volatile_line_args(
            "nl -ba tests/foo.rs | sed -n '1270,1320p'"
        ));
        assert!(PermissionManager::command_has_volatile_line_args(
            "sed -n 10,20p file.txt"
        ));
        assert!(PermissionManager::command_has_volatile_line_args(
            "sed '5d' file.txt"
        ));
        // `$` is not numeric → not a line-number range.
        assert!(!PermissionManager::command_has_volatile_line_args(
            "sed \"1,$q\" file.txt"
        ));
    }

    #[test]
    fn test_volatile_line_args_head_tail() {
        assert!(PermissionManager::command_has_volatile_line_args(
            "head -n 50 big.log"
        ));
        assert!(PermissionManager::command_has_volatile_line_args(
            "head -50 big.log"
        ));
        assert!(PermissionManager::command_has_volatile_line_args(
            "tail -n 100 /var/log/syslog"
        ));
        assert!(PermissionManager::command_has_volatile_line_args(
            "tail +20 file.txt"
        ));
        // No numeric flag → not considered volatile.
        assert!(!PermissionManager::command_has_volatile_line_args(
            "head file.txt"
        ));
        assert!(!PermissionManager::command_has_volatile_line_args(
            "tail -f /var/log/syslog"
        ));
    }

    #[test]
    fn test_volatile_line_args_grep_context() {
        assert!(PermissionManager::command_has_volatile_line_args(
            "grep -A 3 pattern file.txt"
        ));
        assert!(PermissionManager::command_has_volatile_line_args(
            "grep -B5 pattern file.txt"
        ));
        assert!(PermissionManager::command_has_volatile_line_args(
            "rg -C 10 needle ."
        ));
        assert!(!PermissionManager::command_has_volatile_line_args(
            "grep -i pattern file.txt"
        ));
    }

    #[test]
    fn test_volatile_line_args_awk_nr() {
        assert!(PermissionManager::command_has_volatile_line_args(
            "awk 'NR==5' file.txt"
        ));
        assert!(PermissionManager::command_has_volatile_line_args(
            "awk 'NR<=10{print}' file.txt"
        ));
        assert!(!PermissionManager::command_has_volatile_line_args(
            "awk '/pattern/' file.txt"
        ));
        // A non-digit NR== earlier in the program must not shadow a
        // later numeric NR==. Regression for the `.find()` first-hit bug.
        assert!(PermissionManager::command_has_volatile_line_args(
            "awk 'NR==var; NR==5 {print}' file.txt"
        ));
    }

    #[test]
    fn test_volatile_line_args_covers_named_patterns() {
        // Explicit coverage for every pattern the user called out as
        // "should drop to Yes/No only": sed -n 'Np', sed -n 'N,Mp',
        // head -n N, tail -n N, grep -A/-B/-C N, awk 'NR==N'.
        let cases = [
            "sed -n '5p' file.txt",
            "sed -n '10,20p' file.txt",
            "head -n 50 big.log",
            "tail -n 100 /var/log/syslog",
            "grep -A 5 pattern file.txt",
            "grep -B 5 pattern file.txt",
            "grep -C 5 pattern file.txt",
            "awk 'NR==5' file.txt",
        ];
        for cmd in cases {
            assert!(
                PermissionManager::command_has_volatile_line_args(cmd),
                "expected `{cmd}` to be classified as volatile"
            );
        }
    }

    #[test]
    fn test_volatile_line_args_plain_commands() {
        // Normal commands with stable args should NOT be flagged.
        assert!(!PermissionManager::command_has_volatile_line_args(
            "cargo build --release"
        ));
        assert!(!PermissionManager::command_has_volatile_line_args(
            "git log --oneline"
        ));
        assert!(!PermissionManager::command_has_volatile_line_args(
            "ls -la src/"
        ));
    }

    /// One-shot command shapes (multi-line scripts, command / process
    /// substitution, heredocs) can never match a remembered rule again,
    /// so they must be flagged un-rememberable just like volatile args.
    /// These are exactly the strange entries that landed in the allow
    /// list: a `for ...; do ... awk ... done` block and a
    /// `python3 - <<'PY'` heredoc.
    #[test]
    fn test_command_not_rememberable() {
        assert!(PermissionManager::command_not_rememberable(
            "for file in $(find src -name '*.rs'); do echo \"$file\"; done"
        ));
        assert!(PermissionManager::command_not_rememberable(
            "python3 - <<'PY'\nprint('hi')\nPY"
        ));
        assert!(PermissionManager::command_not_rememberable("echo `whoami`"));
        assert!(PermissionManager::command_not_rememberable(
            "diff <(sort a) <(sort b)"
        ));
        // Volatile-arg commands stay covered through the combined gate.
        assert!(PermissionManager::command_not_rememberable(
            "sed -n '10,20p' file.txt"
        ));
        // A plain, stable single command is still rememberable.
        assert!(!PermissionManager::command_not_rememberable(
            "cargo build --release"
        ));
    }

    /// `Bash(<dir>/**)` grants must never be persisted for the filesystem
    /// root (`docker -w /work` → parent `/` → `Bash(//**)`, which grants
    /// the whole machine) or for `host:container` docker mounts
    /// (`-v /repo:/work:ro` → `Bash(/repo:/**)`).
    #[test]
    fn test_is_persistable_grant_dir() {
        assert!(!PermissionManager::is_persistable_grant_dir("/"));
        assert!(!PermissionManager::is_persistable_grant_dir(""));
        assert!(!PermissionManager::is_persistable_grant_dir(
            "/Users/alex/git/aal/sofos-code:"
        ));
        assert!(PermissionManager::is_persistable_grant_dir("/work"));
        assert!(PermissionManager::is_persistable_grant_dir(
            "/Users/alex/Pictures"
        ));
    }

    /// The shared grant-dir helper used by the bash, read, write, and
    /// image gates: a path's parent, except a top-level path is scoped to
    /// itself so it never expands to the filesystem root.
    #[test]
    fn test_grant_dir_for_path() {
        use std::path::Path;
        assert_eq!(grant_dir_for_path(Path::new("/a/b/c.txt")), "/a/b");
        assert_eq!(grant_dir_for_path(Path::new("/etc/hosts")), "/etc");
        // Top-level path: the parent is root, so scope to the path itself
        // rather than emit `//**`.
        assert_eq!(grant_dir_for_path(Path::new("/work")), "/work");
    }

    /// Granting the same directory twice should leave one rule, not pile
    /// up duplicate lines like the three `Bash(//**)` entries the bug
    /// produced.
    #[test]
    fn test_remember_rule_dedup() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

        manager.remember_rule("Bash(/work/**)".to_string(), true);
        manager.remember_rule("Bash(/work/**)".to_string(), true);

        let hits = manager
            .settings
            .permissions
            .allow
            .iter()
            .filter(|r| *r == "Bash(/work/**)")
            .count();
        assert_eq!(hits, 1);
    }

    /// Volatile sed/head/tail buried inside a `for ...; do ...; done`
    /// loop or behind `;` / `&&` should still be detected — the old
    /// `split('|')` pass missed everything that wasn't pipe-separated.
    #[test]
    fn test_volatile_line_args_inside_compound_shell() {
        assert!(PermissionManager::command_has_volatile_line_args(
            "for f in src/*.rs; do sed -n '1,320p' \"$f\" | nl -ba; done"
        ));
        assert!(PermissionManager::command_has_volatile_line_args(
            "cat README.md && head -n 50 CHANGELOG.md"
        ));
        assert!(PermissionManager::command_has_volatile_line_args(
            "echo start; tail -n 20 build.log"
        ));
        assert!(!PermissionManager::command_has_volatile_line_args(
            "for f in *.rs; do echo \"$f\"; cat \"$f\"; done"
        ));
    }

    /// `;` and `&&` inside quotes must NOT split a segment — `echo 'a; b'`
    /// is still a single `echo` command.
    #[test]
    fn test_split_compound_command_respects_quotes() {
        let segs = PermissionManager::split_compound_command("echo 'a; b' && ls");
        assert_eq!(segs, vec!["echo 'a; b'", "ls"]);

        let segs = PermissionManager::split_compound_command("echo \"x | y\" | wc -l");
        assert_eq!(segs, vec!["echo \"x | y\"", "wc -l"]);
    }

    /// Lone `&` is part of `2>&1`, not a separator. The pre-existing
    /// `2>&1` allowance in `is_safe_command_structure` would be undone
    /// if our splitter chopped commands at every `&`.
    #[test]
    fn test_split_compound_command_keeps_stderr_redirect() {
        let segs = PermissionManager::split_compound_command("cargo test 2>&1 | tee out.log");
        assert_eq!(segs, vec!["cargo test 2>&1", "tee out.log"]);
    }

    /// The user-reported regression: a `for ...; do echo; sed; nl; done`
    /// pipeline of read-only tools should resolve as Allowed without a
    /// prompt, not get stuck on the `for` keyword.
    #[test]
    fn compound_for_loop_of_allowed_commands_is_allowed() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

        assert_eq!(
            manager
                .check_command_permission(
                    "for f in src/recipient/*.rs src/recipient/native/*.rs; \
                     do echo '===== '\"$f\"' ====='; sed -n '1,320p' \"$f\" | nl -ba; done"
                )
                .unwrap(),
            CommandPermission::Allowed
        );
    }

    /// A forbidden base anywhere inside a compound shell wins over an
    /// allowed leader. Closes the `cat foo && rm bar` smuggling hole.
    #[test]
    fn compound_with_forbidden_base_is_denied() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

        assert_eq!(
            manager
                .check_command_permission("cat foo && rm bar")
                .unwrap(),
            CommandPermission::Denied
        );
        assert_eq!(
            manager
                .check_command_permission("for f in *.rs; do rm \"$f\"; done")
                .unwrap(),
            CommandPermission::Denied
        );
        assert_eq!(
            manager.check_command_permission("ls; sudo whoami").unwrap(),
            CommandPermission::Denied
        );
    }

    /// Compound shells that include an unknown tool stay at Ask — only
    /// fully-allowed pipelines auto-pass.
    #[test]
    fn compound_with_unknown_base_asks() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

        assert_eq!(
            manager
                .check_command_permission("cat foo && some_custom_tool bar")
                .unwrap(),
            CommandPermission::Ask
        );
        assert_eq!(
            manager
                .check_command_permission("for f in *; do unknown_tool \"$f\"; done")
                .unwrap(),
            CommandPermission::Ask
        );
    }

    /// `ls; # trailing comment` is shell-equivalent to plain `ls` —
    /// the `#` segment is commentary, not a command, and the verdict
    /// must not regress to Ask just because we now look past the head.
    #[test]
    fn trailing_shell_comment_does_not_force_ask() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

        assert_eq!(
            manager
                .check_command_permission("ls -la; # quick listing")
                .unwrap(),
            CommandPermission::Allowed
        );
    }

    /// Bare `"Bash"` in the allow list auto-passes any command whose
    /// every base is non-forbidden. Specific entries become moot; the
    /// blanket beats every other check.
    #[test]
    fn blanket_bash_allow_auto_allows_non_forbidden() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();
        manager.settings.permissions.allow.push("Bash".to_string());

        // Unknown tool: still Allowed under blanket Bash.
        assert_eq!(
            manager
                .check_command_permission("some_custom_tool --flag")
                .unwrap(),
            CommandPermission::Allowed
        );
        // Built-in allowed: still Allowed.
        assert_eq!(
            manager.check_command_permission("ls -la").unwrap(),
            CommandPermission::Allowed
        );
        // Compound with all-unknown bases: still Allowed.
        assert_eq!(
            manager.check_command_permission("foo && bar; baz").unwrap(),
            CommandPermission::Allowed
        );
    }

    /// Blanket `"Bash"` allow does NOT override built-in
    /// `forbidden_commands` — `rm`, `chmod`, `sudo`, … stay denied.
    #[test]
    fn blanket_bash_allow_still_blocks_forbidden() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();
        manager.settings.permissions.allow.push("Bash".to_string());

        assert_eq!(
            manager.check_command_permission("rm -rf /").unwrap(),
            CommandPermission::Denied
        );
        assert_eq!(
            manager.check_command_permission("sudo whoami").unwrap(),
            CommandPermission::Denied
        );
        // Forbidden buried in a compound is also rejected.
        assert_eq!(
            manager
                .check_command_permission("ls && rm tmp.txt")
                .unwrap(),
            CommandPermission::Denied
        );
    }

    /// Bare `"Bash"` in the deny list auto-rejects every bash command.
    #[test]
    fn blanket_bash_deny_rejects_everything() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();
        manager.settings.permissions.deny.push("Bash".to_string());

        assert_eq!(
            manager.check_command_permission("ls -la").unwrap(),
            CommandPermission::Denied
        );
        assert_eq!(
            manager.check_command_permission("cargo build").unwrap(),
            CommandPermission::Denied
        );
        assert_eq!(
            manager
                .check_command_permission("any_tool whatsoever")
                .unwrap(),
            CommandPermission::Denied
        );
    }

    /// Specific `Bash(curl --version)` entry alongside a blanket
    /// `"Bash"` deny is irrelevant — deny wins. Symmetrically, a
    /// blanket `"Bash"` allow wins over its companion specific entry.
    #[test]
    fn blanket_bash_beats_specific_companions() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();
        manager
            .settings
            .permissions
            .deny
            .push("Bash(curl --version)".to_string());
        manager.settings.permissions.deny.push("Bash".to_string());

        assert_eq!(
            manager.check_command_permission("ls").unwrap(),
            CommandPermission::Denied
        );
        assert_eq!(
            manager.check_command_permission("curl --version").unwrap(),
            CommandPermission::Denied
        );

        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();
        manager
            .settings
            .permissions
            .allow
            .push("Bash(curl --version)".to_string());
        manager.settings.permissions.allow.push("Bash".to_string());

        assert_eq!(
            manager
                .check_command_permission("custom_unknown_tool")
                .unwrap(),
            CommandPermission::Allowed
        );
    }

    /// Cross-list precedence: blanket `"Bash"` allow wins over a
    /// specific `Bash(curl --version)` *deny* entry — per the rule
    /// that the blanket beats every check except built-in forbidden.
    /// Mirror direction: a specific allow entry can't rescue a command
    /// when blanket deny is set.
    #[test]
    fn blanket_bash_allow_beats_specific_deny_across_lists() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();
        manager.settings.permissions.allow.push("Bash".to_string());
        manager
            .settings
            .permissions
            .deny
            .push("Bash(curl --version)".to_string());

        assert_eq!(
            manager.check_command_permission("curl --version").unwrap(),
            CommandPermission::Allowed
        );

        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();
        manager
            .settings
            .permissions
            .allow
            .push("Bash(curl --version)".to_string());
        manager.settings.permissions.deny.push("Bash".to_string());

        assert_eq!(
            manager.check_command_permission("curl --version").unwrap(),
            CommandPermission::Denied
        );
    }

    /// Both lists carrying the blanket entry: deny wins.
    #[test]
    fn blanket_bash_deny_beats_blanket_allow() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();
        manager.settings.permissions.allow.push("Bash".to_string());
        manager.settings.permissions.deny.push("Bash".to_string());

        assert_eq!(
            manager.check_command_permission("ls").unwrap(),
            CommandPermission::Denied
        );
    }

    /// `while CMD; do CMD; done` and `if CMD; then CMD; fi` should
    /// likewise be inspected sub-command by sub-command.
    #[test]
    fn while_and_if_compounds_are_inspected() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

        assert_eq!(
            manager
                .check_command_permission("if grep -q foo file.txt; then echo found; fi")
                .unwrap(),
            CommandPermission::Allowed
        );
        assert_eq!(
            manager
                .check_command_permission("if true; then rm file; fi")
                .unwrap(),
            CommandPermission::Denied
        );
    }

    /// Blanket `Bash` allow must NOT be defeated by quote/paren/path
    /// wrappers on the base name. The shell unwraps these before
    /// execution, so the forbidden-command lookup has to do the same.
    #[test]
    fn blanket_bash_allow_does_not_let_wrappers_smuggle_forbidden() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();
        manager.settings.permissions.allow.push("Bash".to_string());

        for cmd in [
            "(rm -rf /tmp/x)",
            "\\rm -rf /tmp/x",
            "'rm' -rf /tmp/x",
            "\"rm\" -rf /tmp/x",
            "/bin/rm /tmp/x",
            "/usr/bin/sudo whoami",
            "{ rm -rf /tmp/x; }",
        ] {
            assert_eq!(
                manager.check_command_permission(cmd).unwrap(),
                CommandPermission::Denied,
                "wrapper-prefixed forbidden command should be denied: {}",
                cmd
            );
        }
    }

    /// Lone `&` is a statement separator in bash. A forbidden base on
    /// either side of one must catch the command, just like `&&` or `;`.
    #[test]
    fn lone_ampersand_splits_segments() {
        let segs = PermissionManager::split_compound_command("ls foo & rm bar");
        assert_eq!(segs, vec!["ls foo", "rm bar"]);

        // No spaces around `&` either.
        let segs = PermissionManager::split_compound_command("ls&rm bar");
        assert_eq!(segs, vec!["ls", "rm bar"]);

        // `>&` stays glued (redirect operand).
        let segs = PermissionManager::split_compound_command("cmd 2>&1");
        assert_eq!(segs, vec!["cmd 2>&1"]);

        // Full check: `ls & rm bar` is denied because the second base is `rm`.
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();
        assert_eq!(
            manager.check_command_permission("ls foo & rm bar").unwrap(),
            CommandPermission::Denied
        );
    }

    /// `extract_base_command` returns the cleaned base, not the raw token.
    /// `(rm`, `'rm'`, `/bin/rm`, and similar shapes all reduce to `rm`.
    #[test]
    fn extract_base_command_strips_wrappers_and_path_prefix() {
        assert_eq!(PermissionManager::extract_base_command("(rm -rf /"), "rm");
        assert_eq!(PermissionManager::extract_base_command("'rm' x"), "rm");
        assert_eq!(PermissionManager::extract_base_command("\\rm x"), "rm");
        assert_eq!(PermissionManager::extract_base_command("/bin/rm x"), "rm");
        assert_eq!(PermissionManager::extract_base_command("./rm x"), "rm");
    }

    /// Compound shells where each segment leads with a wrapper still
    /// surface the real bases through `enumerate_compound_bases`.
    #[test]
    fn enumerate_compound_bases_strips_wrappers_in_each_segment() {
        let bases = PermissionManager::enumerate_compound_bases("ls && (rm bar)");
        assert_eq!(bases, vec!["ls".to_string(), "rm".to_string()]);

        let bases = PermissionManager::enumerate_compound_bases("'echo' hi; \\sudo whoami");
        assert_eq!(bases, vec!["echo".to_string(), "sudo".to_string()]);
    }

    /// `PATH=. cargo build` previously classified as `cargo` (in the
    /// allowed set), letting an attacker-controlled `./cargo` execute
    /// under the auto-allow. The same trick works with `LD_PRELOAD`,
    /// `DYLD_*`, `NODE_PATH`, `PYTHONPATH`. All must now route through
    /// the Ask prompt regardless of the base command.
    #[test]
    fn dangerous_env_prefix_forces_ask() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

        for cmd in [
            "PATH=. cargo build",
            "LD_PRELOAD=./evil.so ls",
            "LD_LIBRARY_PATH=. ./bin",
            "DYLD_INSERT_LIBRARIES=./evil.dylib ls",
            "DYLD_LIBRARY_PATH=. ls",
            "NODE_PATH=. node script.js",
            "PYTHONPATH=. python script.py",
        ] {
            assert_eq!(
                manager.check_command_permission(cmd).unwrap(),
                CommandPermission::Ask,
                "dangerous env prefix should force Ask: {cmd}"
            );
        }

        // Harmless env prefixes (PII-style, `FOO=bar`) still fall through
        // to the base-command check.
        assert_eq!(
            manager
                .check_command_permission("FOO=bar cargo build")
                .unwrap(),
            CommandPermission::Allowed
        );
    }

    /// Even a blanket `"Bash"` allow does NOT short-circuit the
    /// dangerous-env check — the user opted in to "trust this command
    /// family", not to "swap my PATH".
    #[test]
    fn dangerous_env_prefix_overrides_blanket_allow() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();
        manager.settings.permissions.allow.push("Bash".to_string());

        assert_eq!(
            manager
                .check_command_permission("PATH=. cargo build")
                .unwrap(),
            CommandPermission::Ask
        );
    }

    /// A dangerous env prefix in any segment of a compound shell trips
    /// the gate — `ls; PATH=. cargo` still routes to Ask.
    #[test]
    fn dangerous_env_prefix_in_compound_segment_caught() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = PermissionManager::new(temp_dir.path().to_path_buf()).unwrap();

        assert_eq!(
            manager
                .check_command_permission("ls; PATH=. cargo build")
                .unwrap(),
            CommandPermission::Ask
        );
        assert_eq!(
            manager
                .check_command_permission("cat foo && LD_PRELOAD=evil.so ls")
                .unwrap(),
            CommandPermission::Ask
        );
    }

    /// A `Bash(rm)` rule in local-allow must NOT silently strip the
    /// matching `Bash(rm)` from global-deny. Per-list seen sets keep the
    /// deny entry around (the runtime then arbitrates).
    #[test]
    fn merge_does_not_drop_global_deny_for_local_allow() {
        let mut global = PermissionSettings::default();
        global.permissions.deny.push("Bash(rm)".to_string());

        let mut local = PermissionSettings::default();
        local.permissions.allow.push("Bash(rm)".to_string());

        global.merge(local);

        assert!(
            global.permissions.deny.contains(&"Bash(rm)".to_string()),
            "global deny must survive a local-allow with the same key"
        );
        assert!(
            global.permissions.allow.contains(&"Bash(rm)".to_string()),
            "local allow is still merged in"
        );
    }

    /// Whitespace in the command must NOT bypass a session-scoped deny.
    /// `Bash(ls /etc)` and `Bash(ls   /etc)` must produce the same key.
    /// Path-shaped `normalize_command` keeps internal whitespace because
    /// filenames may legitimately contain it.
    #[test]
    fn normalize_command_key_collapses_internal_whitespace() {
        assert_eq!(
            PermissionManager::normalize_command_key("ls /etc"),
            PermissionManager::normalize_command_key("ls  /etc"),
        );
        assert_eq!(
            PermissionManager::normalize_command_key("ls\t/etc"),
            PermissionManager::normalize_command_key("ls /etc"),
        );
        assert_eq!(
            PermissionManager::normalize_command_key("  ls /etc  "),
            "Bash(ls /etc)".to_string()
        );

        // The path-shaped wrapper does NOT collapse — preserves
        // filenames that contain literal multi-whitespace.
        assert_eq!(
            PermissionManager::normalize_command("/tmp/file  with spaces"),
            "Bash(/tmp/file  with spaces)".to_string()
        );
    }

    /// `./secrets/../allowed.txt` lexically resolves to `./allowed.txt`,
    /// so the deny rule `Read(./secrets/**)` must NOT match it. The
    /// glob deny on `./secrets/**` must still catch the un-normalised
    /// input `./secrets/keys`.
    #[test]
    fn lexical_normalisation_keeps_deny_globs_honest() {
        let temp_dir = TempDir::new().unwrap();
        let mut settings = PermissionSettings::default();
        settings
            .permissions
            .deny
            .push("Read(./secrets/**)".to_string());

        let manager = create_test_manager(settings, &temp_dir);

        assert_eq!(
            manager.check_read_permission("./secrets/keys"),
            CommandPermission::Denied,
            "direct child must still match"
        );
        // The shell would resolve this to `./allowed.txt` — outside `./secrets`.
        assert_eq!(
            manager.check_read_permission("./secrets/../allowed.txt"),
            CommandPermission::Allowed,
            "lexically-normalised path that escapes the deny prefix is allowed"
        );
        // Noise variants of the deny path still match.
        assert_eq!(
            manager.check_read_permission(".//secrets/keys"),
            CommandPermission::Denied,
            "double-slash variant is still inside the deny scope"
        );
        assert_eq!(
            manager.check_read_permission("./secrets/./keys"),
            CommandPermission::Denied,
            "interior `.` segment is still inside the deny scope"
        );
    }
}
