//! Workspace and external path resolution for [`ToolExecutor`]. Returns
//! [`ResolvedPath`] — canonical [`PathBuf`] + canonical string +
//! inside-workspace flag — every filesystem-touching dispatcher in
//! `tools::mod` needs.

use crate::error::{Result, SofosError};
use crate::tools::ToolExecutor;
use crate::tools::permissions;
use crate::tools::utils::is_absolute_or_tilde;

/// Path resolved by [`ToolExecutor::resolve_existing`] or
/// [`ToolExecutor::resolve_for_write`]. Carries the three pieces of data
/// every filesystem-touching dispatcher needs: the canonical `PathBuf`
/// for the operation itself, the canonical string form for permission
/// checks, and whether the target lives inside the workspace (drives
/// the "inside FS tool / outside direct-std::fs" branch).
pub struct ResolvedPath {
    pub canonical: std::path::PathBuf,
    pub canonical_str: String,
    pub is_inside_workspace: bool,
}

impl ToolExecutor {
    /// Canonicalise a caller-supplied path against the workspace,
    /// classifying it as inside / outside the workspace for downstream
    /// permission and filesystem-routing decisions.
    ///
    /// When `must_exist` is true, the path must already exist on disk
    /// (read-side: `FileNotFound` otherwise). When false, the path may
    /// not exist yet — we walk up until an existing ancestor is found,
    /// canonicalise that, and re-append the missing tail. Walking is
    /// necessary because `canonicalize` requires every component to
    /// exist, but a write may legitimately target a path whose parent
    /// directories exist yet. An earlier implementation only canonicalised
    /// the immediate parent, so it fell through to an un-canonicalised
    /// path whenever the grandparent was missing too.
    fn resolve(&self, caller_path: &str, must_exist: bool) -> Result<ResolvedPath> {
        let full_path = if is_absolute_or_tilde(caller_path) {
            std::path::PathBuf::from(permissions::PermissionManager::expand_tilde_pub(
                caller_path,
            ))
        } else {
            self.fs_tool.workspace().join(caller_path)
        };

        let canonical = if must_exist {
            std::fs::canonicalize(&full_path)
                .map_err(|_| SofosError::FileNotFound(caller_path.to_string()))?
        } else {
            // Walk up collecting missing components until an existing
            // ancestor is found. `cursor.exists()` follows symlinks, same
            // as `canonicalize` below, so the two stay consistent.
            let mut missing_tail: Vec<std::ffi::OsString> = Vec::new();
            let mut cursor = full_path.as_path();
            let canonical_anchor = loop {
                if cursor.exists() {
                    break std::fs::canonicalize(cursor).map_err(|e| {
                        SofosError::ToolExecution(format!("Failed to resolve path: {}", e))
                    })?;
                }
                match (cursor.file_name(), cursor.parent()) {
                    (Some(name), Some(parent)) => {
                        missing_tail.push(name.to_os_string());
                        cursor = parent;
                    }
                    _ => {
                        // Reached the filesystem root (or a path without
                        // a file_name — empty / ending in `..`) without
                        // finding an existing ancestor. Fall back to the
                        // un-canonicalised path rather than erroring out.
                        let is_inside_workspace = !is_absolute_or_tilde(caller_path);
                        let canonical_str = full_path.to_string_lossy().to_string();
                        return Ok(ResolvedPath {
                            canonical: full_path,
                            canonical_str,
                            is_inside_workspace,
                        });
                    }
                }
            };

            let mut canonical = canonical_anchor;
            for name in missing_tail.iter().rev() {
                canonical.push(name);
            }
            canonical
        };

        let is_inside_workspace = canonical.starts_with(self.fs_tool.workspace());
        let canonical_str = canonical.to_string_lossy().to_string();
        Ok(ResolvedPath {
            canonical,
            canonical_str,
            is_inside_workspace,
        })
    }

    /// Read-side resolve: the path must already exist on disk. Returns
    /// `FileNotFound` otherwise. Thin wrapper around [`Self::resolve`].
    pub(super) fn resolve_existing(&self, caller_path: &str) -> Result<ResolvedPath> {
        self.resolve(caller_path, true)
    }

    /// Write-side resolve: the path may not exist yet. Walks up to find
    /// an existing ancestor, canonicalises it, and re-appends the
    /// missing tail. Thin wrapper around [`Self::resolve`].
    pub(super) fn resolve_for_write(&self, caller_path: &str) -> Result<ResolvedPath> {
        self.resolve(caller_path, false)
    }
}
