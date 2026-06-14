//! Workspace identifier allocation for the Windows sandbox.
//!
//! Sofos persists one identifier per workspace under `.sofos/cap_sid`
//! so the write rule added to the workspace stays meaningful across
//! restarts. The file holds a single identifier string; the workspace
//! permission list is the source of truth and the file is a cache.

use rand::RngExt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Return the workspace identifier, generating and persisting it on
/// first use.
pub fn workspace_cap_sid(workspace: &Path) -> io::Result<String> {
    let path = cap_sid_file(workspace);
    if let Ok(existing) = fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let sid = make_random_cap_sid_string();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, &sid)?;
    Ok(sid)
}

fn cap_sid_file(workspace: &Path) -> PathBuf {
    workspace.join(".sofos").join("cap_sid")
}

/// Build a random identifier in `S-1-5-21-<a>-<b>-<c>-<d>` format. The
/// kernel only compares identifiers for equality, so a strong random
/// source is unnecessary.
fn make_random_cap_sid_string() -> String {
    let mut rng = rand::rng();
    let a: u32 = rng.random_range(0..=u32::MAX);
    let b: u32 = rng.random_range(0..=u32::MAX);
    let c: u32 = rng.random_range(0..=u32::MAX);
    let d: u32 = rng.random_range(0..=u32::MAX);
    format!("S-1-5-21-{a}-{b}-{c}-{d}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_cap_sid_persists_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let first = workspace_cap_sid(dir.path()).unwrap();
        let second = workspace_cap_sid(dir.path()).unwrap();
        assert_eq!(first, second);
        assert!(first.starts_with("S-1-5-21-"));
    }

    #[test]
    fn workspace_cap_sid_is_unique_per_workspace() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let sid_a = workspace_cap_sid(a.path()).unwrap();
        let sid_b = workspace_cap_sid(b.path()).unwrap();
        assert_ne!(sid_a, sid_b);
    }
}
