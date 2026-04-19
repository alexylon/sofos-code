use crate::api::{CacheControl, Tool};
use serde_json::json;

fn read_file_tool() -> Tool {
    Tool::Regular {
        name: "read_file".to_string(),
        description: "Read the contents of a file. Works within the workspace by default. Can also read files outside the workspace — the user will be prompted to allow access if not already configured.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the file (e.g., 'src/main.rs'). Can also be absolute or ~/ paths for external files (user will be prompted for Read access)."
                }
            },
            "required": ["path"]
        }),
        cache_control: Some(CacheControl::ephemeral(None)),
    }
}

fn write_file_tool(has_morph: bool) -> Tool {
    let base = if has_morph {
        "Create a new file with the given content. For editing existing files, use morph_edit_file instead."
    } else {
        "Create a new file or overwrite an existing file with the given content."
    };
    let description = format!(
        "{base} Works within the workspace by default; can also write to external absolute or \
         ~/ paths (user will be prompted for Write access). \
         For files too large to emit in a single response, set `append: true` on subsequent \
         calls — the first call (append=false or omitted) creates/overwrites, and each \
         later call appends. This lets you split a large document into multiple \
         tool calls without hitting `max_output_tokens`."
    );

    Tool::Regular {
        name: "write_file".to_string(),
        description,
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the file (e.g., 'src/main.rs'). Can also be absolute or ~/ paths for external files (user will be prompted for Write access)."
                },
                "content": {
                    "type": "string",
                    "description": "The content to write (or append) to the file."
                },
                "append": {
                    "type": "boolean",
                    "description": "If true, append `content` to an existing file (creating it if missing) instead of overwriting. Use this to write a large document across several calls, each carrying a chunk of the final file.",
                    "default": false
                }
            },
            "required": ["path", "content"]
        }),
        cache_control: None,
    }
}

fn list_directory_tool() -> Tool {
    Tool::Regular {
        name: "list_directory".to_string(),
        description: "List files and directories in a single directory (non-recursive). Use this to explore a specific folder's contents. For finding files across multiple directories by pattern, use glob_files instead.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the directory (e.g., 'src' or '.'). Can also be absolute or ~/ paths for external directories (user will be prompted for Read access)."
                }
            },
            "required": ["path"]
        }),
        cache_control: None,
    }
}

fn create_directory_tool() -> Tool {
    Tool::Regular {
        name: "create_directory".to_string(),
        description: "Create a new directory (and parent directories if needed). Only works within the current project directory.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the directory (e.g., 'src/utils')"
                }
            },
            "required": ["path"]
        }),
        cache_control: None,
    }
}

fn anthropic_web_search_tool() -> Tool {
    Tool::AnthropicWebSearch {
        tool_type: "web_search_20250305".to_string(),
        name: "web_search".to_string(),
        max_uses: Some(5),
        allowed_domains: None,
        blocked_domains: None,
        cache_control: None,
    }
}

fn openai_web_search_tool() -> Tool {
    Tool::OpenAIWebSearch {
        tool_type: "web_search".to_string(),
    }
}

fn execute_bash_tool() -> Tool {
    Tool::Regular {
        name: "execute_bash".to_string(),
        description: "Execute a bash command in the workspace. Commands can reference external absolute or ~/ paths — the user will be prompted for Bash path access. Parent directory traversal (..) is always blocked. Never run destructive or irreversible shell commands (e.g., rm -rf, rm, rmdir, dd, mkfs*, fdisk/parted, wipefs, chmod/chown -R on broad paths, truncate, :>, >/dev/sd*, kill -9 on system services). Prefer read-only commands and dry-runs; if a potentially destructive action seems necessary, stop and request explicit confirmation before proceeding.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute (e.g., 'cargo test', 'ls -la', 'cat /path/to/file')"
                }
            },
            "required": ["command"]
        }),
        cache_control: None,
    }
}

fn delete_file_tool() -> Tool {
    Tool::Regular {
        name: "delete_file".to_string(),
        description: "Delete a file in the current workspace. A confirmation prompt will be shown to the user before deletion.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the file to delete (e.g., 'src/old_file.rs')"
                }
            },
            "required": ["path"]
        }),
        cache_control: None,
    }
}

