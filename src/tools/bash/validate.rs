//! Structural and path-policy checks for the bash executor.
//!
//! Two layers: free functions that look at command text in isolation
//! (parent-traversal detection, shell-boundary-aware operator search)
//! and methods on [`BashExecutor`] that combine those checks with the
//! workspace's permission state — including the per-command rejection
//! messages the executor surfaces to the model.

use crate::error::{Result, SofosError};
use crate::tools::ToolName;
use crate::tools::bash::BashExecutor;
use crate::tools::permissions::{CommandPermission, PermissionManager};
use crate::tools::utils::{is_absolute_path, lexically_normalize, normalize_command_whitespace};
use std::path::PathBuf;

/// Return true when a command argument looks like a parent-directory
/// reference (`..`, `../foo`, `foo/..`, `foo/../bar`). Substring matches
/// inside opaque tokens — like git revision ranges (`HEAD~5..HEAD`) or
/// regex patterns (`\.\.\.`) — are intentionally NOT flagged: blocking
/// them was what stopped the AI from running legitimate git diagnostics
/// when a file ended up corrupted.
///
/// The command is split on whitespace and also on `=` / `:`, so that
/// flag-embedded traversals (`--include=../secret.h`) and PATH-style
/// assignments (`PATH=/usr/bin:../foo`) surface their `..` fragment as
/// its own token rather than hiding inside an opaque `KEY=VALUE`
/// string. Git range syntax (`HEAD~5..HEAD`, `HEAD^:path`) survives
/// the split because neither `..` nor `^` are delimiters here.
pub(super) fn has_path_traversal(command: &str) -> bool {
    let split = |c: char| c.is_whitespace() || matches!(c, '=' | ':');
    for raw in command.split(split).filter(|t| !t.is_empty()) {
        // Strip the common shell wrappers the parser would peel off
        // anyway, so `"../foo"`, `` `../foo` ``, and `$(cat ../foo)`
        // all still flag as traversal after the trailing `)`, quote,
        // or backtick is removed.
        let t = raw.trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | '`' | '(' | ')' | '{' | '}' | '[' | ']' | ';' | ','
            )
        });
        if t == ".." || t.starts_with("../") || t.ends_with("/..") || t.contains("/../") {
            return true;
        }
    }
    false
}

/// Detect shell command and process substitution outside of single
/// quotes. Returns the marker that triggered the match so the rejection
/// message can name it. Single quotes suppress substitution in POSIX
/// shells, so `echo '$(rm bad)'` is literal text; double quotes do not
/// suppress it. A backslash outside single quotes escapes the next
/// byte, so `\$(foo)` is literal. Arithmetic expansion `$((expr))` is
/// not flagged because it evaluates a numeric expression rather than
/// running a command, but a substitution nested inside arithmetic is
/// still caught when the scanner reaches the inner `$(`.
pub(super) fn detect_command_substitution(command: &str) -> Option<&'static str> {
    let bytes = command.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let b = bytes[i];
        if !in_single && b == b'\\' {
            i = i.saturating_add(2);
            continue;
        }
        if !in_double && b == b'\'' {
            in_single = !in_single;
            i += 1;
            continue;
        }
        if !in_single && b == b'"' {
            in_double = !in_double;
            i += 1;
            continue;
        }
        if !in_single {
            if b == b'`' {
                return Some("`");
            }
            if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
                if i + 2 < bytes.len() && bytes[i + 2] == b'(' {
                    i += 3;
                    continue;
                }
                return Some("$(");
            }
            if (b == b'<' || b == b'>') && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
                return Some(if b == b'<' { "<(" } else { ">(" });
            }
        }
        i += 1;
    }
    None
}

