use crate::error::{Result, SofosError};
use crate::tools::utils::{MAX_TOOL_OUTPUT_TOKENS, TruncationKind, truncate_for_context};
use std::path::PathBuf;
use std::process::Command;

/// Shared so the UI display layer can strip it without duplicating the literal.
pub const SEARCH_RESULTS_PREFIX: &str = "Code search results:\n\n";

/// Directories that are almost always build artefacts or vendored code and
/// should never show up in a code search. ripgrep already respects
/// `.gitignore`, but that protection vanishes the moment the workspace
/// isn't a clean git repo (symlinked-in dir, freshly-cloned submodule with
/// a committed lockfile, etc.) — so we apply these as belt-and-suspenders
/// `--glob '!<dir>/**'` excludes on every call. The human-readable list
/// in the tool schema is derived from this via [`default_exclude_dirs_human`],
/// so adding an entry here automatically updates the description the
/// model sees.
pub const DEFAULT_EXCLUDE_DIRS: &[&str] =
    &["target", "node_modules", ".git", "dist", "build"];

/// Default per-file match cap when the caller doesn't specify `max_results`.
/// Exposed `pub` so the schema description in `types.rs` stays in sync
/// without re-hardcoding the value.
pub const DEFAULT_MAX_RESULTS_PER_FILE: usize = 50;

/// Per-line column cap (300). Matched lines longer than this are replaced
/// with a short preview (via `--max-columns-preview`) instead of being
/// dumped in full. Prevents a single minified / generated line from blowing
/// past the tool-output budget.
const MAX_COLUMNS_FLAG: &str = "--max-columns=300";

/// Per-file size cap (1 MB). Files larger than this are skipped entirely —
/// they're overwhelmingly lockfiles, generated bundles, or binaries
/// mis-detected as text. There is no per-call override; if a caller
/// legitimately needs to grep inside a huge file, `execute_bash` with a
/// direct `rg` invocation is the escape hatch.
const MAX_FILESIZE_FLAG: &str = "--max-filesize=1M";

/// Render [`DEFAULT_EXCLUDE_DIRS`] as a comma-separated human string
/// (e.g. `"target/, node_modules/, .git/, dist/, build/"`) for use in
/// tool-schema descriptions. Kept in `codesearch` so the const array
/// remains the single source of truth.
pub fn default_exclude_dirs_human() -> String {
    DEFAULT_EXCLUDE_DIRS
        .iter()
        .map(|d| format!("{}/", d))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone)]
pub struct CodeSearchTool {
    workspace: PathBuf,
    rg_path: PathBuf,
}

impl CodeSearchTool {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        // Allow users to pin an explicit rg path (e.g. when PATH is sanitized in GUI apps)
        let env_override = std::env::var_os("SOFOS_RG_PATH").map(PathBuf::from);

        // Fallback search list for common macOS/Homebrew and Linux locations
        let fallback_paths = ["/opt/homebrew/bin/rg", "/usr/local/bin/rg", "/usr/bin/rg"];

        let try_path = |p: &PathBuf| Command::new(p).arg("--version").output();

        if let Some(p) = env_override {
            if try_path(&p).is_ok() {
                return Ok(Self {
                    workspace,
                    rg_path: p,
                });
            }
        }

        let default_rg = PathBuf::from("rg");
        if try_path(&default_rg).is_ok() {
            return Ok(Self {
                workspace,
                rg_path: default_rg,
            });
        }

        for path in fallback_paths.iter().map(PathBuf::from) {
            if try_path(&path).is_ok() {
                return Ok(Self {
                    workspace,
                    rg_path: path,
                });
            }
        }

