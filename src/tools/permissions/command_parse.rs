//! Shell tokenisation and compound-command splitting. The permission
//! system needs to extract the *base command* (the first non-keyword,
//! non-env-assignment token) of every sub-command in a compound shell
//! so each sub-command can be evaluated against allow/deny lists
//! independently. The splitter is quote-aware but deliberately does
//! NOT descend into `$(...)` substitution — see
//! [`PermissionManager::split_compound_command`] for the rationale.

use crate::tools::permissions::PermissionManager;

/// POSIX shell control keywords that punctuate compound commands but
/// don't introduce one of their own. Stripped from segment heads so the
/// next non-keyword token is the base command we actually want to inspect.
pub(super) const COMPOUND_KEYWORDS: &[&str] = &[
    "do", "done", "then", "else", "elif", "fi", "case", "esac", "{", "}", "(",
];

/// Loop / conditional headers whose tail *is* a real command —
/// `while CMD; do ...`, `until CMD; do ...`, `if CMD; then ...`. The
/// keyword itself is skipped, and the rest of the segment is parsed
/// like any other command.
pub(super) const COMPOUND_HEADERS_WITH_BODY: &[&str] = &["while", "until", "if"];

/// Loop headers whose segment is a word list, *not* a command —
/// `for VAR in WORDS`. Matching one means the segment carries no
/// base command at all and should yield no entry.
pub(super) const COMPOUND_HEADERS_NO_BODY: &[&str] = &["for"];

/// Env-assignment keys whose value can swap the binary the shell ends
/// up running, regardless of what base command follows. `PATH=. cargo
/// build` reads `cargo` from the current directory; `LD_PRELOAD=evil.so
/// ls` runs `ls` under a hijacked loader. The base-command lookup
/// would otherwise classify these as `cargo` / `ls` and auto-allow,
/// so the permission system has to surface them explicitly.
const DANGEROUS_ENV_KEYS: &[&str] = &[
    "PATH",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "NODE_PATH",
    "PYTHONPATH",
];

/// Returns the dangerous env-key (verbatim, uppercased) found among
/// the leading env-assignment tokens of `segment`. Stops at the first
/// token that is not a `KEY=value`, since that's the base command and
/// any later occurrence isn't an env prefix any more.
pub(super) fn leading_dangerous_env_prefix(segment: &str) -> Option<&'static str> {
    for tok in segment.split_whitespace() {
        if !is_env_assignment(tok) {
            return None;
        }
        let key = tok.split_once('=').map(|(k, _)| k).unwrap_or(tok);
        let upper = key.to_ascii_uppercase();
        if upper.starts_with("DYLD_") {
            return Some("DYLD_*");
        }
        if let Some(name) = DANGEROUS_ENV_KEYS.iter().find(|d| **d == upper.as_str()) {
            return Some(*name);
        }
    }
    None
}

