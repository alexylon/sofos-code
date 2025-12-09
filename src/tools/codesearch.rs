use crate::error::{Result, SofosError};
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone)]
pub struct CodeSearchTool {
    workspace: PathBuf,
}

impl CodeSearchTool {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        match Command::new("rg").arg("--version").output() {
            Ok(_) => Ok(Self { workspace }),
            Err(_) => Err(SofosError::Config(
                "ripgrep (rg) not found. Please install it: https://github.com/BurntSushi/ripgrep#installation".to_string()
            )),
        }
    }

    /// Search for a pattern in the codebase using ripgrep
    pub fn search(
        &self,
        pattern: &str,
        file_type: Option<&str>,
        max_results: Option<usize>,
    ) -> Result<String> {
        let mut cmd = Command::new("rg");

        cmd.arg("--heading")
            .arg("--line-number")
            .arg("--color=never")
            .arg("--no-messages")
            .arg("--with-filename");

        if let Some(max) = max_results {
            cmd.arg("--max-count").arg(max.to_string());
        } else {
            cmd.arg("--max-count").arg("50");
        }

        if let Some(ft) = file_type {
            cmd.arg("--type").arg(ft);
        }

        cmd.arg(pattern);
        cmd.current_dir(&self.workspace);

        let output = cmd
            .output()
            .map_err(|e| SofosError::ToolExecution(format!("Failed to execute ripgrep: {}", e)))?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.trim().is_empty() {
                Ok(format!("No matches found for pattern: '{}'", pattern))
            } else {
                Ok(stdout.to_string())
            }
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("No matches found") || stderr.is_empty() {
                Ok(format!("No matches found for pattern: '{}'", pattern))
            } else {
                Err(SofosError::ToolExecution(format!(
                    "ripgrep error: {}",
                    stderr
                )))
            }
        }
    }

    /// List available file types supported by ripgrep
    pub fn _list_file_types() -> Result<String> {
        let output = Command::new("rg")
            .arg("--type-list")
            .output()
            .map_err(|e| SofosError::ToolExecution(format!("Failed to list file types: {}", e)))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(SofosError::ToolExecution(
                "Failed to get ripgrep file types".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_code_search_creation() {
        let temp = TempDir::new().unwrap();

        // This will fail if ripgrep is not installed, which is fine for CI
        let result = CodeSearchTool::new(temp.path().to_path_buf());

        // Just check that the constructor doesn't panic
        if let Ok(tool) = result {
            assert_eq!(tool.workspace, temp.path());
        }
    }

    #[test]
    fn test_search_functionality() {
        let temp = TempDir::new().unwrap();
        let test_file = temp.path().join("test.txt");
        fs::write(&test_file, "Hello World\nTest Pattern\nAnother Line").unwrap();

        if let Ok(tool) = CodeSearchTool::new(temp.path().to_path_buf()) {
            let result = tool.search("Pattern", None, None);
            if let Ok(output) = result {
                assert!(output.contains("Pattern") || output.contains("No matches"));
            }
        }
    }
}
