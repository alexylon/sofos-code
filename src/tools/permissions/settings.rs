//! On-disk shapes for the permission system. [`PermissionSettings`]
//! round-trips through `.sofos/config.local.toml` and
//! `.sofos/config.toml`; the [`merge`] helper combines the two so the
//! local file overrides the global one on conflict but every unique
//! entry from both ends up in the merged result.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PermissionSettings {
    // Defaults so a config with no [permissions] section (e.g. MCP-only)
    // still loads instead of failing on the missing field.
    #[serde(default)]
    pub permissions: Permissions,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Permissions {
    // Every list defaults to empty, so a [permissions] section that sets
    // only one of them still parses instead of failing the whole config
    // load.
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
}

impl PermissionSettings {
    /// Merge two permission settings, with `other` (local) winning ties
    /// inside the same list. Lists are deduplicated independently so a
    /// rule in the local-allow list cannot silently strip a matching
    /// rule from the global-deny list: deny / allow / ask are union-only
    /// across files, and intra-list duplicates are removed only against
    /// other entries on the same list.
    pub(super) fn merge(&mut self, other: Self) {
        let merge_list = |theirs: &[String], mine: &[String]| -> Vec<String> {
            let mut seen: HashSet<String> = theirs.iter().cloned().collect();
            let mut merged: Vec<String> = theirs.to_vec();
            for entry in mine {
                if seen.insert(entry.clone()) {
                    merged.push(entry.clone());
                }
            }
            merged
        };

        let merged_allow = merge_list(&other.permissions.allow, &self.permissions.allow);
        let merged_deny = merge_list(&other.permissions.deny, &self.permissions.deny);
        let merged_ask = merge_list(&other.permissions.ask, &self.permissions.ask);

        self.permissions.allow = merged_allow;
        self.permissions.deny = merged_deny;
        self.permissions.ask = merged_ask;
    }
}
