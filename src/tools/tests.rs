use super::*;
use serde_json::json;
use tempfile::tempdir;

#[test]
fn test_validate_morph_output_rejects_empty() {
    assert!(validate_morph_output("fn main() { println!(\"hi\"); }", "").is_err());
    assert!(validate_morph_output("fn main() { println!(\"hi\"); }", "   \n  ").is_err());
}

#[test]
fn test_validate_morph_output_rejects_dramatic_shrink() {
    // Simulate Morph returning a severely truncated response for a
    // non-trivial file — the exact corruption pattern we've seen in
    // practice. Original is >500 bytes, merged is a stub under 200.
    let original = "fn main() {\n".to_string() + &"    println!(\"line\");\n".repeat(40) + "}\n";
    let merged = "fn main() {\n";
    assert!(validate_morph_output(&original, merged).is_err());
}

#[test]
fn test_validate_morph_output_accepts_reasonable_edits() {
    let original = "fn main() {\n".to_string() + &"    println!(\"line\");\n".repeat(40) + "}\n";
    // A realistic edit — replaces a block but keeps roughly the same size.
    let merged = "fn main() {\n".to_string() + &"    println!(\"other\");\n".repeat(40) + "}\n";
    assert!(validate_morph_output(&original, &merged).is_ok());
}

#[test]
fn test_validate_morph_output_allows_legitimate_small_stub() {
    // User asks Morph to delete everything except a minimal `main()`.
    // Original is a large file; merged is a ~50-byte stub. It's small
    // but at or above the floor, so it must still be accepted — the
    // sanity check exists to catch garbage, not legitimate deletions.
    let original = "fn main() {\n".to_string() + &"    println!(\"line\");\n".repeat(40) + "}\n";
    let merged = "fn main() {\n    // trimmed down by user\n    Ok(())\n}\n";
    assert!(
        merged.len() >= 50,
        "test stub must be at or above the floor"
    );
    assert!(validate_morph_output(&original, merged).is_ok());
}

#[test]
fn test_validate_morph_output_rejects_missing_trailing_newline() {
    // If the original ends with `\n` but the merged output doesn't,
    // the response was almost certainly cut off mid-line.
    let original = "line 1\nline 2\nline 3\n";
    let merged = "line 1\nline 2\nline";
    assert!(validate_morph_output(original, merged).is_err());
}

#[test]
fn test_validate_morph_output_allows_no_newline_when_original_had_none() {
    // Files without a final newline (the original was that way, not
    // because of truncation) should still be accepted.
    let original = "no_trailing_newline";
    let merged = "modified_no_trailing_newline";
    assert!(validate_morph_output(original, merged).is_ok());
}

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

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

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

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

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

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();
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
async fn test_edit_file_preserves_content_past_truncation_cap() {
    // Regression: `edit_file` used to read through `fs_tool.read_file`,
    // which applied a ~64 KB `truncate_for_context` cap before returning
    // the "original" content. The edit was then applied to that
    // truncated view and written back — silently dropping everything
    // past the first ~64 KB and leaving a literal `[TRUNCATED: ...]`
    // footer in the file. Any source file bigger than the cap was
    // getting corrupted by a simple search-and-replace edit.
    //
    // This test builds a file far above the cap (~200 KB), runs
    // `edit_file` against a match that lives in the prefix, and then
    // verifies the tail is still intact and nothing from the
    // truncation-suffix copy leaked onto disk.
    let workspace = tempdir().unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        "[permissions]\nallow = []\ndeny = []\nask = []\n",
    )
    .unwrap();

    let head = "TARGET_MARKER\n".to_string();
    let middle = "x".repeat(200_000);
    let tail = "\nEND_OF_FILE_SENTINEL\n".to_string();
    let original = format!("{head}{middle}{tail}");
    std::fs::write(workspace.path().join("big.txt"), &original).unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();
    let result = executor
        .execute(
            "edit_file",
            &json!({
                "path": "big.txt",
                "old_string": "TARGET_MARKER",
                "new_string": "REPLACED_MARKER",
            }),
        )
        .await;
    assert!(result.is_ok(), "edit_file failed: {:?}", result);

    let on_disk = std::fs::read_to_string(workspace.path().join("big.txt")).unwrap();
    assert!(
        on_disk.starts_with("REPLACED_MARKER\n"),
        "head must contain the replacement"
    );
    assert!(
        on_disk.ends_with("END_OF_FILE_SENTINEL\n"),
        "tail must be preserved — file was truncated: length={}",
        on_disk.len()
    );
    assert!(
        !on_disk.contains("[TRUNCATED:"),
        "truncation suffix must never land on disk"
    );
    assert_eq!(
        on_disk.len(),
        original.len() + "REPLACED_MARKER".len() - "TARGET_MARKER".len(),
        "file length should change only by the replacement delta"
    );
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

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();
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

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();
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

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();
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
async fn test_glob_files_skips_default_excludes() {
    // `glob_files("**/*.rs")` must not descend into `target/`,
    // `node_modules/`, `.git/`, etc. by default — a broad pattern over
    // a repo with a populated `target/` would otherwise return
    // thousands of compiler-generated `.rs` files and blow past the
    // API tool-output limit.
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

    let generated = workspace.path().join("target/debug/build/foo");
    std::fs::create_dir_all(&generated).unwrap();
    std::fs::write(generated.join("generated.rs"), "").unwrap();

    let node_dep = workspace.path().join("node_modules/pkg");
    std::fs::create_dir_all(&node_dep).unwrap();
    std::fs::write(node_dep.join("index.rs"), "").unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    let result = executor
        .execute("glob_files", &json!({"pattern": "**/*.rs"}))
        .await;
    assert!(result.is_ok());
    let text = result.unwrap().text().to_string();
    assert!(text.contains("main.rs"), "expected src/main.rs; got: {text}");
    assert!(
        !text.contains("generated.rs"),
        "target/ descent must be blocked by default; got: {text}"
    );
    assert!(
        !text.contains("node_modules"),
        "node_modules/ descent must be blocked by default; got: {text}"
    );

    // include_ignored=true walks everything.
    let result = executor
        .execute(
            "glob_files",
            &json!({"pattern": "**/*.rs", "include_ignored": true}),
        )
        .await;
    assert!(result.is_ok());
    let text = result.unwrap().text().to_string();
    assert!(
        text.contains("generated.rs"),
        "include_ignored=true must surface target/ contents; got: {text}"
    );
}

