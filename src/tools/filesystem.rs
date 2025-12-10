use crate::error::{Result, SofosError};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024; // 50MB limit

/// FileSystemTool provides secure file operations sandboxed to a workspace directory
#[derive(Clone)]
pub struct FileSystemTool {
    workspace: PathBuf,
}

impl FileSystemTool {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        let workspace = workspace.canonicalize().map_err(|e| {
            SofosError::Config(format!("Failed to canonicalize workspace path: {}", e))
        })?;

        Ok(Self { workspace })
    }

    /// Validate and resolve a path relative to the workspace
    /// Returns an error if the path attempts to escape the workspace
    fn validate_path(&self, path: &str) -> Result<PathBuf> {
        let path = path.trim();
        
        if Path::new(path).is_absolute() {
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

        // Canonicalize if it exists, otherwise just check the parent
        let canonical =
            if full_path.exists() {
                full_path.canonicalize().map_err(|e| {
                    SofosError::InvalidPath(format!("Failed to canonicalize path: {}", e))
                })?
            } else {
                // For non-existent paths, validate that the parent is within workspace
                if let Some(parent) = full_path.parent() {
                    if parent.exists() {
                        let canonical_parent = parent.canonicalize().map_err(|e| {
                            SofosError::InvalidPath(format!("Failed to canonicalize parent: {}", e))
                        })?;
                        canonical_parent.join(full_path.file_name().ok_or_else(|| {
                            SofosError::InvalidPath("Invalid filename".to_string())
                        })?)
                    } else {
                        full_path
                    }
                } else {
                    full_path
                }
            };

        if !canonical.starts_with(&self.workspace) {
            return Err(SofosError::PathViolation(format!(
                "Path '{}' is outside the workspace",
                path
            )));
        }

        Ok(canonical)
    }

    pub fn read_file(&self, path: &str) -> Result<String> {
        let full_path = self.validate_path(path)?;

        if !full_path.exists() {
            return Err(SofosError::FileNotFound(path.to_string()));
        }

        if !full_path.is_file() {
            return Err(SofosError::InvalidPath(format!("'{}' is not a file", path)));
        }

        // Check file size before reading to prevent OOM
        let metadata = fs::metadata(&full_path).map_err(SofosError::Io)?;
        if metadata.len() > MAX_FILE_SIZE {
            return Err(SofosError::InvalidPath(format!(
                "File '{}' is too large ({} bytes). Maximum size is {} MB",
                path,
                metadata.len(),
                MAX_FILE_SIZE / (1024 * 1024)
            )));
        }

        fs::read_to_string(&full_path).map_err(SofosError::Io)
    }

    pub fn write_file(&self, path: &str, content: &str) -> Result<()> {
        let full_path = self.validate_path(path)?;

        if let Some(parent) = full_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }

        fs::write(&full_path, content)?;
        Ok(())
    }

    pub fn _append_file(&self, path: &str, content: &str) -> Result<()> {
        let full_path = self.validate_path(path)?;

        if !full_path.exists() {
            return Err(SofosError::FileNotFound(path.to_string()));
        }

        let mut existing = self.read_file(path)?;
        existing.push_str(content);
        self.write_file(path, &existing)?;
        Ok(())
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

        // Create a file larger than 50MB (51MB)
        let large_data = vec![0u8; 51 * 1024 * 1024];
        fs_tool
            .write_file("large_file.bin", &String::from_utf8_lossy(&large_data))
            .unwrap();

        let result = fs_tool.read_file("large_file.bin");
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, SofosError::InvalidPath(_)));

        // Verify error message mentions file size
        if let SofosError::InvalidPath(msg) = err {
            assert!(msg.contains("too large"));
            assert!(msg.contains("50 MB"));
        }
    }

    #[test]
    #[cfg(unix)] // Symlinks work differently on Windows
    fn test_symlink_escape_blocked() {
        use std::os::unix::fs::symlink;

        let temp_workspace = tempfile::tempdir().unwrap();
        let temp_outside = tempfile::tempdir().unwrap();

        let fs_tool = FileSystemTool::new(temp_workspace.path().to_path_buf()).unwrap();

        // Create a file outside the workspace
        let outside_file = temp_outside.path().join("secret.txt");
        fs::write(&outside_file, "secret data").unwrap();

        // Try to create a symlink inside workspace pointing outside
        let symlink_path = temp_workspace.path().join("escape_link");
        symlink(&outside_file, &symlink_path).unwrap();

        // Attempt to read via symlink should fail with path violation
        let result = fs_tool.read_file("escape_link");
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, SofosError::PathViolation(_)));

        if let SofosError::PathViolation(msg) = err {
            assert!(msg.contains("outside the workspace"));
        }
    }
}
