//! `Bash(...)` / `Read(...)` / `Write(...)` rule-string parsing and
//! shape helpers. Every entry the permission system reads from
//! `config.local.toml` is normalised through these functions before
//! pattern matching, so a future change to the rule syntax has one
//! place to land.

use crate::tools::permissions::PermissionManager;
use crate::tools::utils::is_absolute_path;

/// Bare `"Bash"` in an `allow` or `deny` list acts as a blanket rule
/// over all bash commands. In `allow` it auto-passes everything except
/// the built-in forbidden set (`rm`, `chmod`, `sudo`, …); in `deny` it
/// auto-rejects everything. Deny beats allow when both lists contain
/// the blanket entry.
pub(super) const BLANKET_BASH: &str = "Bash";

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

    pub(super) fn normalize_command(command: &str) -> String {
        format!("Bash({})", command.trim())
    }

    pub(super) fn normalize_read(path: &str) -> String {
        format!("Read({})", path.trim())
    }

    pub(super) fn normalize_write(path: &str) -> String {
        format!("Write({})", path.trim())
    }
}