#[tokio::test]
async fn test_glob_files_gates_external_path_through_permissions() {
    // `base = ".."` used to pass through `workspace.join("..")` = workspace
    // parent and then `read_dir` would walk it. `base = "/etc"` is even
    // more direct — `Path::join` replaces self with an absolute argument,
    // so the walk started at `/etc`. Both paths are now routed through
    // `check_read_access`, which in non-interactive mode with no allow
    // grant produces an "outside workspace" error. With an explicit
    // allow rule the same path succeeds — so the legitimate "review
    // /some/other/repo" workflow still works, but unauthorised escapes
    // can't walk arbitrary directories silently.
    let workspace = tempdir().unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        "[permissions]\nallow = []\ndeny = []\nask = []\n",
    )
    .unwrap();

    let outside = tempdir().unwrap();
    std::fs::write(outside.path().join("SECRET_MARKER"), "leak").unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    // Parent-directory escape — no grant, non-interactive mode → denied
    // with the "outside workspace" hint.
    let result = executor
        .execute("glob_files", &json!({"pattern": "**/*", "path": ".."}))
        .await;
    match result {
        Err(SofosError::ToolExecution(msg)) => assert!(
            msg.contains("outside workspace"),
            "expected 'outside workspace' hint; got: {msg}"
        ),
        other => panic!("expected ToolExecution error for base='..'; got: {other:?}"),
    }

    // Absolute-path escape gets the same treatment.
    let outside_abs = outside.path().to_string_lossy().to_string();
    let result = executor
        .execute(
            "glob_files",
            &json!({"pattern": "**/*", "path": outside_abs}),
        )
        .await;
    match result {
        Err(SofosError::ToolExecution(msg)) => assert!(
            msg.contains("outside workspace"),
            "expected 'outside workspace' hint; got: {msg}"
        ),
        other => panic!("expected ToolExecution error for absolute path; got: {other:?}"),
    }

    // With an explicit Read allow rule covering the outside directory,
    // `glob_files` proceeds — the legitimate workflow the permission
    // system is there to support.
    let canonical_outside = std::fs::canonicalize(outside.path()).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Read({}/**)\"]\ndeny = []\nask = []\n",
            canonical_outside.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();
    let result = executor
        .execute(
            "glob_files",
            &json!({
                "pattern": "**/SECRET_MARKER",
                "path": outside.path().to_string_lossy(),
            }),
        )
        .await;
    let text = result
        .expect("glob_files with Read grant must succeed")
        .text()
        .to_string();
    assert!(
        text.contains("SECRET_MARKER"),
        "expected grant to surface outside files; got: {text}"
    );
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

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();
    let result = executor
        .execute("glob_files", &json!({"pattern": "**/*.xyz"}))
        .await;

    assert!(result.is_ok());
    let text = result.unwrap().text().to_string();
    assert!(text.contains("No files matching"));
}

