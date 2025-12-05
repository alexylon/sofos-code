use crate::api::Tool;
use serde_json::json;

/// Get all available tools for Claude API
pub fn get_tools() -> Vec<Tool> {
    vec![
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
        },
        Tool {
            name: "write_file".to_string(),
            description: "Create a new file or overwrite an existing file with the given content. Only works within the current project directory.".to_string(),
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
        },
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
        },
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
        },
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
        },
    ]
}

/// Get all available tools including Morph-powered fast editing
pub fn get_tools_with_morph() -> Vec<Tool> {
    let mut tools = vec![
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
        },
        Tool {
            name: "write_file".to_string(),
            description: "Create a new file with the given content. For editing existing files, use edit_file_fast instead. Only works within the current project directory.".to_string(),
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
        },
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
        },
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
        },
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
        },
    ];
    
    tools.push(Tool {
        name: "edit_file_fast".to_string(),
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
    });
    tools
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