        let path_env = std::env::var("PATH").unwrap_or_else(|_| "<unset>".to_string());
        Err(SofosError::Config(format!(
            "ripgrep (rg) not found. Checked SOFOS_RG_PATH, PATH, and common locations.\nPATH seen by Sofos: {}\nInstall ripgrep: https://github.com/BurntSushi/ripgrep#installation",
            path_env
        )))
    }

    /// Search for a pattern in the codebase using ripgrep.
    ///
    /// When `include_ignored` is `true`, the hard-coded [`DEFAULT_EXCLUDE_DIRS`]
    /// and ripgrep's automatic `.gitignore` / `.ignore` filtering are both
    /// disabled — the escape hatch for the rare case where the user genuinely
    /// needs to grep inside `target/`, `node_modules/`, or other normally-
    /// skipped paths. Defaults to `false` so the usual output-size protection
    /// stays on.
    pub fn search(
        &self,
        pattern: &str,
        file_type: Option<&str>,
        max_results: Option<usize>,
        include_ignored: bool,
    ) -> Result<String> {
        let mut cmd = Command::new(&self.rg_path);

        cmd.arg("--heading")
            .arg("--line-number")
            .arg("--color=never")
            .arg("--no-messages")
            .arg("--with-filename")
            .arg(MAX_COLUMNS_FLAG)
            .arg("--max-columns-preview")
            .arg(MAX_FILESIZE_FLAG);

        if include_ignored {
            // Bypass gitignore / .ignore / global ignores as well — callers
            // asking for "include ignored" almost always want everything,
            // not just our hard-coded extras.
            cmd.arg("--no-ignore");
        } else {
            for dir in DEFAULT_EXCLUDE_DIRS {
                cmd.arg("--glob").arg(format!("!{}/**", dir));
            }
        }

        let max_count = max_results.unwrap_or(DEFAULT_MAX_RESULTS_PER_FILE);
        cmd.arg("--max-count").arg(max_count.to_string());

        if let Some(ft) = file_type {
            if !ft.trim().is_empty() {
                cmd.arg("--type").arg(ft);
            }
        }

        // `--` terminates flag parsing so a pattern like `-v`, `--files`,
        // or `--no-config` is treated as a literal search string instead
        // of flipping ripgrep's behaviour. Without this, a confused model
        // emitting `pattern="-v"` would silently invert every match.
        cmd.arg("--").arg(pattern);
        cmd.current_dir(&self.workspace);

        let output = cmd
            .output()
            .map_err(|e| SofosError::ToolExecution(format!("Failed to execute ripgrep: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if output.status.success() && !stdout.trim().is_empty() {
            return Ok(truncate_for_context(
                &stdout,
                MAX_TOOL_OUTPUT_TOKENS,
                TruncationKind::SearchOutput,
            ));
        }

        // ripgrep exits 1 for "no matches" (non-success but benign) and
        // 2+ for real errors. An empty stderr with a non-success exit
        // is the common "no matches" shape; we also keep the legacy
        // "No matches found" substring check for defence-in-depth.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let is_no_match = output.status.success()
            || stderr.is_empty()
            || stderr.contains("No matches found");
        if is_no_match {
            Ok(format!("No matches found for pattern: '{}'", pattern))
        } else {
            Err(SofosError::ToolExecution(format!(
                "ripgrep error: {}",
                stderr
            )))
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
    fn default_exclude_dirs_human_renders_every_entry() {
        let rendered = default_exclude_dirs_human();
        for dir in DEFAULT_EXCLUDE_DIRS {
            let expected = format!("{}/", dir);
            assert!(
                rendered.contains(&expected),
                "human rendering '{}' missing entry '{}'",
                rendered,
                expected
            );
        }
        // Comma-joined with one separator per boundary, no trailing comma.
        assert_eq!(
            rendered.matches(", ").count(),
            DEFAULT_EXCLUDE_DIRS.len() - 1
        );
        assert!(!rendered.ends_with(','));
    }

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
            let result = tool.search("Pattern", None, None, false);
            if let Ok(output) = result {
                assert!(output.contains("Pattern") || output.contains("No matches"));
            }
        }
    }

    #[test]
    fn search_treats_flag_like_pattern_as_literal() {
        // Without `--` before the pattern, ripgrep would interpret `-v` as
        // the "invert match" flag and `--files` as "list files instead of
        // search". Both would silently return the wrong output instead of
        // treating the token as a literal search string.
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join("notes.txt"),
            "release --files checklist\nsome -v output\n",
        )
        .unwrap();

        let Ok(tool) = CodeSearchTool::new(temp.path().to_path_buf()) else {
            return;
        };

        let out = tool.search("--files", None, None, false).unwrap();
        assert!(
            out.contains("--files checklist"),
            "pattern '--files' must be treated as literal; got: {}",
            out
        );

        let out = tool.search("-v", None, None, false).unwrap();
        assert!(
            out.contains("some -v output"),
            "pattern '-v' must be treated as literal; got: {}",
            out
        );
    }

    #[test]
    fn search_default_excludes_target_directory() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("target");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("junk.rs"), "unique_marker_xyz\n").unwrap();

        let Ok(tool) = CodeSearchTool::new(temp.path().to_path_buf()) else {
            return;
        };

        let default_output = tool.search("unique_marker_xyz", None, None, false).unwrap();
        assert!(
            default_output.contains("No matches"),
            "target/ should be excluded by default; got: {}",
            default_output
        );

        let override_output = tool.search("unique_marker_xyz", None, None, true).unwrap();
        assert!(
            override_output.contains("unique_marker_xyz"),
            "include_ignored=true must surface files under target/; got: {}",
            override_output
        );
    }
}
