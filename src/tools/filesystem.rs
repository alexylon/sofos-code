use crate::error::{Result, SofosError};
use crate::error_ext::ResultExt;
use crate::tools::utils::{MAX_TOOL_OUTPUT_TOKENS, TruncationKind, truncate_for_context};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024; // 50MB limit

/// Write `content` to `path` atomically: stage a sibling `<name>.sofos.tmp`
/// first, then rename it over the destination. On the same filesystem
/// `rename` is a single inode swap, so a crash / OOM / interrupt partway
/// through the write leaves the original file intact instead of corrupting
/// it with a half-written replacement. If staging or renaming fails, the
/// temp file is best-effort cleaned up so a stray `.sofos.tmp` doesn't
/// accumulate next to the real file.
///
/// When `path` is a symlink we resolve it up front and stage the temp
/// file next to the *real* file, so `rename` replaces the target and
/// leaves the symlink itself pointing at the same inode. Without this,
/// the rename would clobber the symlink with a regular file, silently
/// breaking the link topology users set up on purpose.
///
/// On Unix we also copy the existing file's permission bits onto the
/// temp file before the swap, so an executable script stays executable
/// and private files (`0600`) stay private after the edit.
fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    // Resolve symlinks so we write to the real target. `canonicalize`
    // errors for paths that don't exist yet — new files have no link
    // to preserve, so fall back to the caller-supplied path.
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    let tmp_path = {
        let mut s = target.as_os_str().to_os_string();
        s.push(".sofos.tmp");
        PathBuf::from(s)
    };

    if let Err(e) = fs::write(&tmp_path, content) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    // Preserve the existing file's permission bits. Best-effort: if
    // `metadata` fails (new file, race) or `set_permissions` fails
    // (unusual FS), we fall through to the default permissions the
    // tmp file was created with rather than aborting the write.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(&target) {
            let mode = meta.permissions().mode();
            let _ = fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(mode));
        }
    }

    if let Err(e) = fs::rename(&tmp_path, &target) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

/// Append `content` to `path`, creating the file if it doesn't exist.
/// Unlike `write_atomic` we don't stage through a tmp file — `append` is
/// inherently incremental (each call adds to whatever's already there),
/// so an atomic swap would either lose earlier chunks or require
/// reading the whole file back each time. Instead we use `OpenOptions`
/// with `append(true)`, which the OS handles atomically for each
/// write call on POSIX.
fn append_bytes(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&target)?;
    file.write_all(content.as_bytes())?;
    file.flush()
}

/// FileSystemTool provides secure file operations sandboxed to a workspace directory
#[derive(Clone)]
pub struct FileSystemTool {
    workspace: PathBuf,
}

impl FileSystemTool {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        if !workspace.exists() {
            return Err(SofosError::Config(format!(
                "Workspace directory does not exist: {}",
                workspace.display()
            )));
        }

        let canonical = fs::canonicalize(&workspace).with_context(|| {
            format!("Failed to resolve workspace path: {}", workspace.display())
        })?;

