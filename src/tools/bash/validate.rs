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
use crate::tools::permissions::command_parse::{
    COMPOUND_HEADERS_NO_BODY, COMPOUND_HEADERS_WITH_BODY, COMPOUND_KEYWORDS, is_env_assignment,
};
use crate::tools::permissions::{CommandPermission, PermissionManager, grant_dir_for_path};
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

/// Programs that run one of their arguments as a command, so a dangerous
/// `git` call can hide behind them — `env git push`, `timeout 5 git push`,
/// `nice git push`, `xargs git push`. We look past the launcher to the git
/// call rather than stopping at the wrapper's name.
const GIT_LAUNCHERS: &[&str] = &[
    "env", "nice", "time", "timeout", "command", "nohup", "setsid", "stdbuf", "xargs", "ionice",
    "taskset", "chrt",
];

/// Shells that run the string after `-c` as a command, so `sh -c "git
/// push"` is inspected by re-parsing that string.
const GIT_SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ksh"];

/// Git global options that take their value as the following word, so the
/// real subcommand is the token after the value. Matched case-sensitively
/// (`-C` changes directory, `-c` sets config) and only without `=`, since
/// the `=` form keeps option and value in one token. Any other leading
/// `-` token is treated as possibly value-taking too (see
/// [`git_subcommand_candidates`]), so a future option cannot hide the verb.
const GIT_GLOBAL_VALUE_OPTIONS: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--super-prefix",
    "--config-env",
    "--attr-source",
    "--shallow-file",
];

/// Git global options that take no value. Listed so the subcommand scan
/// does not also explore the "consumes the next word" branch for them,
/// which would step over the real subcommand into its arguments and
/// mis-read a pathspec named like a verb (`git --no-pager log -- rm`).
const GIT_GLOBAL_NOARG_OPTIONS: &[&str] = &[
    "-p",
    "-P",
    "--paginate",
    "--no-pager",
    "--bare",
    "--no-replace-objects",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
    "--no-optional-locks",
    "--no-advice",
];

/// Bound on how deep we follow `sh -c "..."` / launcher nesting.
const GIT_NESTING_LIMIT: u8 = 8;

/// Finish the current word: emit it, or drop it when it is a redirection
/// target the shell would strip from the argument vector.
fn flush_word(words: &mut Vec<String>, cur: &mut String, in_word: &mut bool, drop_next: &mut bool) {
    if !*in_word {
        return;
    }
    if *drop_next {
        cur.clear();
        *drop_next = false;
    } else {
        words.push(std::mem::take(cur));
    }
    *in_word = false;
}

/// Split a shell segment into words the way the shell would, honouring
/// single quotes, double quotes, and backslash escapes and removing those
/// quoting characters. Subshell parentheses end the current word so
/// `(git push)` yields `git`/`push`; brace groups are handled separately
/// because `{` is a standalone keyword token. Redirections (`< file`,
/// `2> out`, `git<file`) are dropped, operator and target both, so the
/// words match the argument vector the shell hands the program — otherwise
/// `git <file push` would look like the subcommand was `<file`. Expansions
/// are not performed — `$(...)` and backticks are rejected upstream by
/// `detect_command_substitution` — so a `$` or backtick is kept literally.
/// `git -C 'a b' push` yields four words with `a b` intact rather than
/// splitting the quoted value.
fn shell_words(segment: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    // The next finished word is a redirection target to drop.
    let mut drop_next = false;
    let mut chars = segment.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            Some(_) => match c {
                '"' => quote = None,
                '\\' => match chars.peek() {
                    Some(&n) if matches!(n, '"' | '\\' | '$' | '`') => {
                        cur.push(n);
                        chars.next();
                    }
                    _ => cur.push('\\'),
                },
                _ => cur.push(c),
            },
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                    in_word = true;
                }
                '<' | '>' => {
                    // A bare fd number right before the operator (`2>`,
                    // `0<`) is part of the redirection, not a word.
                    if in_word && cur.chars().all(|c| c.is_ascii_digit()) {
                        cur.clear();
                        in_word = false;
                    } else {
                        flush_word(&mut words, &mut cur, &mut in_word, &mut drop_next);
                    }
                    // Swallow the rest of the operator: >> << <> >& <& >|.
                    while matches!(chars.peek(), Some('<' | '>' | '&' | '|')) {
                        chars.next();
                    }
                    drop_next = true;
                }
                '(' | ')' => flush_word(&mut words, &mut cur, &mut in_word, &mut drop_next),
                _ if c.is_whitespace() => {
                    flush_word(&mut words, &mut cur, &mut in_word, &mut drop_next)
                }
                _ => {
                    cur.push(c);
                    in_word = true;
                }
            },
        }
    }
    flush_word(&mut words, &mut cur, &mut in_word, &mut drop_next);
    words
}