fn delete_directory_tool() -> Tool {
    Tool::Regular {
        name: "delete_directory".to_string(),
        description: "Delete a directory and all its contents in the current workspace. A confirmation prompt will be shown to the user before deletion.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the directory to delete (e.g., 'src/old_module')"
                }
            },
            "required": ["path"]
        }),
        cache_control: None,
    }
}

fn move_file_tool() -> Tool {
    Tool::Regular {
        name: "move_file".to_string(),
        description: "Move or rename a file or directory within the workspace. Creates parent directories if needed.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "The relative path to the source file/directory (e.g., 'src/old_name.rs')"
                },
                "destination": {
                    "type": "string",
                    "description": "The relative path to the destination (e.g., 'src/new_name.rs' or 'new_folder/file.rs')"
                }
            },
            "required": ["source", "destination"]
        }),
        cache_control: None,
    }
}

fn copy_file_tool() -> Tool {
    Tool::Regular {
        name: "copy_file".to_string(),
        description: "Copy a file to a new location within the workspace. Creates parent directories if needed.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "The relative path to the source file (e.g., 'src/template.rs')"
                },
                "destination": {
                    "type": "string",
                    "description": "The relative path to the destination (e.g., 'src/new_file.rs')"
                }
            },
            "required": ["source", "destination"]
        }),
        cache_control: None,
    }
}

fn edit_file_tool() -> Tool {
    Tool::Regular {
        name: "edit_file".to_string(),
        description: "Make targeted edits to a file by replacing exact string matches. Preferred over write_file for modifying existing files — safer and more efficient since only the changed portion is specified. Works within the workspace by default; can also edit external files at absolute or ~/ paths (user will be prompted for Write access).".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the file (e.g., 'src/main.rs'). Can also be absolute or ~/ paths for external files."
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to find and replace. Must match the file content exactly, including whitespace and indentation."
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement text. Use an empty string to delete the matched text."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "If true, replace all occurrences. Default: false (replace first match only)."
                }
            },
            "required": ["path", "old_string", "new_string"]
        }),
        cache_control: None,
    }
}

fn glob_files_tool() -> Tool {
    Tool::Regular {
        name: "glob_files".to_string(),
        description: "Find files recursively by glob pattern. Use this when you need to find files across the codebase by name or extension (e.g., '**/*.rs', 'src/**/test_*.py'). For listing a single directory's contents, use list_directory instead.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern (e.g., '**/*.rs', 'src/**/mod.rs', '*.toml', 'tests/**/*.py')"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in, relative to workspace root. Default: '.' (entire workspace)"
                }
            },
            "required": ["pattern"]
        }),
        cache_control: None,
    }
}

fn web_fetch_tool() -> Tool {
    Tool::Regular {
        name: "web_fetch".to_string(),
        description: "Fetch a URL and return its content as readable text. Use this to read documentation pages, API references, or any web content. For searching the web by query, use web_search instead.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch (e.g., 'https://docs.rs/serde/latest/serde/')"
                }
            },
            "required": ["url"]
        }),
        cache_control: None,
    }
}

fn morph_edit_file_tool() -> Tool {
    // Schema matches the official Morph Fast Apply tool definition:
    // https://docs.morphllm.com/sdk/components/fast-apply
    // Field names (`target_filepath`, `instructions`, `code_edit`) and the
    // description are kept aligned with Morph's canonical schema so models
    // trained on it call the tool consistently across providers.
    Tool::Regular {
        name: "morph_edit_file".to_string(),
        description: "Edit an existing file by showing only the changed lines. \
            Use // ... existing code ... to represent unchanged sections. \
            Include just enough surrounding context to locate each edit precisely. \
            ALWAYS use the marker for unchanged sections (omitting it will cause deletions). \
            Preserve exact indentation. For deletions, show context before and after. \
            Batch multiple edits to the same file in one call. \
            Backed by the Morph Fast Apply API (10,500+ tokens/sec). \
            Supports absolute or ~/ paths for files outside the workspace \
            (user will be prompted for Write access)."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "target_filepath": {
                    "type": "string",
                    "description": "Path of the file to modify"
                },
                "instructions": {
                    "type": "string",
                    "description": "A single sentence written in the first person describing what the agent is changing. Used to help disambiguate uncertainty in the edit."
                },
                "code_edit": {
                    "type": "string",
                    "description": "Specify ONLY the precise lines of code that you wish to edit. Use // ... existing code ... for unchanged sections."
                }
            },
            "required": ["target_filepath", "instructions", "code_edit"]
        }),
        cache_control: None,
    }
}

