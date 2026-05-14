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

impl PermissionManager {
    pub(super) fn extract_base_command(command: &str) -> &str {
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
        // allow/deny decisions keyed to the real command name.
        without_prefix
            .split_whitespace()
            .find(|tok| !is_env_assignment(tok))
            .or_else(|| without_prefix.split_whitespace().next())
            .unwrap_or(without_prefix)
    }

    /// Quote-aware split of a compound command on shell separators
    /// (`;`, `\n`, `|`, `||`, `&&`). Lone `&` is preserved so that
    /// `2>&1` stays glued to its preceding token. Quoted regions are
    /// kept whole so `echo 'a; b'` doesn't split mid-string.
    ///
    /// Does NOT descend into `$(...)` command substitution or backtick
    /// substitution — a `rm` smuggled inside `echo $(rm bad)` is
    /// invisible to this splitter (and to the rest of the permission
    /// system, both before and after this change). Closing that hole
    /// would require yielding the inner sub-commands as separate
    /// segments and is intentionally out of scope here.
    pub(super) fn split_compound_command(command: &str) -> Vec<String> {
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
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    Self::push_segment(&mut segments, &mut current);
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
    /// Empty for a single command with no separators (callers fall back
    /// to `extract_base_command` in that case).
    pub(super) fn enumerate_compound_bases(command: &str) -> Vec<String> {
        Self::split_compound_command(command)
            .iter()
            .filter_map(|seg| {
                Self::extract_segment_base_with_args(seg).map(|(base, _)| base.to_string())
            })
            .collect()
    }
}