/// Index of the program token in `words`, past leading env-assignments and
/// shell keywords (`FOO=bar`, `then`, `do`, `if`, …). `None` when the
/// segment carries no command of its own — a `for VAR in …` header.
fn command_base_index(words: &[String]) -> Option<usize> {
    let mut i = 0;
    while let Some(tok) = words.get(i) {
        if COMPOUND_HEADERS_NO_BODY.contains(&tok.as_str()) {
            return None;
        }
        if is_env_assignment(tok)
            || COMPOUND_KEYWORDS.contains(&tok.as_str())
            || COMPOUND_HEADERS_WITH_BODY.contains(&tok.as_str())
        {
            i += 1;
        } else {
            break;
        }
    }
    (i < words.len()).then_some(i)
}

/// Append the argument list (everything after the `git` program token) of
/// every git invocation reachable in `words`, looking through leading
/// env-assignments, launcher programs, and `sh -c "..."` wrappers.
fn collect_git_invocations(words: &[String], depth: u8, out: &mut Vec<Vec<String>>) {
    if depth == 0 {
        return;
    }
    let Some(base) = command_base_index(words) else {
        return;
    };
    let rest = &words[base + 1..];
    if base_is_git(&words[base]) {
        out.push(rest.to_vec());
        return;
    }
    let name = program_name(&words[base]);
    if GIT_SHELLS.contains(&name.as_str()) {
        for payload in shell_c_payloads(rest) {
            collect_git_invocations(&shell_words(&payload), depth - 1, out);
        }
        return;
    }
    if GIT_LAUNCHERS.contains(&name.as_str()) {
        // `env -S "git push"` / `env --split-string=...` re-splits its
        // string argument into a fresh command line, so re-parse it.
        if name == "env" {
            for payload in env_split_string_payloads(rest) {
                collect_git_invocations(&shell_words(&payload), depth - 1, out);
            }
        }
        for (i, tok) in rest.iter().enumerate() {
            if base_is_git(tok) {
                out.push(rest[i + 1..].to_vec());
            } else if GIT_SHELLS.contains(&program_name(tok).as_str()) {
                for payload in shell_c_payloads(&rest[i + 1..]) {
                    collect_git_invocations(&shell_words(&payload), depth - 1, out);
                }
            }
        }
    }
}

