use crate::api::Tool;
use serde_json::json;

fn read_file_tool() -> Tool {
    Tool {
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
    }
}

fn write_file_tool(has_morph: bool) -> Tool {
    let description = if has_morph {
        "Create a new file with the given content. For editing existing files, use morph_edit_file instead. Only works within the current project directory."
    } else {
        "Create a new file or overwrite an existing file with the given content. Only works within the current project directory."
    };

    Tool {
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
    }
}

fn list_directory_tool() -> Tool {
    Tool {
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
    }
}

fn create_directory_tool() -> Tool {
    Tool {
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
    }
}

fn web_search_tool() -> Tool {
    Tool {
        name: "web_search".to_string(),
        description: "Search the web for information using DuckDuckGo. Use this when you need current information, documentation, or want to research something.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5)",
                    "default": 5
                }
            },
            "required": ["query"]
        }),
    }
}

fn execute_bash_tool() -> Tool {
    Tool {
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
    }
}

fn delete_file_tool() -> Tool {
    Tool {
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
    }
}

fn delete_directory_tool() -> Tool {
    Tool {
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
    }
}

fn move_file_tool() -> Tool {
    Tool {
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
    }
}

fn copy_file_tool() -> Tool {
    Tool {
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
    }
}

fn morph_edit_file_tool() -> Tool {
    Tool {
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
    }
}

/// Get all available tools for Claude API
pub fn get_tools() -> Vec<Tool> {
    vec![
        read_file_tool(),
        write_file_tool(false),
        list_directory_tool(),
        create_directory_tool(),
        delete_file_tool(),
        delete_directory_tool(),
        move_file_tool(),
        copy_file_tool(),
        web_search_tool(),
        execute_bash_tool(),
    ]
}

/// Get all available tools including Morph-powered fast editing
pub fn get_tools_with_morph() -> Vec<Tool> {
    vec![
        read_file_tool(),
        write_file_tool(true),
        list_directory_tool(),
        create_directory_tool(),
        delete_file_tool(),
        delete_directory_tool(),
        move_file_tool(),
        copy_file_tool(),
        web_search_tool(),
        execute_bash_tool(),
        morph_edit_file_tool(),
    ]
}

/// Add code search tool to an existing tool list
pub fn add_code_search_tool(tools: &mut Vec<Tool>) {
    tools.push(Tool {
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
    });
}