/// Return true when `op` appears as a command-name prefix anywhere in
/// `command` — at the start, or immediately after a shell
/// command-boundary sequence. The set of boundaries must cover every
/// place the shell can start executing a new command, because this
/// function gates our forbidden-git detection: if we miss one, the
/// model can wrap `git push` in that construct and bypass the check.
///
/// Covered: plain space, `;`, `&&`, `||`, `|`, backtick substitution
/// (`` `git push` ``), `$(...)` command substitution, `(...)` subshell,
/// `{...; }` group. False positives (e.g. `ls {git,svn}` brace
/// expansion triggering `git` detection) are acceptable — the worst
/// outcome is the user being prompted to confirm a benign command.
///
/// The caller is expected to pass `command` already routed through
/// [`normalize_command_whitespace`] so non-space whitespace (`\t`,
/// `\r`, `\v`, `\f`, backslash-newline) and the explicit `$IFS` /
/// `${IFS}` shell expansion appear as plain single spaces — otherwise
/// `git\tpush`, `git$IFS\tpush`, and `git\\\npush` would evade the
/// boundary check while still running as `git push`.
pub(super) fn command_contains_op(command: &str, op: &str) -> bool {
    const BOUNDARIES: &[&str] = &[" ", ";", "&&", "||", "|", "`", "$(", "(", "{"];
    if command.starts_with(op) {
        return true;
    }
    BOUNDARIES
        .iter()
        .any(|sep| command.contains(&format!("{sep}{op}")))
}

/// Returns the kind of expansion that would change the path between
/// our deny check and the shell touching it: `$` / `${...}`, backticks,
/// `~user`, or glob metacharacters (`?`, `*`, `[`, `{`). Plain `~/`
/// and bare `~` are allowed because they expand to a known prefix and
/// are handled by the tilde-expansion helper.
pub(super) fn path_token_shell_meta(tok: &str) -> Option<&'static str> {
    if tok.contains('$') {
        return Some("$ variable expansion");
    }
    if tok.contains('`') {
        return Some("backtick command substitution");
    }
    if tok.starts_with('~') && tok != "~" && !tok.starts_with("~/") {
        return Some("~user home expansion");
    }
    if tok.contains('?') || tok.contains('*') || tok.contains('[') || tok.contains('{') {
        return Some("glob expansion");
    }
    None
}

/// True for tokens that look like a path the shell will resolve at
/// run-time. Flag tokens (`--name=value`) and regex patterns fall
/// through so the stricter shell-meta check only fires on real paths.
fn token_looks_like_path(tok: &str) -> bool {
    tok.contains('/') || tok.starts_with('.') || tok.starts_with('~') || is_absolute_path(tok)
}

impl BashExecutor {
    /// Check all external paths (absolute or tilde) in a command against Bash path grants.
    /// Asks the user interactively for any paths not yet covered.
    pub(super) fn check_bash_external_paths(
        &self,
        command: &str,
        permission_manager: &mut PermissionManager,
    ) -> Result<()> {
        for token in command.split_whitespace() {
            let cleaned = token
                .trim_matches('"')
                .trim_matches('\'')
                .trim_matches(';')
                .trim();

            if cleaned.is_empty() {
                continue;
            }

            // `--flag=/path` and `--flag=~/path` tokens would otherwise
            // be swallowed whole by the `starts_with('-')` filter below,
            // so split at the first `=` to expose the path half. Without
            // this, `grep --include=/etc/passwd` bypasses the external-
            // path prompt entirely.
            let path_candidate = if cleaned.starts_with('-') {
                match cleaned.find('=') {
                    Some(i) => cleaned[i + 1..].trim_matches(|c: char| matches!(c, '"' | '\'')),
                    None => continue,
                }
            } else {
                cleaned
            };

            // Reject path tokens whose post-expansion shape we cannot check.
            if token_looks_like_path(path_candidate) {
                if let Some(kind) = path_token_shell_meta(path_candidate) {
                    return Err(SofosError::ToolExecution(format!(
                        "Path argument '{}' uses {} which can't be checked against the permission rules before the shell expands it\n\
                         Hint: pass the resolved literal path instead, or split this into a separate step that doesn't reference the same path.",
                        path_candidate, kind
                    )));
                }
            }

            // Check tilde before absolute so `~` / `~/foo` get expanded
            // first. `is_absolute_path` catches Unix (`/foo`) and
            // Windows (`C:\foo`, `\\server\share`) shapes on every
            // platform — `Path::is_absolute` alone would miss Unix
            // paths on Windows, letting a bash command referencing
            // `/etc/passwd` bypass the external-path prompt when the
            // binary runs there.
            if path_candidate.starts_with("~/") || path_candidate == "~" {
                let expanded = PermissionManager::expand_tilde_pub(path_candidate);
                self.check_bash_external_path(&expanded, permission_manager)?;
            } else if is_absolute_path(path_candidate) {
                self.check_bash_external_path(path_candidate, permission_manager)?;
            } else {
                // Workspace-relative token whose canonical resolution may
                // leave the workspace through a symlink. Canonicalize
                // against the workspace and, if the result lands outside,
                // route it through the same external-path gate.
                self.check_workspace_relative_escape(path_candidate, permission_manager)?;
            }
        }

        Ok(())
    }