/// The command strings `env` runs via `-S` / `--split-string`, in the
/// separate-word, glued (`-S"git push"`), and `=`-joined forms. Each is a
/// command line `env` re-splits and runs, so it is re-parsed like a shell.
fn env_split_string_payloads(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(tok) = args.get(i) {
        if tok == "-S" || tok == "--split-string" {
            if let Some(payload) = args.get(i + 1) {
                out.push(payload.clone());
            }
            i += 2;
        } else if let Some(payload) = tok
            .strip_prefix("--split-string=")
            .or_else(|| tok.strip_prefix("-S").filter(|s| !s.is_empty()))
        {
            out.push(payload.to_string());
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Lower-cased program name with any directory prefix removed, so
/// `/usr/bin/env` and `env` compare equal against the launcher sets.
fn program_name(token: &str) -> String {
    token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .to_ascii_lowercase()
}

/// The command strings a shell runs via `-c`. `sh -c "git push"` yields
/// `["git push"]`.
fn shell_c_payloads(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-c" {
            if let Some(payload) = args.get(i + 1) {
                out.push(payload.clone());
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

/// Indices into `args` that git could dispatch as the subcommand. A
/// leading global option may or may not consume the next word as its
/// value; known value-taking options consume it, the `=` form does not,
/// and every other `-` token is explored both ways so a value-taking
/// option we don't know about cannot push the real verb out of view.
fn git_subcommand_candidates(args: &[String]) -> Vec<usize> {
    let mut seen = vec![false; args.len()];
    let mut out = Vec::new();
    let mut stack = vec![0usize];
    while let Some(i) = stack.pop() {
        if i >= args.len() || seen[i] {
            continue;
        }
        seen[i] = true;
        let tok = &args[i];
        if tok == "--" {
            stack.push(i + 1);
        } else if !tok.starts_with('-') || tok == "-" {
            out.push(i);
        } else if tok.contains('=') {
            stack.push(i + 1);
        } else if GIT_GLOBAL_VALUE_OPTIONS.contains(&tok.as_str()) {
            stack.push(i + 2);
        } else if GIT_GLOBAL_NOARG_OPTIONS.contains(&tok.as_str()) {
            stack.push(i + 1);
        } else {
            stack.push(i + 1);
            stack.push(i + 2);
        }
    }
    out
}

/// Whether one git invocation's argument list runs a destructive or
/// networked operation, recognised regardless of leading global options.
fn git_args_are_dangerous(args: &[String]) -> bool {
    if git_args_use_exec_capable_config(args) {
        return true;
    }
    git_subcommand_candidates(args)
        .into_iter()
        .any(|i| git_subcommand_is_dangerous(&args[i].to_ascii_lowercase(), &args[i + 1..]))
}

/// Whether a git subcommand (lower-cased) with the arguments that follow it
/// is one Sofos refuses to run for the model: it reaches the network,
/// rewrites history, or destroys working-tree state. File-recovery forms
/// (`git restore`, `git checkout -- <path>`) are intentionally absent so
/// the model can roll back a botched edit.
fn git_subcommand_is_dangerous(verb: &str, rest: &[String]) -> bool {
    let has = |flags: &[&str]| {
        rest.iter()
            .any(|t| flags.contains(&t.to_ascii_lowercase().as_str()))
    };
    let first = || rest.first().map(|s| s.to_ascii_lowercase());
    match verb {
        "push" | "pull" | "fetch" | "clone" | "clean" | "filter-branch" | "gc" | "prune"
        | "update-ref" | "send-email" | "apply" | "am" | "cherry-pick" | "revert" | "commit"
        | "merge" | "rebase" | "init" | "add" | "rm" | "mv" | "switch" | "submodule" | "daemon"
        | "instaweb" => true,
        "reset" => has(&["--hard", "--mixed"]),
        "checkout" => has(&["-f", "--force", "-b", "-B"]),
        "branch" => has(&["-d", "-D", "-m", "-M", "--delete", "--move"]),
        "tag" => has(&["-d", "--delete"]),
        "remote" => matches!(
            first().as_deref(),
            Some("add" | "set-url" | "remove" | "rm")
        ),
        "stash" => !matches!(first().as_deref(), Some("list" | "show")),
        _ => false,
    }
}

/// Whether the leading global options set an inline config key whose value
/// git executes as a command. Walks only the option run before the
/// subcommand; `-c`/`--config-env` carry the key, in either the
/// separate-word or `=`-joined form.
fn git_args_use_exec_capable_config(args: &[String]) -> bool {
    let mut i = 0;
    while let Some(tok) = args.get(i) {
        if tok == "--" || !tok.starts_with('-') || tok == "-" {
            return false;
        }
        if tok == "-c" || tok == "--config-env" {
            if args
                .get(i + 1)
                .is_some_and(|v| config_key_is_exec_capable(v))
            {
                return true;
            }
            i += 2;
            continue;
        }
        if let Some(key) = tok.strip_prefix("-c").filter(|k| !k.is_empty()) {
            if config_key_is_exec_capable(key) {
                return true;
            }
        }
        if let Some(key) = tok.strip_prefix("--config-env=") {
            if config_key_is_exec_capable(key) {
                return true;
            }
        }
        if !tok.contains('=') && GIT_GLOBAL_VALUE_OPTIONS.contains(&tok.as_str()) {
            i += 2;
        } else {
            i += 1;
        }
    }
    false
}

/// Whether a git config setting (`key` or `key=value`) runs code: an alias
/// or config include, a pager/editor/ssh/program/hook command, or any
/// per-driver hook key (`diff.<d>.command`, `filter.<d>.process`,
/// `trailer.<t>.command`, …). Matched by suffix so per-driver and
/// per-tool variants and new sections reusing these conventions are all
/// covered, rather than enumerating exact keys git keeps adding to.
fn config_key_is_exec_capable(setting: &str) -> bool {
    let (key, value) = match setting.split_once('=') {
        Some((k, v)) => (k.to_ascii_lowercase(), Some(v)),
        None => (setting.to_ascii_lowercase(), None),
    };
    // Config injection is dangerous whatever the value: it pulls in
    // arbitrary config that can define any of the command keys below.
    if key == "alias"
        || key.starts_with("alias.")
        || key == "include.path"
        || (key.starts_with("includeif.") && key.ends_with(".path"))
    {
        return true;
    }
    // The remaining keys name a command only when the value is a real
    // string. A boolean or empty value just toggles a setting and runs
    // nothing — `core.fsmonitor=true` selects git's built-in monitor,
    // `credential.helper=` clears helpers, `pager.log=false` disables
    // the pager — so those stay allowed.
    if value.is_none_or(is_git_boolean) {
        return false;
    }
    const EXEC_SUFFIXES: &[&str] = &[
        ".command",
        ".cmd",
        ".process",
        ".clean",
        ".smudge",
        ".textconv",
        ".helper",
        ".program",
        ".editor",
        ".sshcommand",
        ".fsmonitor",
        ".hookspath",
        ".external",
        ".askpass",
        ".packobjectshook",
        ".gitproxy",
    ];
    key == "core.pager"
        || key.starts_with("pager.")
        || EXEC_SUFFIXES.iter().any(|suffix| key.ends_with(suffix))
}

/// Whether a git config value is one of the boolean spellings, which only
/// toggle a setting and never name a command.
fn is_git_boolean(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "" | "true" | "false" | "yes" | "no" | "on" | "off" | "1" | "0"
    )
}

/// Argument lists of every git invocation in `command`, across all
/// compound-command segments. Each segment is tokenised quote-aware, then
/// followed through launchers and `sh -c` wrappers.
fn git_invocations(command: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for segment in PermissionManager::split_compound_command(command) {
        collect_git_invocations(&shell_words(&segment), GIT_NESTING_LIMIT, &mut out);
    }
    out
}

/// Whether every program the command runs is `git`. A confined command of
/// this kind may write the project's `.git` directory: git needs that for
/// `checkout`, `config`, `restore --staged`, and similar, and the git
/// gate has already vetted the operation. A command that also runs
/// anything else keeps `.git` read-only, so it cannot plant a hook there.
///
/// Fails closed: any segment whose base program is not plainly `git`,
/// including launcher wrappers such as `env git`, makes this false. The
/// caller only reaches it once the command is confinement-safe, so there
/// are no hidden subcommands ($(...), backticks) for a non-git program to
/// hide behind.
pub(super) fn command_runs_only_git(command: &str) -> bool {
    let mut saw_git = false;
    for segment in PermissionManager::split_compound_command(command) {
        let words = shell_words(&segment);
        let Some(base) = command_base_index(&words) else {
            continue;
        };
        if !base_is_git(&words[base]) {
            return false;
        }
        saw_git = true;
    }
    saw_git
}

/// The subcommand verb (lower-cased) of the first dangerous git invocation
/// in `command`, or `"config"` when the offence is an exec-capable inline
/// config option. Drives the wording of the rejection message.
fn first_dangerous_git_verb(command: &str) -> Option<String> {
    for args in git_invocations(command) {
        if git_args_use_exec_capable_config(&args) {
            return Some("config".to_string());
        }
        for i in git_subcommand_candidates(&args) {
            let verb = args[i].to_ascii_lowercase();
            if git_subcommand_is_dangerous(&verb, &args[i + 1..]) {
                return Some(verb);
            }
        }
    }
    None
}

/// Whether `command` runs `git checkout` in a form that should prompt the
/// user — branch switches and HEAD detaches that mutate the working tree
/// without being destructive enough to hard-deny. Sees through global
/// options, quotes, launchers, and `sh -c`, and matches the exact
/// `checkout` subcommand so plumbing such as `git checkout-index` does not
/// over-trigger the prompt.
pub(super) fn command_contains_askable_git_checkout(command: &str) -> bool {
    git_invocations(command).iter().any(|args| {
        git_subcommand_candidates(args)
            .into_iter()
            .any(|i| args[i].eq_ignore_ascii_case("checkout"))
    })
}

/// Whether `token` is the `git` program however bash would spell it. The
/// quote and backslash characters bash strips while expanding a word are
/// removed — so `g\it`, `g""it`, `g'i't`, `\git`, and `'git'` all reduce
/// to `git` — subshell and group delimiters are dropped, and any
/// directory prefix is removed before the comparison. Backslash is a
/// bash escape here, not a path separator: the shell that runs these
/// commands is sh or bash on every platform sofos supports, so a
/// backslash is removed rather than split on.
fn base_is_git(token: &str) -> bool {
    let bare: String = token
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\' | '(' | ')' | '{' | '}'))
        .collect();
    bare.rsplit('/')
        .next()
        .unwrap_or(&bare)
        .eq_ignore_ascii_case("git")
}

/// Returns the kind of expansion that would change the path between
/// our deny check and the shell touching it: `$` / `${...}`, backticks,
/// `~user`, or glob metacharacters (`?`, `*`, `[`, `{`). Plain `~/`
/// and bare `~` are allowed because they expand to a known prefix and
/// are handled by the tilde-expansion helper.
///
/// The Windows verbatim-path prefixes `\\?\` and `\\.\` are stripped
/// before the glob check so a canonical Windows path is not mistaken
/// for a glob just because the prefix contains `?`.
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
    let body = strip_windows_verbatim_prefix(tok);
    if body.contains('?') || body.contains('*') || body.contains('[') || body.contains('{') {
        return Some("glob expansion");
    }
    None
}

/// Strip the Windows verbatim-path prefix (`\\?\`, `\\.\`, or the
/// forward-slash equivalents) when present so the remainder can be
/// inspected without the prefix's `?` or `.` being read as shell
/// metacharacters.
fn strip_windows_verbatim_prefix(tok: &str) -> &str {
    for prefix in [r"\\?\", r"\\.\", "//?/", "//./"] {
        if let Some(rest) = tok.strip_prefix(prefix) {
            return rest;
        }
    }
    tok
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

        let grant_dir = grant_dir_for_path(std::path::Path::new(&check_path));

        // Non-interactive mode (tests, piped input): deny with a config hint
        if !self.interactive {
            return Err(SofosError::ToolExecution(format!(
                "Command references path '{}' outside workspace\n\
                 Hint: Add Bash({}/**) to 'allow' list in .sofos/config.local.toml",
                check_path, grant_dir
            )));
        }

        // Ask user interactively
        let (allowed, remember) = permission_manager.ask_user_path_permission("Bash", grant_dir)?;

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

            // A canonical Windows path like `\\?\C:\file.txt` is absolute
            // but does not start with `/`, `.` or `~`; treat it as path-
            // shaped so the read-deny check fires on it too.
            let path_shaped = cleaned.contains('/')
                || cleaned.starts_with('.')
                || cleaned.starts_with('~')
                || is_absolute_path(cleaned);

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
        // `git$IFS\tpush`, `git\\\npush` are seen as `git push`. Case is
        // preserved so the git scan can tell `-C` (change directory) from
        // `-c` (set config).
        if !self.is_safe_git_command(&normalize_command_whitespace(command)) {
            return false;
        }

        true
    }

    /// Whether `command` is free of dangerous git operations. Every git
    /// invocation it reaches — directly, behind a launcher such as `env` or
    /// `timeout`, or inside `sh -c "..."` — is parsed into its real
    /// subcommand (skipping leading global options however they are written)
    /// and rejected if it pushes, rewrites history, destroys the working
    /// tree, or sets an inline config value git would execute.
    pub(super) fn is_safe_git_command(&self, command: &str) -> bool {
        !git_invocations(command)
            .iter()
            .any(|args| git_args_are_dangerous(args))
    }

    pub(super) fn get_rejection_reason(&self, command: &str) -> String {
        let matcher_input = normalize_command_whitespace(command);

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
        // Phrase the reason from the same subcommand the gate flagged, so
        // the message lines up with the rejection however the command was
        // written (global options, quotes, launchers, `sh -c`).
        let verb = first_dangerous_git_verb(&normalize_command_whitespace(command));
        match verb.as_deref() {
            Some("push") => format!(
                "Command '{command}' blocked: 'git push' sends data to remote repositories\n\
                 Hint: Use 'git status', 'git log', 'git diff' to view changes."
            ),
            Some(op @ ("pull" | "fetch")) => format!(
                "Command '{command}' blocked: 'git {op}' fetches data from remote repositories\n\
                 Hint: Use 'git status', 'git log', 'git diff' to view local changes."
            ),
            Some("clone") => format!(
                "Command '{command}' blocked: 'git clone' downloads repositories\n\
                 Hint: Clone repositories manually outside of Sofos."
            ),
            Some(op @ ("commit" | "add")) => format!(
                "Command '{command}' blocked: 'git {op}' modifies the git repository\n\
                 Hint: Use 'git status', 'git diff' to view changes. Create commits manually."
            ),
            Some(op @ ("reset" | "clean")) => format!(
                "Command '{command}' blocked: 'git {op}' is a destructive operation\n\
                 Hint: Use 'git status', 'git log', 'git diff' to view repository state."
            ),
            Some(op @ ("checkout" | "switch")) => format!(
                "Command '{command}' blocked: 'git {op}' changes branches or modifies the working directory\n\
                 Hint: Use 'git branch' to list branches, 'git status' to see the current branch."
            ),
            Some(op @ ("merge" | "rebase")) => format!(
                "Command '{command}' blocked: 'git {op}' modifies git history\n\
                 Hint: Perform merges/rebases manually outside of Sofos."
            ),
            Some("stash") => format!(
                "Command '{command}' blocked: 'git stash' modifies repository state\n\
                 Hint: Use 'git stash list' or 'git stash show' to view stashed changes."
            ),
            Some("remote") => format!(
                "Command '{command}' blocked: modifying git remotes is not allowed\n\
                 Hint: Use 'git remote -v' to view configured remotes."
            ),
            Some("submodule") => format!(
                "Command '{command}' blocked: 'git submodule' can fetch from remote repositories\n\
                 Hint: Manage submodules manually outside of Sofos."
            ),
            Some("config") => format!(
                "Command '{command}' blocked: a '-c'/'--config-env' option sets a git config value that runs an external command\n\
                 Hint: Run the git command without the inline config override."
            ),
            _ => format!(
                "Command '{command}' blocked: git operation modifies repository or accesses network\n\
                 Hint: Allowed git commands: status, log, diff, show, branch, remote -v, grep, blame"
            ),
        }
    }
}