// --- External path permission tests ---

#[tokio::test]
async fn test_write_file_to_external_path_blocked_without_grant() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_file = outside.path().join("file.txt");

    let canonical_outside = std::fs::canonicalize(outside.path()).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    // No Write grant — only Read
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Read({}/**)\"]\ndeny = []\nask = []\n",
            canonical_outside.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    let result = executor
        .execute(
            "write_file",
            &json!({"path": outside_file.to_string_lossy(), "content": "test"}),
        )
        .await;

    assert!(
        result.is_err(),
        "Write should be blocked without Write grant"
    );
    if let Err(SofosError::ToolExecution(msg)) = result {
        assert!(msg.contains("outside workspace"));
    }
}

#[tokio::test]
async fn test_write_file_to_external_path_allowed_with_grant() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_file = outside.path().join("file.txt");

    let canonical_outside = std::fs::canonicalize(outside.path()).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Write({}/**)\"]\ndeny = []\nask = []\n",
            canonical_outside.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    let result = executor
        .execute(
            "write_file",
            &json!({"path": outside_file.to_string_lossy(), "content": "hello external"}),
        )
        .await;

    assert!(
        result.is_ok(),
        "Write should succeed with Write grant: {:?}",
        result
    );
    let content = std::fs::read_to_string(&outside_file).unwrap();
    assert_eq!(content, "hello external");
}

#[tokio::test]
async fn test_edit_file_external_path_allowed_with_write_grant() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_file = outside.path().join("editable.txt");
    std::fs::write(&outside_file, "foo bar baz").unwrap();

    let canonical_outside = std::fs::canonicalize(outside.path()).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Write({}/**)\"]\ndeny = []\nask = []\n",
            canonical_outside.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    let result = executor
        .execute(
            "edit_file",
            &json!({
                "path": outside_file.to_string_lossy(),
                "old_string": "bar",
                "new_string": "qux"
            }),
        )
        .await;

    assert!(
        result.is_ok(),
        "Edit should succeed with Write grant: {:?}",
        result
    );
    let content = std::fs::read_to_string(&outside_file).unwrap();
    assert_eq!(content, "foo qux baz");
}

#[tokio::test]
async fn test_read_grant_does_not_allow_write() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_file = outside.path().join("readonly.txt");
    std::fs::write(&outside_file, "original").unwrap();

    let canonical_outside = std::fs::canonicalize(outside.path()).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    // Only Read grant, no Write
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Read({}/**)\"]\ndeny = []\nask = []\n",
            canonical_outside.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    // Read should work
    let read_result = executor
        .execute(
            "read_file",
            &json!({"path": outside_file.to_string_lossy()}),
        )
        .await;
    assert!(read_result.is_ok(), "Read should work with Read grant");

    // Edit (write) should be blocked
    let edit_result = executor
        .execute(
            "edit_file",
            &json!({
                "path": outside_file.to_string_lossy(),
                "old_string": "original",
                "new_string": "modified"
            }),
        )
        .await;
    assert!(
        edit_result.is_err(),
        "Edit should be blocked — Read grant doesn't imply Write"
    );

    // File should be unchanged
    let content = std::fs::read_to_string(&outside_file).unwrap();
    assert_eq!(content, "original");
}

#[tokio::test]
async fn test_list_directory_external_with_read_grant() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_dir = outside.path().join("listing");
    std::fs::create_dir_all(&outside_dir).unwrap();
    std::fs::write(outside_dir.join("a.txt"), "").unwrap();
    std::fs::write(outside_dir.join("b.txt"), "").unwrap();

    let canonical_outside = std::fs::canonicalize(outside.path()).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Read({}/**)\"]\ndeny = []\nask = []\n",
            canonical_outside.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    let result = executor
        .execute(
            "list_directory",
            &json!({"path": outside_dir.to_string_lossy()}),
        )
        .await;

    assert!(
        result.is_ok(),
        "list_directory should work with Read grant: {:?}",
        result
    );
    let text = result.unwrap().text().to_string();
    assert!(text.contains("a.txt"));
    assert!(text.contains("b.txt"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_symlink_does_not_bypass_write_permission() {
    use std::os::unix::fs::symlink;

    let workspace = tempdir().unwrap();
    let allowed_dir = tempdir().unwrap();
    let secret_dir = tempdir().unwrap();

    // Create target file in secret dir
    let secret_file = secret_dir.path().join("secret.txt");
    std::fs::write(&secret_file, "secret data").unwrap();

    // Create symlink in allowed dir pointing to secret file
    let link_path = allowed_dir.path().join("link.txt");
    symlink(&secret_file, &link_path).unwrap();

    let canonical_allowed = std::fs::canonicalize(allowed_dir.path()).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    // Grant Write only to allowed_dir, NOT secret_dir
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Write({}/**)\"]\ndeny = []\nask = []\n",
            canonical_allowed.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    // Editing via symlink should be blocked — canonical resolves to secret_dir
    let result = executor
        .execute(
            "edit_file",
            &json!({
                "path": link_path.to_string_lossy(),
                "old_string": "secret",
                "new_string": "hacked"
            }),
        )
        .await;

    assert!(
        result.is_err(),
        "Symlink should not bypass Write permission scope"
    );

    // Secret file should be unchanged
    let content = std::fs::read_to_string(&secret_file).unwrap();
    assert_eq!(content, "secret data");
}

#[tokio::test]
async fn test_bash_external_path_blocked_without_grant() {
    let workspace = tempdir().unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        "[permissions]\nallow = []\ndeny = []\nask = []\n",
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    let result = executor
        .execute("execute_bash", &json!({"command": "cat /etc/hosts"}))
        .await;

    assert!(result.is_err(), "Bash with external path should be blocked");
    if let Err(SofosError::ToolExecution(msg)) = result {
        assert!(
            msg.contains("outside workspace") || msg.contains("Bash access denied"),
            "Error should mention external path: {}",
            msg
        );
    }
}

#[tokio::test]
async fn test_bash_external_path_allowed_with_grant() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_file = outside.path().join("readable.txt");
    std::fs::write(&outside_file, "bash content").unwrap();

    let canonical_outside = std::fs::canonicalize(outside.path()).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Bash({}/**)\"]\ndeny = []\nask = []\n",
            canonical_outside.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    let result = executor
        .execute(
            "execute_bash",
            &json!({"command": format!("cat {}", outside_file.to_string_lossy())}),
        )
        .await;

    assert!(
        result.is_ok(),
        "Bash with granted external path should work: {:?}",
        result
    );
    let text = result.unwrap().text().to_string();
    assert!(text.contains("bash content"));
}

#[tokio::test]
async fn test_edit_file_external_blocked_without_any_grant() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_file = outside.path().join("nowrite.txt");
    std::fs::write(&outside_file, "protected").unwrap();

    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        "[permissions]\nallow = []\ndeny = []\nask = []\n",
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    let result = executor
        .execute(
            "edit_file",
            &json!({
                "path": outside_file.to_string_lossy(),
                "old_string": "protected",
                "new_string": "hacked"
            }),
        )
        .await;

    assert!(result.is_err(), "Edit should be blocked without any grant");
    let content = std::fs::read_to_string(&outside_file).unwrap();
    assert_eq!(content, "protected");
}

#[tokio::test]
async fn test_bash_grant_does_not_allow_read_or_write() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_file = outside.path().join("bashonly.txt");
    std::fs::write(&outside_file, "bash data").unwrap();

    let canonical_outside = std::fs::canonicalize(outside.path()).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    // Only Bash grant
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Bash({}/**)\"]\ndeny = []\nask = []\n",
            canonical_outside.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    // read_file should be blocked (Bash grant doesn't imply Read)
    let read_result = executor
        .execute(
            "read_file",
            &json!({"path": outside_file.to_string_lossy()}),
        )
        .await;
    assert!(
        read_result.is_err(),
        "Read should be blocked — Bash grant doesn't imply Read"
    );

    // write_file should be blocked (Bash grant doesn't imply Write)
    let write_result = executor
        .execute(
            "write_file",
            &json!({"path": outside_file.to_string_lossy(), "content": "overwrite"}),
        )
        .await;
    assert!(
        write_result.is_err(),
        "Write should be blocked — Bash grant doesn't imply Write"
    );

    // File should be unchanged
    let content = std::fs::read_to_string(&outside_file).unwrap();
    assert_eq!(content, "bash data");
}

#[tokio::test]
async fn test_write_deny_overrides_allow() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_file = outside.path().join("denied.txt");
    std::fs::write(&outside_file, "original").unwrap();

    let canonical_outside = std::fs::canonicalize(outside.path()).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    // Allow Write to parent, but deny a specific file
    let canonical_file = std::fs::canonicalize(&outside_file).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Write({}/**)\"]\ndeny = [\"Write({})\"]\nask = []\n",
            canonical_outside.display(),
            canonical_file.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    let result = executor
        .execute(
            "write_file",
            &json!({"path": outside_file.to_string_lossy(), "content": "new content"}),
        )
        .await;

    assert!(result.is_err(), "Write should be blocked by deny rule");
    if let Err(SofosError::ToolExecution(msg)) = result {
        assert!(
            msg.contains("denied") || msg.contains("Denied"),
            "Error should mention deny: {}",
            msg
        );
    }
    // File should be unchanged
    let content = std::fs::read_to_string(&outside_file).unwrap();
    assert_eq!(content, "original");
}

#[tokio::test]
async fn test_read_external_absolute_path_blocked_without_grant() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_file = outside.path().join("noaccess.txt");
    std::fs::write(&outside_file, "private").unwrap();

    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        "[permissions]\nallow = []\ndeny = []\nask = []\n",
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    let result = executor
        .execute(
            "read_file",
            &json!({"path": outside_file.to_string_lossy()}),
        )
        .await;

    assert!(
        result.is_err(),
        "Read external should be blocked without grant"
    );
    if let Err(SofosError::ToolExecution(msg)) = result {
        assert!(
            msg.contains("outside workspace"),
            "Error should contain config hint: {}",
            msg
        );
        assert!(
            msg.contains("Read("),
            "Error should hint at Read scope: {}",
            msg
        );
    }
}

#[tokio::test]
async fn test_write_new_file_to_external_path() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    // File doesn't exist yet — only the parent directory exists
    let new_file = outside.path().join("brand_new.txt");
    assert!(!new_file.exists());

    let canonical_outside = std::fs::canonicalize(outside.path()).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Write({}/**)\"]\ndeny = []\nask = []\n",
            canonical_outside.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    let result = executor
        .execute(
            "write_file",
            &json!({"path": new_file.to_string_lossy(), "content": "created externally"}),
        )
        .await;

    assert!(
        result.is_ok(),
        "Writing new file to granted external path should work: {:?}",
        result
    );
    assert!(new_file.exists());
    let content = std::fs::read_to_string(&new_file).unwrap();
    assert_eq!(content, "created externally");
}

#[tokio::test]
async fn test_bash_partial_path_grant_blocks_ungranated_path() {
    let workspace = tempdir().unwrap();
    let allowed = tempdir().unwrap();
    let denied = tempdir().unwrap();

    let allowed_file = allowed.path().join("ok.txt");
    std::fs::write(&allowed_file, "allowed").unwrap();
    let denied_file = denied.path().join("nope.txt");
    std::fs::write(&denied_file, "denied").unwrap();

    let canonical_allowed = std::fs::canonicalize(allowed.path()).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    // Only grant Bash access to allowed dir, not denied dir
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Bash({}/**)\"]\ndeny = []\nask = []\n",
            canonical_allowed.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    // Command with both paths — denied path should block entire command
    let result = executor
        .execute(
            "execute_bash",
            &json!({
                "command": format!(
                    "cat {} {}",
                    allowed_file.to_string_lossy(),
                    denied_file.to_string_lossy()
                )
            }),
        )
        .await;

    assert!(
        result.is_err(),
        "Bash command should be blocked when any path is not granted"
    );
}

#[tokio::test]
async fn test_bash_deny_overrides_allow() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_sub = outside.path().join("secret");
    std::fs::create_dir_all(&outside_sub).unwrap();
    std::fs::write(outside_sub.join("file.txt"), "secret data").unwrap();

    let canonical_outside = std::fs::canonicalize(outside.path()).unwrap();
    let canonical_sub = std::fs::canonicalize(&outside_sub).unwrap();
    let config_dir = workspace.path().join(".sofos");
    std::fs::create_dir_all(&config_dir).unwrap();
    // Allow entire dir, but deny the secret subdirectory
    std::fs::write(
        config_dir.join("config.local.toml"),
        format!(
            "[permissions]\nallow = [\"Bash({}/**)\"]\ndeny = [\"Bash({}/**)\"]\nask = []\n",
            canonical_outside.display(),
            canonical_sub.display()
        ),
    )
    .unwrap();

    let executor =
        ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false).unwrap();

    let secret_file = canonical_sub.join("file.txt");
    let result = executor
        .execute(
            "execute_bash",
            &json!({"command": format!("cat {}", secret_file.display())}),
        )
        .await;

    assert!(
        result.is_err(),
        "Bash should be blocked by deny rule even with broader allow: {:?}",
        result
    );
    if let Err(SofosError::ToolExecution(msg)) = result {
        assert!(
            msg.contains("denied") || msg.contains("Denied"),
            "Error should mention deny: {}",
            msg
        );
    }
}
