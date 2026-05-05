use std::path::PathBuf;
use tempfile::TempDir;

/// Allocate a fresh temp directory for use as a test workspace and
/// return both the `TempDir` (kept alive by the caller — drop it to
/// clean up) and its path as an owned `PathBuf` (handy for passing
/// straight into the tool constructors that take ownership).
pub fn workspace() -> (TempDir, PathBuf) {
    let temp = TempDir::new().expect("create TempDir for test workspace");
    let path = temp.path().to_path_buf();
    (temp, path)
}
