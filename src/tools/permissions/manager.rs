use crate::error::{Result, SofosError};
use crate::tools::permissions::CommandPermission;
use crate::tools::permissions::pattern::BLANKET_BASH;
use crate::tools::permissions::settings::PermissionSettings;
use crate::tools::utils::{ConfirmationType, confirm_multi_choice};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

const LOCAL_CONFIG_FILE: &str = ".sofos/config.local.toml";
const GLOBAL_CONFIG_FILE: &str = ".sofos/config.toml";

pub struct PermissionManager {
    pub(super) settings: PermissionSettings,
    pub(super) local_settings_path: PathBuf,
    #[allow(dead_code)]
    pub(super) global_settings_path: Option<PathBuf>,
    pub(super) allowed_commands: HashSet<String>,
    pub(super) forbidden_commands: HashSet<String>,
    pub(super) read_allow_set: GlobSet,
    pub(super) read_deny_set: GlobSet,
    pub(super) write_allow_set: GlobSet,
    pub(super) write_deny_set: GlobSet,
    pub(super) bash_path_allow_set: GlobSet,
    pub(super) bash_path_deny_set: GlobSet,
    pub(super) global_rules: HashSet<String>,
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
            "nl",
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

    pub(super) fn build_scope_globs(
        settings: &PermissionSettings,
        extract_fn: fn(&str) -> Option<&str>,
    ) -> Result<(GlobSet, GlobSet)> {
        let mut allow_builder = GlobSetBuilder::new();
        let mut deny_builder = GlobSetBuilder::new();

        // Compile every permission pattern with `literal_separator(true)`
        // so that `*` does not cross `/`. A casual `Read(/etc/*.conf)`
        // rule used to broaden into every `*.conf` file under any depth
        // of `/etc`, because the globset default lets `*` swallow path
        // separators. Recursive matches still work through `**`.
        let compile_path_glob = |pattern: &str| -> Result<Glob> {
            globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .map_err(|e| {
                    SofosError::ToolExecution(format!("Invalid glob pattern '{}': {}", pattern, e))
                })
        };

        let add_patterns = |builder: &mut GlobSetBuilder, entries: &[String]| -> Result<()> {
            for entry in entries {
                if let Some(pattern) = extract_fn(entry) {
                    let expanded_pattern = Self::expand_tilde(pattern);
                    let glob = compile_path_glob(&expanded_pattern)?;
                    builder.add(glob);

                    // For patterns ending with /**, also allow the base directory itself.
                    // e.g. Read(/some/path/**) should also match /some/path for list_directory.
                    if expanded_pattern.ends_with("/**") {
                        let base = &expanded_pattern[..expanded_pattern.len() - 3];
                        let base_glob = compile_path_glob(base)?;
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

    pub(super) fn load_settings(path: &PathBuf) -> Result<PermissionSettings> {
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

    pub(super) fn save_settings(&self) -> Result<()> {
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

    /// Look up the user's home directory in a way that works on both
    /// Unix (`$HOME`) and Windows (`%USERPROFILE%`). `std::env::home_dir`
    /// was re-stabilised with a correct Windows implementation in Rust
    /// 1.85, but reading the platform-native env var directly keeps us
    /// compatible with older toolchains and makes the per-platform
    /// choice explicit. Returns `None` when the env var is unset, which
    /// is the same "fall through unexpanded" signal the caller uses.
    pub(super) fn home_dir() -> Option<PathBuf> {
        #[cfg(windows)]
        {
            std::env::var_os("USERPROFILE").map(PathBuf::from)
        }
        #[cfg(not(windows))]
        {
            std::env::var_os("HOME").map(PathBuf::from)
        }
    }

    /// Expand a leading `~` or `~/` to the user's home directory. Uses
    /// `PathBuf::push` so the separator between the home directory and
    /// the rest of the path is the platform's native one — the old
    /// `format!("{}/{}", home, rest)` produced `C:\Users\alice/foo` on
    /// Windows, which Windows accepts but looks wrong on inspection.
    /// Paths not starting with `~` are returned unchanged.
    ///
    /// Strips leading separators from the remainder before pushing
    /// because `PathBuf::push` *replaces* self when the argument is
    /// absolute — so `expand_tilde("~//foo")` without the trim would
    /// return `/foo` (escaped out of home) instead of the
    /// bash-semantic `~/foo` = `home/foo`. Matters more on Windows
    /// where a user-supplied `~/\\server\share\file` would be UNC-
    /// absolute and would likewise replace the home prefix.
    pub(super) fn expand_tilde(path: &str) -> String {
        if path == "~" {
            return Self::home_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string());
        }
        if let Some(rest) = path.strip_prefix("~/") {
            if let Some(mut home) = Self::home_dir() {
                let rest = rest.trim_start_matches(['/', '\\']);
                home.push(rest);
                return home.to_string_lossy().to_string();
            }
        }
        path.to_string()
    }

    pub fn expand_tilde_pub(path: &str) -> String {
        Self::expand_tilde(path)
    }

    pub fn check_command_permission(&mut self, command: &str) -> Result<CommandPermission> {
        // Blanket `"Bash"` rules trump every other check. Deny wins over
        // allow when both lists contain the blanket entry, matching the
        // existing "deny is strictest" pattern used elsewhere.
        let blanket_deny = self
            .settings
            .permissions
            .deny
            .iter()
            .any(|e| e == BLANKET_BASH);
        if blanket_deny {
            return Ok(CommandPermission::Denied);
        }

        let normalized = Self::normalize_command(command);
        let base_command = Self::extract_base_command(command);

        // Blanket `"Bash"` allow short-circuits below the deny check but
        // still defers to `forbidden_commands` — the user's "trust me"
        // intent stops at things that are dangerous regardless of
        // context (`rm`, `chmod`, `sudo`, …). Structural safety checks
        // (`>` redirection, `<<`, `git push`, parent traversal, external
        // paths) still run later in `bashexec`.
        let blanket_allow = self
            .settings
            .permissions
            .allow
            .iter()
            .any(|e| e == BLANKET_BASH);
        if blanket_allow {
            let bases = Self::collect_command_bases(base_command, command);
            if bases.iter().any(|b| self.forbidden_commands.contains(b)) {
                return Ok(CommandPermission::Denied);
            }
            return Ok(CommandPermission::Allowed);
        }

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

        // Walk every sub-command in a compound shell (`for ...; do ...; done`,
        // `cmd1 && cmd2`, `cmd1; cmd2 | cmd3`) so the verdict reflects the
        // whole pipeline, not just the first token. Two reasons:
        //
        // 1. `for f in *; do echo $f; sed -n '1,N'p $f; done` leads with the
        //    structural keyword `for`, which isn't in `allowed_commands` —
        //    the old single-token lookup forced a prompt even though every
        //    real step is read-only. Bases pulled from the compound let the
        //    same shell auto-allow.
        // 2. `cat foo && rm bar` used to slip past as Allowed because `cat`
        //    is on the allow-list; the smuggled `rm` was never seen. Any
        //    forbidden base anywhere in the pipeline now wins.
        //
        // The splitter does NOT descend into `$(...)` / backticks, so a
        // command smuggled there is still seen as part of the parent
        // command's args (same blind spot as before this change).
        let bases = Self::collect_command_bases(base_command, command);

        if bases.iter().any(|b| self.forbidden_commands.contains(b)) {
            return Ok(CommandPermission::Denied);
        }

        if bases.iter().all(|b| self.allowed_commands.contains(b)) {
            return Ok(CommandPermission::Allowed);
        }

        Ok(CommandPermission::Ask)
    }

    /// Build the list of base-command names to evaluate for a command.
    /// Falls back to the leading token from `extract_base_command` when
    /// the compound splitter finds no separators, so single commands
    /// and compound shells share one verdict path.
    pub(super) fn collect_command_bases(base_command: &str, command: &str) -> Vec<String> {
        let compound_bases = Self::enumerate_compound_bases(command);
        if compound_bases.is_empty() {
            vec![base_command.to_string()]
        } else {
            compound_bases
        }
    }

    pub fn ask_user_permission(&mut self, command: &str) -> Result<(bool, bool)> {
        let normalized = Self::normalize_command(command);
        let prompt = format!("Allow command `{}`?", command);

        // For commands whose args change every call (sed line ranges,
        // head/tail line counts, grep context flags, awk NR predicates),
        // "remember this exact command" would never match the next
        // invocation. Drop those to a plain Yes/No so the user isn't
        // offered a useless persistence option. Users who want to
        // allowlist the invocation family can add `Bash(cmd:*)` to
        // settings directly.
        let (confirmed, remember) = if Self::command_has_volatile_line_args(command) {
            let choices = ["Yes", "No"];
            let idx = confirm_multi_choice(&prompt, &choices, 1, ConfirmationType::Permission)?;
            (idx == 0, false)
        } else {
            Self::ask_three_way(&prompt)?
        };

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

    /// Heuristic: does `command` carry line-number / line-range args that
    /// make the exact string un-rememberable? Walks every sub-command of
    /// a compound shell looking for:
    ///
    /// - sed numeric addresses: `sed -n '10,20p'`, `sed '5d'`, `sed 1,5q`
    /// - head/tail numeric counts: `head -50`, `head -n 50`, `tail +20`
    /// - grep/rg context flags: `grep -A 3`, `grep -B5`, `grep -C 10`
    /// - awk record-number predicates: `awk 'NR==5'`, `awk 'NR<=10'`
    ///
    /// False positives just downgrade the prompt to Yes/No, so the
    /// heuristic prefers simple matches over exhaustive parsing.
    pub(super) fn command_has_volatile_line_args(command: &str) -> bool {
        Self::split_compound_command(command)
            .iter()
            .any(|segment| Self::segment_has_volatile_line_args(segment))
    }

    pub(super) fn segment_has_volatile_line_args(segment: &str) -> bool {
        let Some((base, args)) = Self::extract_segment_base_with_args(segment) else {
            return false;
        };
        match base {
            "sed" => Self::sed_has_numeric_address(&args),
            "head" | "tail" => Self::head_tail_has_numeric_count(&args),
            "grep" | "egrep" | "fgrep" | "rg" => Self::grep_has_context_count(&args),
            "awk" => Self::awk_has_nr_predicate(&args),
            _ => false,
        }
    }

    pub(super) fn sed_has_numeric_address(args: &[&str]) -> bool {
        args.iter().any(|raw| {
            let s = raw.trim_matches(['\'', '"']);
            // Strip trailing sed command letters (p/d/q/!) — what remains
            // must look like N or N,M with only digits.
            let addr = s.trim_end_matches(['p', 'd', 'q', '!']);
            if addr.is_empty() || addr == s {
                return false;
            }
            addr.split(',')
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
    }

    pub(super) fn head_tail_has_numeric_count(args: &[&str]) -> bool {
        // Separate form: `-n 50`, `-c 100`. Glued form: `-n50`, `-c100`,
        // bare digits with `-`/`+`: `-50`, `+20`. `-n`/`-c` are listed in
        // both lists so `-n50` is caught by the glued path while `-n`
        // alone is caught by the separate path.
        Self::scan_numeric_flag_arg(args, &["-n", "-c"], &["-n", "-c", "-", "+"])
    }

    pub(super) fn grep_has_context_count(args: &[&str]) -> bool {
        Self::scan_numeric_flag_arg(args, &["-A", "-B", "-C"], &["-A", "-B", "-C"])
    }

    /// Shared scanner for "does this arg list contain a flag whose value
    /// is a line number"? Flags in `separate_flags` consume the next arg
    /// (which must be all-digits). Prefixes in `glued_prefixes` match
    /// flag+digits in a single token (e.g. `-n50`, `-A3`, `+20`).
    pub(super) fn scan_numeric_flag_arg(
        args: &[&str],
        separate_flags: &[&str],
        glued_prefixes: &[&str],
    ) -> bool {
        let mut prev_was_flag = false;
        for arg in args {
            if prev_was_flag {
                prev_was_flag = false;
                if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_digit()) {
                    return true;
                }
                continue;
            }
            if separate_flags.contains(arg) {
                prev_was_flag = true;
                continue;
            }
            for prefix in glued_prefixes {
                if let Some(rest) = arg.strip_prefix(prefix) {
                    if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub(super) fn awk_has_nr_predicate(args: &[&str]) -> bool {
        args.iter().any(|raw| {
            let s = raw.trim_matches(['\'', '"']);
            for op in ["NR==", "NR<=", "NR>=", "NR<", "NR>"] {
                // Scan every occurrence of `op` — an earlier non-digit
                // match (e.g. `NR==var`) shouldn't shadow a later
                // numeric one (`NR==5`).
                let mut rest = s;
                while let Some(pos) = rest.find(op) {
                    let tail = &rest[pos + op.len()..];
                    if tail.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        return true;
                    }
                    rest = tail;
                }
            }
            false
        })
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
    pub(super) fn ask_three_way(prompt: &str) -> Result<(bool, bool)> {
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

    pub(super) fn rebuild_all_globs(&mut self) -> Result<()> {
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
