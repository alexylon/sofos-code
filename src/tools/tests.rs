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

    let executor = ToolExecutor::new(workspace.path().to_path_buf(), None, None, false).unwrap();

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

    let executor = ToolExecutor::new(workspace.path().to_path_buf(), None, None, false).unwrap();

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

#[tokio::test]
async fn test_edit_file_replaces_string() {
    let workspace = tempdir().unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        "[permissions]\nallow = []\ndeny = []\nask = []\n",
    )
    .unwrap();
    std::fs::write(workspace.path().join("test.txt"), "hello world").unwrap();

    let executor = ToolExecutor::new(workspace.path().to_path_buf(), None, None, false).unwrap();
    let result = executor
        .execute(
            "edit_file",
            &json!({"path": "test.txt", "old_string": "world", "new_string": "rust"}),
        )
        .await;

    assert!(result.is_ok());
    let content = std::fs::read_to_string(workspace.path().join("test.txt")).unwrap();
    assert_eq!(content, "hello rust");
}

#[tokio::test]
async fn test_edit_file_not_found_string() {
    let workspace = tempdir().unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        "[permissions]\nallow = []\ndeny = []\nask = []\n",
    )
    .unwrap();
    std::fs::write(workspace.path().join("test.txt"), "hello world").unwrap();

    let executor = ToolExecutor::new(workspace.path().to_path_buf(), None, None, false).unwrap();
    let result = executor
        .execute(
            "edit_file",
            &json!({"path": "test.txt", "old_string": "missing", "new_string": "x"}),
        )
        .await;

    assert!(result.is_err());
    let content = std::fs::read_to_string(workspace.path().join("test.txt")).unwrap();
    assert_eq!(content, "hello world");
}

#[tokio::test]
async fn test_edit_file_replace_all() {
    let workspace = tempdir().unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        "[permissions]\nallow = []\ndeny = []\nask = []\n",
    )
    .unwrap();
    std::fs::write(workspace.path().join("test.txt"), "aaa bbb aaa").unwrap();

    let executor = ToolExecutor::new(workspace.path().to_path_buf(), None, None, false).unwrap();
    let result = executor
        .execute(
            "edit_file",
            &json!({"path": "test.txt", "old_string": "aaa", "new_string": "ccc", "replace_all": true}),
        )
        .await;

    assert!(result.is_ok());
    let content = std::fs::read_to_string(workspace.path().join("test.txt")).unwrap();
    assert_eq!(content, "ccc bbb ccc");
}

#[tokio::test]
async fn test_glob_files_finds_matches() {
    let workspace = tempdir().unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        "[permissions]\nallow = []\ndeny = []\nask = []\n",
    )
    .unwrap();

    let src = workspace.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.rs"), "").unwrap();
    std::fs::write(src.join("lib.rs"), "").unwrap();
    std::fs::write(workspace.path().join("README.md"), "").unwrap();

    let executor = ToolExecutor::new(workspace.path().to_path_buf(), None, None, false).unwrap();
    let result = executor
        .execute("glob_files", &json!({"pattern": "**/*.rs"}))
        .await;

    assert!(result.is_ok());
    let text = result.unwrap().text().to_string();
    assert!(text.contains("main.rs"));
    assert!(text.contains("lib.rs"));
    assert!(!text.contains("README.md"));
}

#[tokio::test]
async fn test_glob_files_no_matches() {
    let workspace = tempdir().unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        "[permissions]\nallow = []\ndeny = []\nask = []\n",
    )
    .unwrap();

    let executor = ToolExecutor::new(workspace.path().to_path_buf(), None, None, false).unwrap();
    let result = executor
        .execute("glob_files", &json!({"pattern": "**/*.xyz"}))
        .await;

    assert!(result.is_ok());
    let text = result.unwrap().text().to_string();
    assert!(text.contains("No files matching"));
}