        Ok(Self {
            workspace: canonical,
        })
    }

    /// Validate and resolve a path relative to the workspace
    /// Returns an error if the path attempts to escape the workspace
    fn validate_path(&self, path: &str) -> Result<PathBuf> {
        if path.starts_with('/') {
            return Err(SofosError::PathViolation(
                "Absolute paths are not allowed".to_string(),
            ));
        }

        if path.contains("..") {
            return Err(SofosError::PathViolation(
                "Parent directory traversal (..) is not allowed".to_string(),
            ));
        }

        let full_path = self.workspace.join(path);

        let canonical = if full_path.exists() {
            fs::canonicalize(&full_path)?
        } else if let Some(parent) = full_path.parent() {
            if parent.exists() {
                let canonical_parent = fs::canonicalize(parent)?;
                canonical_parent.join(full_path.file_name().context("Invalid filename")?)
            } else {
                full_path
            }
        } else {
            full_path
        };

        if !canonical.starts_with(&self.workspace) {
            return Err(SofosError::PathViolation(format!(
                "Path escapes workspace: {}",
                path
            )));
        }

        Ok(canonical)
    }

    pub fn read_file(&self, path: &str) -> Result<String> {
        let validated_path = self.validate_path(path)?;

        if !validated_path.exists() {
            return Err(SofosError::FileNotFound(path.to_string()));
        }

        let metadata = fs::metadata(&validated_path)
            .with_context(|| format!("Failed to read metadata for: {}", path))?;

        if metadata.len() > MAX_FILE_SIZE {
            return Err(SofosError::ToolExecution(format!(
                "File too large: {} (max: {} MB)",
                path,
                MAX_FILE_SIZE / (1024 * 1024)
            )));
        }

        let content = fs::read_to_string(&validated_path)
            .with_context(|| format!("Failed to read file: {}", path))?;

        Ok(truncate_for_context(
            &content,
            MAX_TOOL_OUTPUT_TOKENS,
            TruncationKind::File,
        ))
    }

    /// Read a file that may be outside the workspace
    /// Only used when explicitly allowed by config - does not enforce workspace prefix
    pub fn read_file_with_outside_access(&self, path: &str) -> Result<String> {
        let full_path = PathBuf::from(path);

        if !full_path.is_absolute() {
            let joined = self.workspace.join(path);
            self.read_canonicalized(joined, path)
        } else {
            self.read_canonicalized(full_path, path)
        }
    }

    fn read_canonicalized(&self, path_buf: PathBuf, original: &str) -> Result<String> {
        let canonical = fs::canonicalize(&path_buf)
            .with_context(|| format!("Failed to resolve path: {}", original))?;

        if !canonical.exists() {
            return Err(SofosError::FileNotFound(original.to_string()));
        }

        let metadata = fs::metadata(&canonical)
            .with_context(|| format!("Failed to read metadata for: {}", original))?;

        if metadata.len() > MAX_FILE_SIZE {
            return Err(SofosError::ToolExecution(format!(
                "File too large: {} (max: {} MB)",
                original,
                MAX_FILE_SIZE / (1024 * 1024)
            )));
        }

        let content = fs::read_to_string(&canonical)
            .with_context(|| format!("Failed to read file: {}", original))?;

        Ok(truncate_for_context(
            &content,
            MAX_TOOL_OUTPUT_TOKENS,
            TruncationKind::File,
        ))
    }

    pub fn write_file(&self, path: &str, content: &str) -> Result<()> {
        let validated_path = self.validate_path(path)?;

        if let Some(parent) = validated_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directories for: {}", path))?;
        }

        write_atomic(&validated_path, content)
            .with_context(|| format!("Failed to write file: {}", path))
    }

    /// Write a file that may be outside the workspace.
    /// Only used when explicitly allowed by user — does not enforce workspace prefix.
    pub fn write_file_with_outside_access(&self, path: &str, content: &str) -> Result<()> {
        let full_path = if PathBuf::from(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.workspace.join(path)
        };

        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directories for: {}", path))?;
        }

        write_atomic(&full_path, content).with_context(|| format!("Failed to write file: {}", path))
    }

    /// Append `content` to `path` inside the workspace. Creates the
    /// file and any missing parent directories if it doesn't exist,
    /// so the model can drive a "first-chunk / subsequent-chunks"
    /// pattern for writing files larger than a single `max_tokens`
    /// response can emit in one shot.
    pub fn append_file(&self, path: &str, content: &str) -> Result<()> {
        let validated_path = self.validate_path(path)?;

        if let Some(parent) = validated_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directories for: {}", path))?;
        }

        append_bytes(&validated_path, content)
            .with_context(|| format!("Failed to append to file: {}", path))
    }

    /// Append to a file that may be outside the workspace. Counterpart
    /// to `write_file_with_outside_access` — used after the user has
    /// explicitly granted Write access to the external path.
    pub fn append_file_with_outside_access(&self, path: &str, content: &str) -> Result<()> {
        let full_path = if PathBuf::from(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.workspace.join(path)
        };

        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directories for: {}", path))?;
        }

        append_bytes(&full_path, content)
            .with_context(|| format!("Failed to append to file: {}", path))
    }

    pub fn create_directory(&self, path: &str) -> Result<()> {
        let full_path = self.validate_path(path)?;
        fs::create_dir_all(&full_path)?;
        Ok(())
    }

    pub fn list_directory(&self, path: &str) -> Result<Vec<String>> {
        let full_path = self.validate_path(path)?;

        if !full_path.exists() {
            return Err(SofosError::FileNotFound(path.to_string()));
        }

        if !full_path.is_dir() {
            return Err(SofosError::InvalidPath(format!(
                "'{}' is not a directory",
                path
            )));
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(&full_path)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type()?.is_dir();
            entries.push(if is_dir { format!("{}/", name) } else { name });
        }

        entries.sort();
        Ok(entries)
    }

    pub fn _exists(&self, path: &str) -> Result<bool> {
        let full_path = self.validate_path(path)?;
        Ok(full_path.exists())
    }

    pub fn delete_file(&self, path: &str) -> Result<()> {
        let full_path = self.validate_path(path)?;

        if !full_path.exists() {
            return Err(SofosError::FileNotFound(path.to_string()));
        }

        if !full_path.is_file() {
            return Err(SofosError::InvalidPath(format!("'{}' is not a file", path)));
        }

        fs::remove_file(&full_path)?;
        Ok(())
    }

    pub fn delete_directory(&self, path: &str) -> Result<()> {
        let full_path = self.validate_path(path)?;

        if !full_path.exists() {
            return Err(SofosError::FileNotFound(path.to_string()));
        }

        if !full_path.is_dir() {
            return Err(SofosError::InvalidPath(format!(
                "'{}' is not a directory",
                path
            )));
        }

        fs::remove_dir_all(&full_path)?;
        Ok(())
    }

    pub fn move_file(&self, source: &str, destination: &str) -> Result<()> {
        let source_path = self.validate_path(source)?;
        let dest_path = self.validate_path(destination)?;

        if !source_path.exists() {
            return Err(SofosError::FileNotFound(source.to_string()));
        }

        if let Some(parent) = dest_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }

        fs::rename(&source_path, &dest_path)?;
        Ok(())
    }

    pub fn copy_file(&self, source: &str, destination: &str) -> Result<()> {
        let source_path = self.validate_path(source)?;
        let dest_path = self.validate_path(destination)?;

        if !source_path.exists() {
            return Err(SofosError::FileNotFound(source.to_string()));
        }

        if !source_path.is_file() {
            return Err(SofosError::InvalidPath(format!(
                "'{}' is not a file",
                source
            )));
        }

        if let Some(parent) = dest_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }

        fs::copy(&source_path, &dest_path)?;
        Ok(())
    }

    pub fn _workspace(&self) -> &Path {
        &self.workspace
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_path_validation_rejects_parent_traversal() {
        let temp = TempDir::new().unwrap();
        let fs_tool = FileSystemTool::new(temp.path().to_path_buf()).unwrap();

        assert!(fs_tool.validate_path("../etc/passwd").is_err());
        assert!(fs_tool.validate_path("foo/../../etc/passwd").is_err());
    }

    #[test]
    fn append_file_creates_then_appends_across_calls() {
        let temp = TempDir::new().unwrap();
        let fs_tool = FileSystemTool::new(temp.path().to_path_buf()).unwrap();
        fs_tool.append_file("doc.md", "# Part 1\n").unwrap();
        fs_tool.append_file("doc.md", "# Part 2\n").unwrap();
        fs_tool.append_file("doc.md", "# Part 3\n").unwrap();
        let contents = fs_tool.read_file("doc.md").unwrap();
        assert_eq!(contents, "# Part 1\n# Part 2\n# Part 3\n");
    }

    #[test]
    fn append_file_creates_missing_parent_dirs() {
        let temp = TempDir::new().unwrap();
        let fs_tool = FileSystemTool::new(temp.path().to_path_buf()).unwrap();
        fs_tool
            .append_file("nested/deep/file.txt", "hello")
            .unwrap();
        let contents = fs_tool.read_file("nested/deep/file.txt").unwrap();
        assert_eq!(contents, "hello");
    }

    #[test]
    fn append_preserves_multibyte_chunks() {
        // Writing long Cyrillic/CJK content part-by-part shouldn't
        // corrupt multi-byte sequences at the chunk boundary — each
        // chunk is a complete UTF-8 string on its own.
        let temp = TempDir::new().unwrap();
        let fs_tool = FileSystemTool::new(temp.path().to_path_buf()).unwrap();
        fs_tool
            .append_file("bg.md", "# Синергията между Божия промисъл")
            .unwrap();
        fs_tool.append_file("bg.md", " и човешката воля").unwrap();
        let contents = fs_tool.read_file("bg.md").unwrap();
        assert_eq!(
            contents,
            "# Синергията между Божия промисъл и човешката воля"
        );
    }

    #[test]
    fn truncate_for_context_handles_multibyte_boundary() {
        // Build a string whose natural byte-index cut (`max_tokens * 4`)
        // lands inside a multi-byte UTF-8 scalar. Cyrillic 'ъ' is 2
        // bytes, so 15 ASCII chars followed by 'ъ' puts the character
        // at bytes 15..17 — byte 16 is in the middle. Without the
        // char-boundary snap, slicing `content[..16]` would panic.
        let max_tokens = 4;
        let cut = max_tokens * 4; // 16
        let mut s = "a".repeat(cut - 1);
        s.push('ъ');
        s.push_str(" and some trailing context to push past the limit");
        assert!(
            !s.is_char_boundary(cut),
            "test setup: byte {} must be inside a multi-byte char",
            cut
        );
        let out = truncate_for_context(&s, max_tokens, TruncationKind::File);
        assert!(out.contains("[TRUNCATED"));
    }

    #[test]
    fn test_path_validation_rejects_absolute_paths() {
        let temp = TempDir::new().unwrap();
        let fs_tool = FileSystemTool::new(temp.path().to_path_buf()).unwrap();

        assert!(fs_tool.validate_path("/etc/passwd").is_err());
    }

    #[test]
    fn test_path_validation_allows_relative_paths() {
        let temp = TempDir::new().unwrap();
        let fs_tool = FileSystemTool::new(temp.path().to_path_buf()).unwrap();

        assert!(fs_tool.validate_path("foo/bar.txt").is_ok());
        assert!(fs_tool.validate_path("test.txt").is_ok());
    }

    #[test]
    fn test_write_and_read_file() {
        let temp = TempDir::new().unwrap();
        let fs_tool = FileSystemTool::new(temp.path().to_path_buf()).unwrap();

        fs_tool.write_file("test.txt", "Hello, World!").unwrap();
        let content = fs_tool.read_file("test.txt").unwrap();
        assert_eq!(content, "Hello, World!");
    }

    #[test]
    #[cfg(unix)]
    fn test_write_atomic_preserves_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("script.sh");

        // Create the file with an executable mode — the property
        // `write_atomic` has to preserve across the tmp+rename swap.
        fs::write(&path, "#!/bin/sh\necho hello\n").unwrap();
        fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        write_atomic(&path, "#!/bin/sh\necho updated\n").unwrap();

        let mode_after = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode_after, 0o755,
            "write_atomic must preserve the original file mode across the swap"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "#!/bin/sh\necho updated\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_write_atomic_preserves_symlink() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("real.txt");
        let link = temp.path().join("link.txt");

        fs::write(&target, "original content").unwrap();
        symlink(&target, &link).unwrap();

        // Writing through the symlink should update the real file and
        // leave the link itself intact — the whole point of resolving
        // via canonicalize before staging the tmp.
        write_atomic(&link, "updated via link").unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "link must still be a symlink after write_atomic"
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "updated via link");
        assert_eq!(fs::read_to_string(&link).unwrap(), "updated via link");
    }

    #[test]
    fn test_create_directory_and_list() {
        let temp = TempDir::new().unwrap();
        let fs_tool = FileSystemTool::new(temp.path().to_path_buf()).unwrap();

        fs_tool.create_directory("subdir").unwrap();
        fs_tool.write_file("subdir/file.txt", "test").unwrap();

        let entries = fs_tool.list_directory("subdir").unwrap();
        assert_eq!(entries, vec!["file.txt"]);
    }

    #[test]
    fn test_list_nested_subdirectories() {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs_tool = FileSystemTool::new(temp_dir.path().to_path_buf()).unwrap();

        fs_tool.create_directory("parent/child").unwrap();
        fs_tool.write_file("parent/file1.txt", "test1").unwrap();
        fs_tool
            .write_file("parent/child/file2.txt", "test2")
            .unwrap();

        let parent_entries = fs_tool.list_directory("parent").unwrap();
        assert!(parent_entries.contains(&"child/".to_string()));
        assert!(parent_entries.contains(&"file1.txt".to_string()));

        let child_entries = fs_tool.list_directory("parent/child").unwrap();
        assert_eq!(child_entries, vec!["file2.txt"]);
    }

    #[test]
    fn test_file_size_limit() {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs_tool = FileSystemTool::new(temp_dir.path().to_path_buf()).unwrap();

        let large_data = vec![0u8; 51 * 1024 * 1024];
        fs_tool
            .write_file("large_file.bin", &String::from_utf8_lossy(&large_data))
            .unwrap();

        let result = fs_tool.read_file("large_file.bin");
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, SofosError::ToolExecution(_)));
    }

    #[test]
    #[cfg(unix)] // Symlinks work differently on Windows
    fn test_symlink_escape_blocked() {
        use std::os::unix::fs::symlink;

        let temp_workspace = tempfile::tempdir().unwrap();
        let temp_outside = tempfile::tempdir().unwrap();

        let fs_tool = FileSystemTool::new(temp_workspace.path().to_path_buf()).unwrap();

        let outside_file = temp_outside.path().join("secret.txt");
        fs::write(&outside_file, "secret data").unwrap();

        let symlink_path = temp_workspace.path().join("escape_link");
        symlink(&outside_file, &symlink_path).unwrap();

        let result = fs_tool.read_file("escape_link");
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, SofosError::PathViolation(_)));

        if let SofosError::PathViolation(msg) = err {
            assert!(msg.contains("workspace"));
        }
    }
}
