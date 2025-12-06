use crate::error::{Result, SofosError};
use std::fs;
use std::path::{Path, PathBuf};

/// FileSystemTool provides secure file operations sandboxed to a workspace directory
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
        let canonical = if full_path.exists() {
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
                    canonical_parent.join(
                        full_path
                            .file_name()
                            .ok_or_else(|| SofosError::InvalidPath("Invalid filename".to_string()))?,
                    )
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
            return Err(SofosError::InvalidPath(format!(
                "'{}' is not a file",
                path
            )));
        }

        fs::read_to_string(&full_path).map_err(|e| {
            SofosError::Io(e)
        })
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
            let name = entry
                .file_name()
                .to_string_lossy()
                .to_string();
            let is_dir = entry.file_type()?.is_dir();
            entries.push(if is_dir {
                format!("{}/", name)
            } else {
                name
            });
        }

        entries.sort();
        Ok(entries)
    }

    pub fn _exists(&self, path: &str) -> Result<bool> {
        let full_path = self.validate_path(path)?;
        Ok(full_path.exists())
    }

    pub fn _delete_file(&self, path: &str) -> Result<()> {
        let full_path = self.validate_path(path)?;

        if !full_path.exists() {
            return Err(SofosError::FileNotFound(path.to_string()));
        }

        if !full_path.is_file() {
            return Err(SofosError::InvalidPath(format!(
                "'{}' is not a file",
                path
            )));
        }

        fs::remove_file(&full_path)?;
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
        fs_tool.write_file("parent/child/file2.txt", "test2").unwrap();

        let parent_entries = fs_tool.list_directory("parent").unwrap();
        assert!(parent_entries.contains(&"child/".to_string()));
        assert!(parent_entries.contains(&"file1.txt".to_string()));

        let child_entries = fs_tool.list_directory("parent/child").unwrap();
        assert_eq!(child_entries, vec!["file2.txt"]);
    }
}
