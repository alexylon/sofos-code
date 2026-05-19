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

    #[allow(dead_code)]
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

    #[allow(dead_code)]
    pub fn is_read_explicit_allow_both_forms(&self, original: &str, canonical: &str) -> bool {
        self.is_read_explicit_allow(original) || self.is_read_explicit_allow(canonical)
    }

    pub fn is_write_explicit_allow(&self, path: &str) -> bool {
        self.is_scope_explicit_allow(path, Self::normalize_write, &self.write_allow_set)
    }

    #[allow(dead_code)]
    pub fn is_write_explicit_allow_both_forms(&self, original: &str, canonical: &str) -> bool {
        self.is_write_explicit_allow(original) || self.is_write_explicit_allow(canonical)
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
