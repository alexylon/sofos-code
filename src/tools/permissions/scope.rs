//! Read / Write / Bash-path scope matching and the
//! `extract_read_pattern` / `extract_write_pattern` rule shapers. Each
//! `check_*_permission` walks the four-tier priority — exact deny,
//! exact allow, glob deny, glob allow — and falls back to `Allowed`
//! when nothing matches. The generic helpers
//! ([`PermissionManager::check_scope_permission`],
//! [`PermissionManager::is_scope_explicit_allow`]) factor the
//! candidate-path normalisation (`./` prefix variants, tilde expansion)
//! so the Read/Write/Bash callers stay in lockstep.

use crate::tools::permissions::{CommandPermission, PermissionManager};
use crate::tools::utils::lexically_normalize;
use globset::GlobSet;
use std::path::PathBuf;

/// Candidate paths fed to the deny/allow matchers. Includes the raw
/// trimmed input, the `./`-stripped and `./`-prefixed shapes, and the
/// lexically normalized form of each so noise variants collapse to the
/// canonical text. Inputs containing a `..` segment skip the raw
/// shapes — the shell resolves `..` before touching the file, so
/// `./secrets/../allowed.txt` must not match `./secrets/**` on the
/// string-only path.
fn build_candidates(trimmed: &str) -> Vec<String> {
    let stripped = trimmed.strip_prefix("./").unwrap_or(trimmed);
    let with_prefix = if stripped.starts_with("./") {
        stripped.to_string()
    } else {
        format!("./{}", stripped)
    };
    let has_parent_segment = trimmed.split(['/', '\\']).any(|seg| seg == "..");
    let mut out: Vec<String> = Vec::with_capacity(8);
    let bases: [&str; 3] = [trimmed, stripped, with_prefix.as_str()];
    for b in bases {
        if !has_parent_segment && !out.iter().any(|s| s == b) {
            out.push(b.to_string());
        }
        let n = lexically_normalize(&PathBuf::from(b))
            .to_string_lossy()
            .to_string();
        if !n.is_empty() && !out.contains(&n) {
            let needs_prefix =
                !n.starts_with('/') && !n.starts_with("./") && !is_windows_absolute(&n);
            let prefixed = if needs_prefix {
                Some(format!("./{}", n))
            } else {
                None
            };
            out.push(n);
            if let Some(p) = prefixed {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    if out.is_empty() {
        out.push(trimmed.to_string());
    }
    out
}

/// Whether a fetch `host` is covered by a `domain` grant — a
/// `WebFetch(domain:...)` rule or a host allowed earlier in the session:
/// the host itself, or any subdomain of it on a label boundary, so
/// `rust-lang.org` covers `blog.rust-lang.org` but not `evilrust-lang.org`.
/// Both sides are expected lower-cased by the caller.
pub(super) fn web_fetch_host_matches(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .and_then(|prefix| prefix.strip_suffix('.'))
            .is_some()
}

/// Windows-shaped absolute path: drive letter (`C:\foo`) or UNC
/// (`\\server\share`).
fn is_windows_absolute(s: &str) -> bool {
    let bytes = s.as_bytes();
    (bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/'))
        || s.starts_with("\\\\")
}

impl PermissionManager {
    pub(super) fn extract_read_pattern(entry: &str) -> Option<&str> {
        let trimmed = entry.trim();
        if let Some(rest) = trimmed.strip_prefix("Read(") {
            if let Some(end) = rest.rfind(')') {
                return Some(&rest[..end]);
            }
        }
        None
    }

    pub(super) fn extract_write_pattern(entry: &str) -> Option<&str> {
        let trimmed = entry.trim();
        if let Some(rest) = trimmed.strip_prefix("Write(") {
            if let Some(end) = rest.rfind(')') {
                return Some(&rest[..end]);
            }
        }
        None
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

    /// Whether any `Read(...)` deny rule is configured, from the global or
    /// local config. The sandbox enforces these as kernel-level read masks,
    /// so a caller can refuse to drop the sandbox while any are active —
    /// nothing else reliably keeps an arbitrary command off a denied path.
    pub fn has_read_deny_rules(&self) -> bool {
        self.settings
            .permissions
            .deny
            .iter()
            .any(|entry| Self::extract_read_pattern(entry).is_some())
    }

    /// The configured `Write(...)` deny patterns, for translating into
    /// kernel-level write protection inside the workspace.
    pub fn sandbox_write_deny_rules(&self) -> Vec<String> {
        self.settings
            .permissions
            .deny
            .iter()
            .filter_map(|entry| Self::extract_write_pattern(entry))
            .map(str::to_string)
            .collect()
    }

    /// The configured `Read(...)` deny and allow patterns, for translating
    /// into kernel-level sandbox read rules.
    pub fn sandbox_read_rules(&self) -> (Vec<String>, Vec<String>) {
        let patterns = |entries: &[String]| -> Vec<String> {
            entries
                .iter()
                .filter_map(|e| Self::extract_read_pattern(e))
                .map(str::to_string)
                .collect()
        };
        (
            patterns(&self.settings.permissions.deny),
            patterns(&self.settings.permissions.allow),
        )
    }

    /// Resolve whether `web_fetch` may reach `host` from the configured
    /// rules: a matching `WebFetch(domain:...)` deny wins, then a matching
    /// allow, otherwise `Ask` so the caller prompts. The host is matched
    /// case-insensitively against each rule's domain, exact or subdomain.
    pub fn check_web_fetch_permission(&self, host: &str) -> CommandPermission {
        let host = Self::canonical_web_fetch_host(host);
        let any_match = |entries: &[String]| {
            entries
                .iter()
                .filter_map(|entry| Self::extract_web_fetch_domain(entry))
                .any(|domain| web_fetch_host_matches(&host, &domain))
        };
        if any_match(&self.settings.permissions.deny) {
            CommandPermission::Denied
        } else if any_match(&self.settings.permissions.allow) {
            CommandPermission::Allowed
        } else {
            CommandPermission::Ask
        }
    }

    /// Resolve whether the model may call tools from MCP `server` from the
    /// configured rules: a matching `Mcp(...)` deny wins, then a matching
    /// allow, otherwise `Ask` so the caller prompts. The server name is
    /// matched case-insensitively against each rule.
    pub fn check_mcp_permission(&self, server: &str) -> CommandPermission {
        let server = Self::canonical_mcp_server(server);
        let any_match = |entries: &[String]| {
            entries
                .iter()
                .filter_map(|entry| Self::extract_mcp_server(entry))
                .any(|s| s == server)
        };
        if any_match(&self.settings.permissions.deny) {
            CommandPermission::Denied
        } else if any_match(&self.settings.permissions.allow) {
            CommandPermission::Allowed
        } else {
            CommandPermission::Ask
        }
    }

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

    pub fn is_write_explicit_allow(&self, path: &str) -> bool {
        self.is_scope_explicit_allow(path, Self::normalize_write, &self.write_allow_set)
    }

    /// Check if a path is covered by a Bash(/path/**) grant
    pub fn is_bash_path_allowed(&self, path: &str) -> bool {
        self.is_scope_explicit_allow(path, Self::normalize_command, &self.bash_path_allow_set)
    }

    /// Check if a path is blocked by a Bash(/path/**) deny rule
    pub fn is_bash_path_denied(&self, path: &str) -> bool {
        let expanded = Self::expand_tilde(path);
        let trimmed = expanded.trim();
        let candidates = build_candidates(trimmed);

        for candidate in &candidates {
            let normalized = Self::normalize_command(candidate);
            if self.settings.permissions.deny.contains(&normalized) {
                return true;
            }
        }

        for candidate in &candidates {
            if !self.bash_path_deny_set.is_empty()
                && self.bash_path_deny_set.is_match(candidate.as_str())
            {
                return true;
            }
        }

        false
    }

    pub(super) fn check_scope_permission(
        &self,
        path: &str,
        normalize_fn: fn(&str) -> String,
        allow_set: &GlobSet,
        deny_set: &GlobSet,
    ) -> CommandPermission {
        let expanded = Self::expand_tilde(path);
        let trimmed = expanded.trim();
        let candidates = build_candidates(trimmed);

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
            if !deny_set.is_empty() && deny_set.is_match(candidate.as_str()) {
                return CommandPermission::Denied;
            }
        }

        for candidate in &candidates {
            if !allow_set.is_empty() && allow_set.is_match(candidate.as_str()) {
                return CommandPermission::Allowed;
            }
        }

        // Default-Allowed when no explicit rule matches. Inside-workspace
        // reads are expected to pass through; paths that leave the
        // workspace are gated separately by
        // `BashExecutor::check_bash_external_paths`, which runs the
        // interactive prompt and the deny-glob check on its own. Do NOT
        // collapse these two paths without preserving that boundary —
        // the bash side enforces explicit confirmation for external
        // paths, which this default branch would skip.
        CommandPermission::Allowed
    }

    pub(super) fn is_scope_explicit_allow(
        &self,
        path: &str,
        normalize_fn: fn(&str) -> String,
        allow_set: &GlobSet,
    ) -> bool {
        let expanded = Self::expand_tilde(path);
        let trimmed = expanded.trim();
        let candidates = build_candidates(trimmed);

        for candidate in &candidates {
            let normalized = normalize_fn(candidate);
            if self.settings.permissions.allow.contains(&normalized) {
                return true;
            }
            if !allow_set.is_empty() && allow_set.is_match(candidate.as_str()) {
                return true;
            }
        }

        false
    }
}
