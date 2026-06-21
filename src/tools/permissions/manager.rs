use crate::config::{GLOBAL_CONFIG_FILE, LOCAL_CONFIG_FILE, global_config_path, home_dir};
use crate::error::{Result, SofosError};
use crate::tools::permissions::CommandPermission;
use crate::tools::permissions::command_parse::{command_lookup_key, leading_dangerous_env_prefix};
use crate::tools::permissions::pattern::BLANKET_BASH;
use crate::tools::permissions::settings::PermissionSettings;
use crate::tools::utils::{ConfirmationType, confirm_multi_choice};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

pub struct PermissionManager {
    /// The merged global + local view used for every runtime permission
    /// check.
    pub(super) settings: PermissionSettings,
    /// Exactly what the local file holds (and what `save_settings` writes
    /// back). Kept apart from `settings` so a save never copies the
    /// global `~/.sofos/config.toml` rules into the local file.
    pub(super) local_settings: PermissionSettings,
    pub(super) local_settings_path: PathBuf,
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

        let mut settings = if let Some(global_path) = global_config_path() {
            Self::load_settings(&global_path)?
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
        settings.merge(local_settings.clone());

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
            local_settings,
            local_settings_path,
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
        // Rules merge from `~/.sofos/config.toml` (global) and
        // `<workspace>/.sofos/config.local.toml` (local). When a rule
        // appears in the global set we report both candidate files so
        // the user knows where to look; the local-only path stays
        // unambiguous.
        if self.global_rules.contains(rule) {
            format!(
                "~/{} (or {} if overridden)",
                GLOBAL_CONFIG_FILE, LOCAL_CONFIG_FILE
            )
        } else {
            LOCAL_CONFIG_FILE.to_string()
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

        // Edit in place with toml_edit so comments, formatting, and other
        // sections (notably [mcp-servers]) survive; only [permissions] is
        // replaced, from the local settings alone, never the merged view.
        let mut document = if self.local_settings_path.exists() {
            let existing = fs::read_to_string(&self.local_settings_path).map_err(|e| {
                SofosError::ToolExecution(format!("Failed to read config file: {}", e))
            })?;
            existing.parse::<toml_edit::DocumentMut>().map_err(|e| {
                SofosError::ToolExecution(format!("Failed to parse config file: {}", e))
            })?
        } else {
            toml_edit::DocumentMut::new()
        };

        let serialized: toml_edit::DocumentMut = toml::to_string(&self.local_settings)
            .map_err(|e| SofosError::ToolExecution(format!("Failed to serialize config: {}", e)))?
            .parse()
            .map_err(|e| SofosError::ToolExecution(format!("Failed to serialize config: {}", e)))?;
        document["permissions"] = serialized["permissions"].clone();

        crate::tools::filesystem::write_atomic(&self.local_settings_path, &document.to_string())
            .map_err(|e| {
                SofosError::ToolExecution(format!("Failed to write config file: {}", e))
            })?;

        Ok(())
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
            return home_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string());
        }
        if let Some(rest) = path.strip_prefix("~/") {
            if let Some(mut home) = home_dir() {
                let rest = rest.trim_start_matches(['/', '\\']);
                // Push each segment so the join uses the platform's
                // native separator on both sides. A single `push(rest)`
                // would keep forward slashes inside `rest` verbatim,
                // giving `C:\Users\me\docs/file.txt` on Windows.
                // Filtering empty segments collapses `//` runs.
                for segment in rest.split('/').filter(|s| !s.is_empty()) {
                    home.push(segment);
                }
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

        // Dangerous env prefixes (`PATH=.`, `LD_PRELOAD=...`) can swap
        // the binary the shell runs, so allow paths downgrade to Ask.
        // Denies still fire normally.
        let dangerous_env = Self::command_has_dangerous_env_prefix(command);
        let allow_verdict = if dangerous_env {
            CommandPermission::Ask
        } else {
            CommandPermission::Allowed
        };

        let normalized = Self::normalize_command_key(command);
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
            let bases = Self::collect_command_bases(&base_command, command);
            if bases
                .iter()
                .any(|b| self.forbidden_commands.contains(&command_lookup_key(b)))
            {
                return Ok(CommandPermission::Denied);
            }
            return Ok(allow_verdict);
        }

        // An exact-match allow is an explicit opt-in to the full
        // command (env prefix included), so it bypasses the
        // dangerous-env downgrade. Wildcards below still downgrade.
        if self.settings.permissions.allow.contains(&normalized) {
            return Ok(CommandPermission::Allowed);
        }

        if self.settings.permissions.deny.contains(&normalized) {
            return Ok(CommandPermission::Denied);
        }

        let wildcard_pattern = format!("Bash({}:*)", base_command);
        if self.settings.permissions.allow.contains(&wildcard_pattern) {
            return Ok(allow_verdict);
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
        let bases = Self::collect_command_bases(&base_command, command);

        if bases
            .iter()
            .any(|b| self.forbidden_commands.contains(&command_lookup_key(b)))
        {
            return Ok(CommandPermission::Denied);
        }

        if bases
            .iter()
            .all(|b| self.allowed_commands.contains(&command_lookup_key(b)))
        {
            return Ok(allow_verdict);
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

    /// True when any sub-command of a compound shell leads with a
    /// dangerous env-assignment (`PATH=`, `LD_PRELOAD=`, etc.). Walking
    /// every segment means `ls; PATH=. cargo build` is caught even though
    /// the first segment is plain.
    pub(crate) fn command_has_dangerous_env_prefix(command: &str) -> bool {
        Self::split_compound_command(command)
            .iter()
            .any(|seg| leading_dangerous_env_prefix(seg).is_some())
    }

    pub fn ask_user_permission(&mut self, command: &str) -> Result<(bool, bool)> {
        let normalized = Self::normalize_command_key(command);
        let prompt = format!("Allow command `{}`?", command);

        // "Remember this exact command" only helps when the same string
        // can recur. Commands whose args change every call (sed line
        // ranges, head/tail counts, grep context flags, awk NR predicates)
        // and one-shot shapes (multi-line scripts, command / process
        // substitution, heredocs) never will, so they drop to a plain
        // Yes/No instead of offering a persistence option that would only
        // clutter config.local.toml. Users who want to allowlist an
        // invocation family can add `Bash(cmd:*)` to settings directly.
        let (confirmed, remember) = if Self::command_not_rememberable(command) {
            let choices = ["Yes", "No"];
            let idx = confirm_multi_choice(&prompt, &choices, 1, ConfirmationType::Permission)?;
            (idx == 0, false)
        } else {
            Self::ask_three_way(&prompt)?
        };

        if remember {
            self.remember_rule(normalized, confirmed);
            self.save_settings()?;
            self.rebuild_all_globs()?;
        }

        Ok((confirmed, remember))
    }

    /// Whether the prompt for `command` should drop the "and remember"
    /// options and offer a plain Yes/No, because a remembered rule could
    /// never usefully apply:
    /// - volatile line-number args ([`Self::command_has_volatile_line_args`]):
    ///   the args change every run, so the exact string never recurs
    /// - one-shot shapes ([`Self::command_is_one_shot`]): multi-line
    ///   scripts and heredocs that won't be retyped verbatim, and command
    ///   or process substitution (`$( )`, backticks, `<( )`, `>( )`) which
    ///   the executor rejects outright, so a saved rule could never fire
    pub(super) fn command_not_rememberable(command: &str) -> bool {
        Self::command_has_volatile_line_args(command) || Self::command_is_one_shot(command)
    }

    pub(super) fn command_is_one_shot(command: &str) -> bool {
        command.contains('\n')
            || command.contains("$(")
            || command.contains('`')
            || command.contains("<<")
            || command.contains("<(")
            || command.contains(">(")
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

        // A degenerate directory (filesystem root, or a `host:container`
        // docker mount) can't be persisted as a sane `<scope>(<dir>/**)`
        // rule, so offer only Yes/No there instead of the "and remember"
        // options — the prompt must not promise a grant it would silently
        // drop. Same handling as an un-rememberable command.
        let (confirmed, remember) = if Self::is_persistable_grant_dir(dir) {
            Self::ask_three_way(&prompt)?
        } else {
            let choices = ["Yes", "No"];
            let idx = confirm_multi_choice(&prompt, &choices, 1, ConfirmationType::Permission)?;
            (idx == 0, false)
        };

        if remember {
            self.remember_rule(grant, confirmed);
            self.save_settings()?;
            self.rebuild_all_globs()?;
        }

        Ok((confirmed, remember))
    }

    /// Ask the user whether `web_fetch` may reach `host`, offering the
    /// four-way Yes / Yes-and-remember / No / No-and-remember modal.
    /// Returns `(confirmed, remember)`. A remembered choice is persisted
    /// as a `WebFetch(domain:<host>)` rule in the allow or deny list.
    /// Web-fetch rules are matched by a direct host scan rather than a
    /// glob set, so no glob rebuild is needed for the new rule to apply.
    pub fn ask_user_web_fetch_permission(&mut self, host: &str) -> Result<(bool, bool)> {
        let prompt = format!("Allow web_fetch to `{}`?", host);
        let (confirmed, remember) = Self::ask_three_way(&prompt)?;
        if remember {
            self.remember_rule(Self::normalize_web_fetch(host), confirmed);
            self.save_settings()?;
        }
        Ok((confirmed, remember))
    }

    /// Ask whether the model may use tools from MCP `server` (it is calling
    /// `tool`). The grant is server-wide: a "yes", or a remembered
    /// `Mcp(<server>)` rule, covers every tool from that server. Returns
    /// `(confirmed, remember)`. MCP rules match by a direct server-name
    /// scan, so no glob rebuild is needed.
    pub fn ask_user_mcp_permission(&mut self, server: &str, tool: &str) -> Result<(bool, bool)> {
        let prompt = format!(
            "Allow tools from MCP server `{}`? (requested `{}`)",
            server, tool
        );
        let (confirmed, remember) = Self::ask_three_way(&prompt)?;
        if remember {
            self.remember_rule(Self::normalize_mcp(server), confirmed);
            self.save_settings()?;
        }
        Ok((confirmed, remember))
    }

    /// Persist `rule` into the allow (`allow == true`) or deny list,
    /// skipping the push when an identical rule is already present so
    /// repeat grants of the same command or directory don't accumulate
    /// duplicate lines in config.local.toml. The rule lands in both the
    /// local settings (so `save_settings` writes it to the local file)
    /// and the merged runtime view (so the grant takes effect at once).
    pub(super) fn remember_rule(&mut self, rule: String, allow: bool) {
        if allow {
            Self::push_unique(&mut self.local_settings.permissions.allow, &rule);
            Self::push_unique(&mut self.settings.permissions.allow, &rule);
        } else {
            Self::push_unique(&mut self.local_settings.permissions.deny, &rule);
            Self::push_unique(&mut self.settings.permissions.deny, &rule);
        }
    }

    /// Append `rule` to `list` unless it is already present.
    fn push_unique(list: &mut Vec<String>, rule: &str) {
        if !list.iter().any(|r| r == rule) {
            list.push(rule.to_string());
        }
    }

    /// Whether `dir` is specific enough to persist as a `<scope>(<dir>/**)`
    /// rule. Rejects the filesystem root and the empty string — the grant
    /// would match the whole machine, which is where `Bash(//**)` came
    /// from (`docker -w /work` gives `/work` a parent of `/`) — and
    /// Unix-absolute paths carrying a colon, which are `host:container`
    /// docker mounts or `PATH`-style lists (`-v /repo:/work:ro`) rather
    /// than a single directory and would persist as nonsense such as
    /// `Bash(/repo:/**)`.
    pub(super) fn is_persistable_grant_dir(dir: &str) -> bool {
        if dir.is_empty() || std::path::Path::new(dir).parent().is_none() {
            return false;
        }
        !(dir.starts_with('/') && dir.contains(':'))
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