/// Match POSIX-shell `KEY=value` assignment tokens: the key starts with
/// a letter or underscore, is alphanumeric+`_` throughout, and is
/// followed by `=`. Used by `extract_base_command` to skip leading
/// env prefixes so `FOO=bar rm -rf /` is classified as a `rm`
/// invocation (and therefore caught by the forbidden-command set),
/// not as a never-heard-of `FOO=bar` command.
pub(super) fn is_env_assignment(tok: &str) -> bool {
    let Some((key, _)) = tok.split_once('=') else {
        return false;
    };
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Strip a base-command token so the forbidden-command lookup sees the
/// same name the shell will execute. Removes leading subshell / group /
/// quote / backslash wrappers (`(`, `{`, `\`, `'`, `"`) and their
/// matching trailing wrappers, then drops any leading directory prefix
/// (`/usr/bin/rm` -> `rm`, `.\bin\rm` -> `rm`). Case is preserved so the
/// returned name can flow into user-configured wildcard rules like
/// `Bash(MyTool:*)`; case-insensitive matching against the built-in
/// command sets happens at the comparison site.
pub(crate) fn clean_base_token(tok: &str) -> String {
    let stripped = tok.trim_start_matches(['(', '{', '\\', '\'', '"']);
    let stripped = stripped.trim_end_matches([')', '}', '\'', '"']);
    let after_unix = stripped.rsplit('/').next().unwrap_or(stripped);
    let after_win = after_unix.rsplit('\\').next().unwrap_or(after_unix);
    after_win.to_string()
}

/// Compare a cleaned base name against the built-in command sets.
/// Filesystem case-sensitivity differs by OS — Linux treats `RM` and
/// `rm` as distinct executables, Windows resolves both to the same
/// binary — so this collapses to lowercase on platforms where the
/// shell would not. Built-in `allowed_commands` and `forbidden_commands`
/// are stored lowercase, so the comparison is a single equality check
/// per base.
pub(super) fn command_lookup_key(base: &str) -> String {
    #[cfg(windows)]
    {
        base.to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        base.to_string()
    }
}

impl PermissionManager {
    pub(super) fn extract_base_command(command: &str) -> String {
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

        // Skip any leading env-var assignments: `FOO=bar rm -rf /`
        // runs `rm`, not `FOO=bar`. Without this, a shell prefix would
        // bypass both the forbidden-command set and any remembered
        // allow/deny decisions keyed to the real command name. Pass
        // the eventual base through `clean_base_token` so quote and
        // path-prefix obfuscation (`(rm`, `'rm'`, `/bin/rm`) cannot
        // disguise a forbidden binary from the membership check.
        let raw = without_prefix
            .split_whitespace()
            .find(|tok| !is_env_assignment(tok))
            .or_else(|| without_prefix.split_whitespace().next())
            .unwrap_or(without_prefix);
        clean_base_token(raw)
    }

    /// Quote-aware split of a compound command on shell separators
    /// (`;`, `\n`, `|`, `||`, `&&`, and bare `&` background-control).
    /// The `&` after `>` or `<` (e.g. `2>&1`, `>&2`, `&>`) is preserved
    /// because there it is part of a redirection operand, not a
    /// control operator. Quoted regions are kept whole so
    /// `echo 'a; b'` doesn't split mid-string.
    ///
    /// Does NOT descend into `$(...)` command substitution or backtick
    /// substitution — a `rm` smuggled inside `echo $(rm bad)` is
    /// invisible to this splitter (and to the rest of the permission
    /// system, both before and after this change). The structural
    /// check in `tools/bash/validate.rs::detect_command_substitution`
    /// rejects those constructs outright instead.
    pub(crate) fn split_compound_command(command: &str) -> Vec<String> {
        let mut segments: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut chars = command.chars().peekable();
        let mut quote: Option<char> = None;

        while let Some(c) = chars.next() {
            if let Some(q) = quote {
                current.push(c);
                if c == q {
                    quote = None;
                }
                continue;
            }
            match c {
                '\'' | '"' => {
                    quote = Some(c);
                    current.push(c);
                }
                ';' | '\n' => Self::push_segment(&mut segments, &mut current),
                '|' => {
                    if chars.peek() == Some(&'|') {
                        chars.next();
                    }
                    Self::push_segment(&mut segments, &mut current);
                }
                '&' => {
                    if chars.peek() == Some(&'&') {
                        chars.next();
                        Self::push_segment(&mut segments, &mut current);
                    } else if matches!(prev_non_space(&current), Some('>') | Some('<')) {
                        // Redirect operand: `2>&1`, `>&2`.
                        current.push(c);
                    } else {
                        // Lone `&` backgrounds and starts a new statement.
                        Self::push_segment(&mut segments, &mut current);
                    }
                }
                _ => current.push(c),
            }
        }
        Self::push_segment(&mut segments, &mut current);
        segments
    }

    fn push_segment(segments: &mut Vec<String>, current: &mut String) {
        let trimmed = current.trim();
        if !trimmed.is_empty() {
            segments.push(trimmed.to_string());
        }
        current.clear();
    }
}

/// Last non-whitespace character emitted into the current segment.
/// Used by `split_compound_command` to tell a redirect operand (`>&`,
/// `<&`, `2>&1`) apart from a control `&`.
fn prev_non_space(current: &str) -> Option<char> {
    current.chars().rev().find(|c| !c.is_whitespace())
}

impl PermissionManager {
    /// Strip leading shell-control prefixes from a segment and return
    /// the next "real" command (base + args). Returns `None` for
    /// segments that carry no command of their own — `for VAR in WORDS`
    /// loop headers, bare `done` / `fi` / `esac` closers, or shell
    /// comments (`# anything`).
    pub(super) fn extract_segment_base_with_args(segment: &str) -> Option<(&str, Vec<&str>)> {
        let mut tokens = segment.split_whitespace().peekable();
        while let Some(&tok) = tokens.peek() {
            if COMPOUND_HEADERS_NO_BODY.contains(&tok) {
                return None;
            }
            if is_env_assignment(tok)
                || COMPOUND_KEYWORDS.contains(&tok)
                || COMPOUND_HEADERS_WITH_BODY.contains(&tok)
            {
                tokens.next();
                continue;
            }
            break;
        }
        let base = tokens.next()?;
        // A bare `#` (or token starting with `#`) at the head of a
        // segment marks the rest as a comment — `ls; # tail msg`
        // splits to `["ls", "# tail msg"]`, and the second segment is
        // entirely commentary, not a command. Treating it as a base
        // would force a needless Ask prompt for what the shell ignores.
        if base.starts_with('#') {
            return None;
        }
        let args: Vec<&str> = tokens.collect();
        Some((base, args))
    }

    /// Base-command names of every sub-command in a compound shell.
    /// Empty for a single command with no separators. Each base passes
    /// through `clean_base_token` so wrappers can't disguise it.
    pub(super) fn enumerate_compound_bases(command: &str) -> Vec<String> {
        Self::split_compound_command(command)
            .iter()
            .filter_map(|seg| {
                Self::extract_segment_base_with_args(seg).map(|(base, _)| clean_base_token(base))
            })
            .collect()
    }
}