    fn check_workspace_relative_escape(
        &self,
        path_candidate: &str,
        permission_manager: &mut PermissionManager,
    ) -> Result<()> {
        let joined = self.workspace.join(path_candidate);
        let canonical = match std::fs::canonicalize(&joined) {
            Ok(path) => path,
            Err(_) => return Ok(()),
        };
        if canonical.starts_with(&self.workspace) {
            return Ok(());
        }
        let canonical_str = canonical.to_string_lossy().to_string();
        self.check_bash_external_path(&canonical_str, permission_manager)
    }

    /// Check a single external path against Bash path grants; ask user if not covered.
    pub(super) fn check_bash_external_path(
        &self,
        path: &str,
        permission_manager: &mut PermissionManager,
    ) -> Result<()> {
        // Canonicalize when possible; also keep a lexically normalized form
        // and the raw input so deny rules match every shape of the same path.
        let canonical = std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().to_string())
            .ok();
        let normalized = lexically_normalize(&PathBuf::from(path))
            .to_string_lossy()
            .to_string();
        let candidates: Vec<String> = match canonical {
            Some(c) if c == normalized || c == path => vec![c],
            Some(c) => vec![c, normalized.clone(), path.to_string()],
            None if normalized == path => vec![path.to_string()],
            None => vec![normalized.clone(), path.to_string()],
        };
        let check_path = candidates
            .first()
            .cloned()
            .unwrap_or_else(|| path.to_string());

        // Deny wins over allow; check every shape.
        for cand in &candidates {
            if permission_manager.is_bash_path_denied(cand) {
                return Err(SofosError::ToolExecution(format!(
                    "Bash access denied for path '{}'\n\
                     Hint: Blocked by deny rule in .sofos/config.local.toml or ~/.sofos/config.toml",
                    cand
                )));
            }
        }

        // Already allowed by config?
        if permission_manager.is_bash_path_allowed(&check_path) {
            return Ok(());
        }

        let path_obj = std::path::Path::new(&check_path);

        // Session allowed?
        if let Ok(allowed_dirs) = self.bash_path_session_allowed.lock() {
            for dir in allowed_dirs.iter() {
                if path_obj.starts_with(std::path::Path::new(dir)) {
                    return Ok(());
                }
            }
        }

        // Session denied?
        if let Ok(denied_dirs) = self.bash_path_session_denied.lock() {
            for dir in denied_dirs.iter() {
                if path_obj.starts_with(std::path::Path::new(dir)) {
                    return Err(SofosError::ToolExecution(format!(
                        "Bash access denied for path '{}' (denied earlier this session)",
                        check_path
                    )));
                }
            }
        }

        let parent = std::path::Path::new(&check_path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or(&check_path);

        // Non-interactive mode (tests, piped input): deny with a config hint
        if !self.interactive {
            return Err(SofosError::ToolExecution(format!(
                "Command references path '{}' outside workspace\n\
                 Hint: Add Bash({}/**) to 'allow' list in .sofos/config.local.toml",
                check_path, parent
            )));
        }

        // Ask user interactively
        let (allowed, remember) = permission_manager.ask_user_path_permission("Bash", parent)?;

        if allowed {
            if !remember {
                if let Ok(mut dirs) = self.bash_path_session_allowed.lock() {
                    // Session-only grant: store the file path itself, not
                    // the parent, so a second file under the same parent
                    // re-prompts. Persistent grants (remember=true) keep
                    // `Bash(parent/**)` because the user explicitly
                    // opted in to that scope through `ask_user_path_permission`.
                    dirs.insert(check_path.to_string());
                }
            }
            Ok(())
        } else {
            if !remember {
                if let Ok(mut dirs) = self.bash_path_session_denied.lock() {
                    dirs.insert(check_path.to_string());
                }
            }
            Err(SofosError::ToolExecution(format!(
                "Bash access denied by user for path '{}'",
                check_path
            )))
        }
    }

