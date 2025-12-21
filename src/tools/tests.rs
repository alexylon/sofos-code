use super::*;
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn test_read_file_blocks_relative_escape() {
    let workspace = tempdir().unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        "[permissions]\nallow = []\ndeny = []\nask = []\n",
    )
    .unwrap();

    let executor = ToolExecutor::new(workspace.path().to_path_buf(), None, false).unwrap();

    let result = executor
        .execute(
            "read_file",
            &json!({"path": "../../../../../../etc/passwd"}),
        )
        .await;

    assert!(result.is_err());
    if let Err(SofosError::ToolExecution(msg)) = result {
        assert!(msg.contains("outside workspace"));
    } else {
        panic!("Expected ToolExecution error about workspace escape");
    }
}

#[tokio::test]
async fn test_read_file_allows_explicit_outside_path_with_glob() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();

    // Create a file outside workspace
    let outside_dir = outside.path().join("data");
    std::fs::create_dir_all(&outside_dir).unwrap();
    let outside_file = outside_dir.join("file.txt");
    std::fs::write(&outside_file, "outside content").unwrap();

    // Allow with glob pattern using canonical path
    let canonical_outside = std::fs::canonicalize(outside.path()).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Read({}/data/**)\"]\ndeny = []\nask = []\n",
            canonical_outside.display()
        ),
    )
    .unwrap();

    let executor = ToolExecutor::new(workspace.path().to_path_buf(), None, false).unwrap();

    // Should allow access via glob pattern
    let result = executor
        .execute(
            "read_file",
            &json!({"path": outside_file.to_string_lossy()}),
        )
        .await;

    assert!(
        result.is_ok(),
        "Should allow file matching glob pattern: {:?}",
        result
    );
}