/// Get available tools for Claude/GPT API
pub fn get_all_tools() -> Vec<Tool> {
    vec![
        list_directory_tool(),
        read_file_tool(),
        write_file_tool(false),
        edit_file_tool(),
        glob_files_tool(),
        create_directory_tool(),
        delete_file_tool(),
        delete_directory_tool(),
        move_file_tool(),
        copy_file_tool(),
        execute_bash_tool(),
        web_fetch_tool(),
        anthropic_web_search_tool(),
        openai_web_search_tool(),
    ]
}

pub fn get_all_tools_with_morph() -> Vec<Tool> {
    vec![
        list_directory_tool(),
        read_file_tool(),
        write_file_tool(true),
        edit_file_tool(),
        glob_files_tool(),
        create_directory_tool(),
        delete_file_tool(),
        delete_directory_tool(),
        move_file_tool(),
        copy_file_tool(),
        execute_bash_tool(),
        morph_edit_file_tool(),
        web_fetch_tool(),
        anthropic_web_search_tool(),
        openai_web_search_tool(),
    ]
}

pub fn get_read_only_tools() -> Vec<Tool> {
    vec![
        list_directory_tool(),
        read_file_tool(),
        glob_files_tool(),
        web_fetch_tool(),
        // Anthropic web search tool
        anthropic_web_search_tool(),
        // OpenAI web search tool
        openai_web_search_tool(),
    ]
}

/// Add code search tool to an existing tool list
pub fn add_code_search_tool(tools: &mut Vec<Tool>) {
    tools.push(Tool::Regular {
        name: "search_code".to_string(),
        description: "Search for patterns in code using ripgrep. Supports regex patterns and file type filtering. Fast search across the entire codebase.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The search pattern (regex supported, e.g., 'fn main', 'struct.*User')"
                },
                "file_type": {
                    "type": "string",
                    "description": "Optional file type filter (e.g., 'rust', 'py', 'js', 'ts'). See ripgrep docs for available types."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results per file (default: 50)"
                }
            },
            "required": ["pattern"]
        }),
        cache_control: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_regular_tool_serialization() {
        let tool = read_file_tool();
        let serialized = serde_json::to_value(&tool).expect("Failed to serialize regular tool");

        // Regular tools should have name, description, and input_schema
        assert_eq!(serialized["name"], "read_file");
        assert!(serialized.get("description").is_some());
        assert!(serialized.get("input_schema").is_some());

        // Regular tools should NOT have a type field at the root
        assert!(serialized.get("type").is_none());
    }

    #[test]
    fn test_anthropic_web_search_tool_serialization() {
        let tool = anthropic_web_search_tool();
        let serialized = serde_json::to_value(&tool).expect("Failed to serialize web search tool");

        // Verify the correct structure for Claude's server-side web search
        assert_eq!(serialized["type"], "web_search_20250305");
        assert_eq!(serialized["name"], "web_search");
        assert_eq!(serialized["max_uses"], 5);

        // Ensure it has the correct type identifier
        assert!(serialized.get("type").is_some());
    }

    #[test]
    fn test_openai_web_search_tool_serialization() {
        let tool = openai_web_search_tool();
        let serialized = serde_json::to_value(&tool).expect("Failed to serialize web search tool");

        // Verify the correct structure for OpenAI's server-side web search
        assert_eq!(serialized["type"], "web_search");

        // Ensure it has the correct type identifier
        assert!(serialized.get("type").is_some());
    }
}
