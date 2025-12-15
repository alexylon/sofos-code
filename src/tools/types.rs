use crate::api::{CacheControl, Tool};
use serde_json::json;

fn read_file_tool() -> Tool {
    Tool::Regular {
        name: "read_file".to_string(),
        description: "Read the contents of a file in the current workspace. Only works for files within the current project directory.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the file (e.g., 'src/main.rs' or 'README.md')"
                }
            },
            "required": ["path"]
        }),
        cache_control: Some(CacheControl::ephemeral(None)),
    }
}

fn write_file_tool(has_morph: bool) -> Tool {
    let description = if has_morph {
        "Create a new file with the given content. For editing existing files, use morph_edit_file instead. Only works within the current project directory."
    } else {
        "Create a new file or overwrite an existing file with the given content. Only works within the current project directory."
    };

    Tool::Regular {
        name: "write_file".to_string(),
        description: description.to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the file (e.g., 'src/main.rs' or 'README.md')"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
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
        description: "List all files and directories in a given path. Only works within the current project directory.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the directory (e.g., 'src' or '.')"
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
        description: "Execute read-only bash commands for testing code. Commands are sandboxed to the current directory. Forbidden: sudo, file modification commands (rm, mv, cp, chmod, etc.), and output redirection. Use this to run tests, check program output, or verify code behavior.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute (e.g., 'cargo test', 'ls -la', 'cat file.txt')"
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

fn morph_edit_file_tool() -> Tool {
    Tool::Regular {
        name: "morph_edit_file".to_string(),
        description: "**PREFERRED FOR EDITING FILES** - Ultra-fast file editing using Morph Apply API (10,500+ tokens/sec, 96-98% accuracy). Use this for ALL modifications to existing files. Provide the instruction, original code, and your proposed changes with '// ... existing code ...' markers for unchanged sections. Much more efficient than write_file.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the file (e.g., 'src/main.rs')"
                },
                "instruction": {
                    "type": "string",
                    "description": "Brief first-person description of what you're changing (e.g., 'I will add error handling')"
                },
                "code_edit": {
                    "type": "string",
                    "description": "The updated code showing only changes, using '// ... existing code ...' for unchanged sections"
                }
            },
            "required": ["path", "instruction", "code_edit"]
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
        create_directory_tool(),
        delete_file_tool(),
        delete_directory_tool(),
        move_file_tool(),
        copy_file_tool(),
        execute_bash_tool(),
        // Anthropic web search tool
        anthropic_web_search_tool(),
        // OpenAI web search tool
        openai_web_search_tool(),
    ]
}

pub fn get_all_tools_with_morph() -> Vec<Tool> {
    vec![
        list_directory_tool(),
        read_file_tool(),
        write_file_tool(true),
        create_directory_tool(),
        delete_file_tool(),
        delete_directory_tool(),
        move_file_tool(),
        copy_file_tool(),
        execute_bash_tool(),
        morph_edit_file_tool(),
        // Anthropic web search tool
        anthropic_web_search_tool(),
        // OpenAI web search tool
        openai_web_search_tool(),
    ]
}

pub fn get_read_only_tools() -> Vec<Tool> {
    vec![
        list_directory_tool(),
        read_file_tool(),
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