    pub(super) fn enforce_read_permissions(
        &self,
        permission_manager: &PermissionManager,
        command: &str,
    ) -> Result<()> {
        // Heuristic-based detection of file paths in commands.
        // Checks paths against Read deny rules (regardless of Bash path grants).
        // External path access is handled separately by check_bash_external_paths.
        for token in command.split_whitespace().skip(1) {
            let cleaned = token
                .trim_matches('"')
                .trim_matches('\'')
                .trim_matches(';')
                .trim();

            if cleaned.is_empty() || cleaned.starts_with('-') {
                continue;
            }

            let path_shaped =
                cleaned.contains('/') || cleaned.starts_with('.') || cleaned.starts_with('~');

            if path_shaped {
                if let Some(kind) = path_token_shell_meta(cleaned) {
                    return Err(SofosError::ToolExecution(format!(
                        "Read argument '{}' uses {} which can't be checked against the Read rules before the shell expands it\n\
                         Hint: pass the resolved literal path instead, or split this into a separate step that doesn't reference the same path.",
                        cleaned, kind
                    )));
                }
            }

            // Path candidates: looks-like-path, or a bare token with no
            // expansion meta (regex / ad-hoc strings fall through).
            let is_path = path_shaped
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
                            "Read access denied for path '{}' in command\n\
                             Hint: Blocked by deny rule in {}",
                            cleaned, config_source
                        )));
                    }
                    CommandPermission::Ask => {
                        return Err(SofosError::ToolExecution(format!(
                            "Path '{}' requires confirmation per config file\n\
                             Hint: Move it to 'allow' or 'deny' list.",
                            cleaned
                        )));
                    }
                }
            }
        }

        Ok(())
    }

    pub(super) fn is_safe_command_structure(&self, command: &str) -> bool {
        // Parent directory traversal — always blocked (use absolute paths for external access)
        if has_path_traversal(command) {
            return false;
        }

        // Command and process substitution hide subcommands from the
        // permission system, so `echo $(rm bad)` would otherwise be
        // classified by the base-command (`echo`).
        if detect_command_substitution(command).is_some() {
            return false;
        }

        // Note: absolute paths (/...) and tilde paths (~/) are now handled by
        // check_bash_external_paths which asks the user interactively.

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

        // Normalise whitespace before matching so `git\tpush`,
        // `git$IFS\tpush`, `git\\\npush` are seen as `git push`.
        let matcher_input = normalize_command_whitespace(command).to_lowercase();
        if !self.is_safe_git_command(&matcher_input) {
            return false;
        }

        true
    }

    pub(super) fn is_safe_git_command(&self, command: &str) -> bool {
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

        // Dangerous git operations that are completely blocked.
        //
        // File-recovery commands (`git restore <path>`, `git checkout --`)
        // are intentionally NOT on this list any more: when `morph_edit_file`
        // or `edit_file` corrupts a file, the model needs a way to roll back
        // to HEAD without going through the write tools (which would just
        // write whatever broken content the model already has). These
        // commands only affect specified paths, so the blast radius is
        // bounded by whichever path the model names.
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
            "git switch",
        ];

        for dangerous_op in &dangerous_git_ops {
            if command_contains_op(command, dangerous_op) {
                return false;
            }
        }

        true
    }

    pub(super) fn get_rejection_reason(&self, command: &str) -> String {
        let matcher_input = normalize_command_whitespace(command).to_lowercase();

        if has_path_traversal(command) {
            return format!(
                "Command '{}' contains '..' as a path component (parent directory traversal)\n\
                 Hint: Use absolute paths for external directory access instead of '..'. \
                 Git revision ranges like `HEAD~5..HEAD` are allowed.",
                command
            );
        }

        if let Some(marker) = detect_command_substitution(command) {
            return format!(
                "Command '{}' uses shell substitution ('{}') which would run a hidden subcommand outside the permission system\n\
                 Hint: Run each step as its own bash call so the permission system can see it. Use single quotes if you need the literal characters.",
                command, marker
            );
        }

        if !self.is_safe_git_command(&matcher_input) {
            return self.get_git_rejection_reason(command);
        }

        let command_without_stderr_redirect = command.replace("2>&1", "");
        if command_without_stderr_redirect.contains('>')
            || command_without_stderr_redirect.contains(">>")
        {
            let edit_hint: String = if self.has_morph {
                format!(
                    "{}/{}",
                    ToolName::EditFile.as_str(),
                    ToolName::MorphEditFile.as_str()
                )
            } else {
                ToolName::EditFile.as_str().to_string()
            };
            return format!(
                "Command '{}' contains output redirection ('>' or '>>')\n\
                 Hint: Use write_file tool to create or {} to modify files. Note: '2>&1' is allowed.",
                command, edit_hint
            );
        }

        if command.contains("<<") {
            return format!(
                "Command '{}' contains here-doc ('<<')\n\
                 Hint: Use write_file tool to create files instead.",
                command
            );
        }

        format!(
            "Command '{}' is in the forbidden list (destructive or violates sandbox)\n\
             Hint: Use appropriate file operation tools instead.",
            command
        )
    }

    pub(super) fn get_git_rejection_reason(&self, command: &str) -> String {
        // Match against the same normalized input as `is_safe_git_command`
        // so the reason picked here lines up with the rejection.
        let command_lower = normalize_command_whitespace(command).to_lowercase();

        if command_lower.contains("git push") {
            return format!(
                "Command '{}' blocked: 'git push' sends data to remote repositories\n\
                 Hint: Use 'git status', 'git log', 'git diff' to view changes.",
                command
            );
        }

        if command_lower.contains("git pull") || command_lower.contains("git fetch") {
            let op = if command_lower.contains("git pull") {
                "git pull"
            } else {
                "git fetch"
            };
            return format!(
                "Command '{}' blocked: '{}' fetches data from remote repositories\n\
                 Hint: Use 'git status', 'git log', 'git diff' to view local changes.",
                command, op
            );
        }

        if command_lower.contains("git clone") {
            return format!(
                "Command '{}' blocked: 'git clone' downloads repositories\n\
                 Hint: Clone repositories manually outside of Sofos.",
                command
            );
        }

        if command_lower.contains("git commit") || command_lower.contains("git add") {
            let op = if command_lower.contains("git commit") {
                "git commit"
            } else {
                "git add"
            };
            return format!(
                "Command '{}' blocked: '{}' modifies the git repository\n\
                 Hint: Use 'git status', 'git diff' to view changes. Create commits manually.",
                command, op
            );
        }

        if command_lower.contains("git reset") || command_lower.contains("git clean") {
            let op = if command_lower.contains("git reset") {
                "git reset"
            } else {
                "git clean"
            };
            return format!(
                "Command '{}' blocked: '{}' is a destructive operation\n\
                 Hint: Use 'git status', 'git log', 'git diff' to view repository state.",
                command, op
            );
        }

        if command_lower.contains("git checkout") || command_lower.contains("git switch") {
            let op = if command_lower.contains("git checkout") {
                "git checkout"
            } else {
                "git switch"
            };
            return format!(
                "Command '{}' blocked: '{}' changes branches or modifies working directory\n\
                 Hint: Use 'git branch' to list branches, 'git status' to see current branch.",
                command, op
            );
        }

        if command_lower.contains("git merge") || command_lower.contains("git rebase") {
            let op = if command_lower.contains("git merge") {
                "git merge"
            } else {
                "git rebase"
            };
            return format!(
                "Command '{}' blocked: '{}' modifies git history\n\
                 Hint: Perform merges/rebases manually outside of Sofos.",
                command, op
            );
        }

        if command_lower.contains("git stash")
            && !command_lower.contains("git stash list")
            && !command_lower.contains("git stash show")
        {
            return format!(
                "Command '{}' blocked: 'git stash' modifies repository state\n\
                 Hint: Use 'git stash list' or 'git stash show' to view stashed changes.",
                command
            );
        }

        if command_lower.contains("git remote add") || command_lower.contains("git remote set-url")
        {
            return format!(
                "Command '{}' blocked: Modifying git remotes is not allowed\n\
                 Hint: Use 'git remote -v' to view configured remotes.",
                command
            );
        }

        if command_lower.contains("git submodule") {
            return format!(
                "Command '{}' blocked: 'git submodule' can fetch from remote repositories\n\
                 Hint: Manage submodules manually outside of Sofos.",
                command
            );
        }

        format!(
            "Command '{}' blocked: git operation modifies repository or accesses network\n\
             Hint: Allowed git commands: status, log, diff, show, branch, remote -v, grep, blame",
            command
        )
    }
}
