//! Workspace and external path resolution for [`ToolExecutor`]. Returns
//! [`ResolvedPath`] — canonical [`PathBuf`] + canonical string +
//! inside-workspace flag — every filesystem-touching dispatcher in
//! `tools::mod` needs.

use crate::error::{Result, SofosError};
use crate::tools::ToolExecutor;
use crate::tools::permissions;
use crate::tools::utils::is_absolute_or_tilde;
use std::path::{Component, Path, PathBuf};

/// Collapse `.` and `..` components in `p` lexically, without touching
/// the filesystem. `..` pops the previous Normal component but never
/// pops the prefix or root, so an over-popping path like `../../etc`
/// keeps its leading `..` (and is therefore *not* classified as inside
/// any workspace by `starts_with`). Used in the resolve fallback when
/// we couldn't find any existing ancestor to canonicalise against and
/// have to classify a purely lexical path.
fn lexically_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(c.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let last_is_normal = out
                    .components()
                    .next_back()
                    .map(|c| matches!(c, Component::Normal(_)))
                    .unwrap_or(false);
                if last_is_normal {
                    out.pop();
                } else {
                    out.push(Component::ParentDir.as_os_str());
                }
            }
        }
    }
    out
}

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
                        // No existing ancestor found, or the cursor ends
                        // in a `..`/`.` component that `file_name` can't
                        // name. Classify the path lexically rather than
                        // returning it raw: an earlier version used
                        // `!is_absolute_or_tilde(caller_path)` to decide
                        // inside-workspace, which mis-classified a
                        // workspace-relative `../../etc/passwd` as
                        // inside the workspace.
                        let normalized = lexically_normalize(&full_path);
                        let is_inside_workspace = normalized.starts_with(self.fs_tool.workspace());
                        let canonical_str = normalized.to_string_lossy().to_string();
                        return Ok(ResolvedPath {
                            canonical: normalized,
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

#[cfg(test)]
mod tests {
    use super::lexically_normalize;
    use std::path::PathBuf;

    #[test]
    fn lexically_normalize_collapses_current_dir() {
        assert_eq!(
            lexically_normalize(&PathBuf::from("/tmp/./workspace/./file")),
            PathBuf::from("/tmp/workspace/file")
        );
    }

    #[test]
    fn lexically_normalize_collapses_parent_dir() {
        assert_eq!(
            lexically_normalize(&PathBuf::from("/tmp/workspace/foo/../bar")),
            PathBuf::from("/tmp/workspace/bar")
        );
    }

    #[test]
    fn lexically_normalize_escapes_workspace_via_double_dot() {
        // Workspace-relative `../../etc/passwd` joined onto a workspace
        // produces a path that lexically resolves above the workspace.
        // The normalised form must NOT start with the workspace prefix,
        // which is the property `is_inside_workspace` relies on in the
        // resolve fallback.
        let workspace = PathBuf::from("/home/user/project");
        let joined = workspace.join("../../etc/passwd");
        let normalized = lexically_normalize(&joined);
        assert_eq!(normalized, PathBuf::from("/home/etc/passwd"));
        assert!(!normalized.starts_with(&workspace));
    }

    #[test]
    fn lexically_normalize_keeps_leading_parent_when_over_popping() {
        // Without a root or normal anchor, `..` components must survive
        // so a relative escape stays visibly outside any anchored
        // workspace.
        assert_eq!(
            lexically_normalize(&PathBuf::from("../../etc")),
            PathBuf::from("../../etc")
        );
    }
}
