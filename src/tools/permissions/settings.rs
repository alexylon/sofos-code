//! On-disk shapes for the permission system. [`PermissionSettings`]
//! round-trips through `.sofos/config.local.toml` and
//! `.sofos/config.toml`; the [`merge`] helper combines the two so the
//! local file overrides the global one on conflict but every unique
//! entry from both ends up in the merged result.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionSettings {
    pub permissions: Permissions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Permissions {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
}

impl Default for PermissionSettings {
    fn default() -> Self {
        Self {
            permissions: Permissions {
                allow: Vec::new(),
                deny: Vec::new(),
                ask: Vec::new(),
            },
        }
    }
}

impl PermissionSettings {
    /// Merge two permission settings, with other taking precedence for conflicts
    pub(super) fn merge(&mut self, other: Self) {
        let mut seen = HashSet::new();

        for entry in &other.permissions.allow {
            seen.insert(entry.clone());
        }
        for entry in &other.permissions.deny {
            seen.insert(entry.clone());
        }
        for entry in &other.permissions.ask {
            seen.insert(entry.clone());
        }

        let mut merged_allow = other.permissions.allow.clone();
        for entry in &self.permissions.allow {
            if !seen.contains(entry) {
                merged_allow.push(entry.clone());
            }
        }

        let mut merged_deny = other.permissions.deny.clone();
        for entry in &self.permissions.deny {
            if !seen.contains(entry) {
                merged_deny.push(entry.clone());
            }
        }

        let mut merged_ask = other.permissions.ask.clone();
        for entry in &self.permissions.ask {
            if !seen.contains(entry) {
                merged_ask.push(entry.clone());
            }
        }

        self.permissions.allow = merged_allow;
        self.permissions.deny = merged_deny;
        self.permissions.ask = merged_ask;
    }
}
