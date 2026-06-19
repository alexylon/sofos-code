//! `Bash(...)` / `Read(...)` / `Write(...)` rule-string parsing and
//! shape helpers. Every entry the permission system reads from
//! `config.local.toml` is normalised through these functions before
//! pattern matching, so a future change to the rule syntax has one
//! place to land.

use crate::tools::permissions::PermissionManager;
use crate::tools::utils::{is_absolute_path, normalize_command_whitespace};

/// Bare `"Bash"` in an `allow` or `deny` list acts as a blanket rule
/// over all bash commands. In `allow` it auto-passes everything except
/// the built-in forbidden set (`rm`, `chmod`, `sudo`, …); in `deny` it
/// auto-rejects everything. Deny beats allow when both lists contain
/// the blanket entry.
pub(super) const BLANKET_BASH: &str = "Bash";

/// Scope token for the `WebFetch(domain:<host>)` rule that gates the
/// `web_fetch` tool's network access by host.
pub(super) const WEB_FETCH_SCOPE: &str = "WebFetch";

/// Qualifier inside a `WebFetch(...)` rule that selects host matching.
/// Rules are written with this prefix; it is optional when reading, so a
/// hand-edited `WebFetch(example.com)` is accepted as well.
pub(super) const WEB_FETCH_DOMAIN_PREFIX: &str = "domain:";

impl PermissionManager {
    /// Extract path patterns from Bash() entries.
    /// Only treats entries as path grants if the content is absolute or
    /// a tilde path AND contains a glob char. Plain commands like
    /// Bash(npm test) are not path patterns. `is_absolute_path` handles
    /// Unix (`/tmp/**`), Windows drive-letter (`C:\Users\**`), and UNC
    /// paths on every platform — using `Path::is_absolute` alone would
    /// mis-classify a Unix-style `Bash(/var/log/**)` config entry as a
    /// command pattern when the binary runs on Windows.
    pub(super) fn extract_bash_path_pattern(entry: &str) -> Option<&str> {
        let trimmed = entry.trim();
        if let Some(rest) = trimmed.strip_prefix("Bash(") {
            if let Some(end) = rest.rfind(')') {
                let content = &rest[..end];
                let looks_like_path = content.starts_with('~') || is_absolute_path(content);
                if looks_like_path && content.contains('*') {
                    return Some(content);
                }
            }
        }
        None
    }

    /// `Bash(...)` wrapper that preserves internal whitespace. Used by
    /// path-grant lookups so a filename with legitimate multi-whitespace
    /// matches its config entry verbatim.
    pub fn normalize_command(command: &str) -> String {
        format!("Bash({})", command.trim())
    }

    /// Command-key variant of [`Self::normalize_command`]: collapses
    /// internal whitespace so the same logical command hashes to one
    /// rule key regardless of spacing. Use this for command lookups,
    /// not paths.
    pub fn normalize_command_key(command: &str) -> String {
        let collapsed = normalize_command_whitespace(command);
        format!("Bash({})", collapsed.trim())
    }

    pub(super) fn normalize_read(path: &str) -> String {
        format!("Read({})", path.trim())
    }

    pub(super) fn normalize_write(path: &str) -> String {
        format!("Write({})", path.trim())
    }

    /// Canonical comparison form for a web-fetch host or rule domain:
    /// trimmed, with any trailing FQDN dot removed (`example.com.` and
    /// `example.com` are the same host, the way DNS and the HTTP client
    /// treat them), and lower-cased. Applied to both the fetched host and
    /// the rule domain so the two match regardless of a trailing dot or
    /// letter case — and so a trailing dot cannot slip a request past a
    /// deny rule.
    pub(super) fn canonical_web_fetch_host(host: &str) -> String {
        host.trim().trim_end_matches('.').to_ascii_lowercase()
    }

    /// Extract the host from a `WebFetch(domain:<host>)` rule in canonical
    /// form (see [`Self::canonical_web_fetch_host`]). The `domain:`
    /// qualifier is optional on read, so a bare `WebFetch(<host>)` is
    /// accepted too. Returns `None` for any entry that is not a web-fetch
    /// rule or whose host is empty.
    pub(super) fn extract_web_fetch_domain(entry: &str) -> Option<String> {
        let trimmed = entry.trim();
        let inner = trimmed
            .strip_prefix(WEB_FETCH_SCOPE)?
            .strip_prefix('(')?
            .strip_suffix(')')?
            .trim();
        let host = Self::canonical_web_fetch_host(
            inner.strip_prefix(WEB_FETCH_DOMAIN_PREFIX).unwrap_or(inner),
        );
        (!host.is_empty()).then_some(host)
    }

    /// The canonical rule string persisted and looked up for a fetch
    /// `host`: always the `domain:` form, in canonical host form.
    pub(super) fn normalize_web_fetch(host: &str) -> String {
        format!(
            "{WEB_FETCH_SCOPE}({WEB_FETCH_DOMAIN_PREFIX}{})",
            Self::canonical_web_fetch_host(host)
        )
    }
}
