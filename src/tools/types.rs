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
