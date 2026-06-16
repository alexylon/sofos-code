use crate::api::Tool;
use serde_json::json;

fn read_file_tool() -> Tool {
    Tool::Regular {
        name: "read_file".to_string(),
        description: "Read the contents of a file. Works within the workspace by default. Can also read files outside the workspace — the user will be prompted to allow access if not already configured. Files larger than 50 MB are rejected outright, and the returned content is itself capped (~64 KB) before being passed to the model, so for very large logs prefer `search_code` or page through with `execute_bash` (`head`, `sed -n 'A,Bp'`).".to_string(),
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
        cache_control: None,
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
        description: "Create a new directory (and parent directories if needed). Supports absolute or ~/ paths for external directories (user will be prompted for Write access).".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the directory (e.g., 'src/utils'). Can also be absolute or ~/ paths for external directories (user will be prompted for Write access)."
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
        description: "Execute a bash command in the workspace. Use the shell freely for project work — builds, tests, scripts, and creating, overwriting, or editing files inside the workspace are all expected and safe. In the default mode commands run confined by the operating system: their writes cannot leave the workspace and they have no network access. Commands may reference external absolute or ~/ paths (the user is prompted for access). Parent directory traversal (..) is always blocked. Do not run irreversible or system-wide commands (e.g., rm -rf, rm, rmdir, dd, mkfs*, fdisk/parted, wipefs, chmod/chown -R on broad paths, truncate, :>, >/dev/sd*, kill -9 on system services); if one seems genuinely necessary, stop and request explicit confirmation first.".to_string(),
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
        description: "Delete a file. Works within the workspace by default and also supports external absolute or ~/ paths after the user grants Write access to the target directory. A confirmation prompt is shown before deletion in every case.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to delete. Relative paths resolve inside the workspace; absolute and ~/ paths can target external files when Write access is granted."
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
        description: "Delete a directory and all its contents. Works within the workspace by default and also supports external absolute or ~/ paths after the user grants Write access to the target directory. A confirmation prompt is shown before deletion in every case.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the directory to delete. Relative paths resolve inside the workspace; absolute and ~/ paths can target external directories when Write access is granted."
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
        description: "Move or rename a file or directory. Creates parent directories if needed. Supports absolute or ~/ paths for either endpoint (user will be prompted for Write access on external paths, since moving removes the source).".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "The relative path to the source file/directory (e.g., 'src/old_name.rs'). Can also be absolute or ~/ paths; external sources require a Write grant (move removes the source)."
                },
                "destination": {
                    "type": "string",
                    "description": "The relative path to the destination (e.g., 'src/new_name.rs' or 'new_folder/file.rs'). Can also be absolute or ~/ paths; external destinations require a Write grant."
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
        description: "Copy a file to a new location. Creates parent directories if needed. Supports absolute or ~/ paths for either endpoint (user will be prompted for Read on external sources and Write on external destinations).".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "The relative path to the source file (e.g., 'src/template.rs'). Can also be absolute or ~/ paths; external sources require a Read grant."
                },
                "destination": {
                    "type": "string",
                    "description": "The relative path to the destination (e.g., 'src/new_file.rs'). Can also be absolute or ~/ paths; external destinations require a Write grant."
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
        description: "Make targeted edits to a file by replacing exact string matches. Preferred over write_file for modifying existing files — safer and more efficient since only the changed portion is specified. Works within the workspace by default; can also edit external files at absolute or ~/ paths (the user will be prompted for both Read and Write access on the target directory because the tool reads the file before writing it back).".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path to the file (e.g., 'src/main.rs'). Can also be absolute or ~/ paths for external files."
                },
                "old_string": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The exact text to find and replace. Must match the file content exactly, including whitespace and indentation. Must be unique unless replace_all is true."
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement text. Use an empty string to delete the matched text."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "If true, replace all occurrences. Default: false; when false, old_string must match exactly one occurrence."
                }
            },
            "required": ["path", "old_string", "new_string"]
        }),
        cache_control: None,
    }
}

fn glob_files_tool() -> Tool {
    let excludes = crate::tools::codesearch::default_exclude_dirs_human();
    let tool_description = format!(
        "Find files recursively by glob pattern. Use this when you need to find files across the codebase by name or extension (e.g., '**/*.rs', 'src/**/test_*.py'). For listing a single directory's contents, use list_directory instead. By default skips build/vendored directories ({excludes}) and does not follow symlinks (matching ripgrep's default); set include_ignored=true or follow_symlinks=true to widen the walk."
    );
    let include_ignored_description = format!(
        "When true, descend into the default build/vendored excludes ({excludes}). Default: false. Only set this when you specifically need to find files inside build artefacts or vendored code."
    );

    Tool::Regular {
        name: "glob_files".to_string(),
        description: tool_description,
        input_schema: json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern (e.g., '**/*.rs', 'src/**/mod.rs', '*.toml', 'tests/**/*.py')"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in, relative to workspace root. Default: '.' (entire workspace). Can also be absolute or ~/ paths for external directories (user will be prompted for Read access)."
                },
                "include_ignored": {
                    "type": "boolean",
                    "description": include_ignored_description
                },
                "follow_symlinks": {
                    "type": "boolean",
                    "description": "When true, follow symlinks while walking (equivalent to `rg -L`). Default: false — matches ripgrep's default and prevents a workspace-internal symlink that points outside from leaking filenames under the target directory."
                }
            },
            "required": ["pattern"]
        }),
        cache_control: None,
    }
}

fn update_plan_tool() -> Tool {
    Tool::Regular {
        name: "update_plan".to_string(),
        description: "Update the visible task plan. Provide an optional explanation and the complete current plan. Each item has a step and a status. At most one item may be in_progress at a time.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "explanation": {
                    "type": "string",
                    "description": "Optional short note explaining why the plan changed or what was just completed."
                },
                "plan": {
                    "type": "array",
                    "description": "The complete current plan, in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "step": {
                                "type": "string",
                                "description": "A concise task step."
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "The step status. Use in_progress for at most one step."
                            }
                        },
                        "required": ["step", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["plan"],
            "additionalProperties": false
        }),
        cache_control: None,
    }
}

fn view_image_tool() -> Tool {
    let description = format!(
        "Attach an image to the conversation so you can see it. Use this when the user references a screenshot, diagram, photo, or other image by path or URL. \
         For a folder of images, call list_directory first to discover the files, then call view_image once per image. Passing a folder directly is rejected with a hint to do that. \
         Supports {formats} up to {max_mb} MB per file; larger images are resized to fit within {max_dim} pixels on the long side before they reach the model. \
         Animated GIFs are decoded but only the first frame is sent — for an animation, ask the user for a still frame instead. \
         Local paths can be workspace-relative, absolute, or use ~/; external paths prompt for Read access the first time. \
         HTTP/HTTPS URLs are passed through to the model provider, which fetches them on its side. \
         `data:` URLs are NOT accepted; the user must save the image to a path or expose it as http(s)://.",
        formats = crate::tools::image::SUPPORTED_FORMATS_HUMAN_LIST,
        max_mb = crate::tools::image::MAX_IMAGE_SIZE_MB,
        max_dim = crate::tools::image::MAX_PROMPT_IMAGE_DIMENSION,
    );

    Tool::Regular {
        name: "view_image".to_string(),
        description,
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "A local image filepath (workspace-relative, absolute, or ~/), or an http(s):// URL to an image. Directories are not accepted — list the directory first and call view_image on the file."
                }
            },
            "required": ["path"],
            "additionalProperties": false
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
            (the user will be prompted for both Read and Write access on the target directory \
            because the tool reads the file before writing it back)."
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
        update_plan_tool(),
        view_image_tool(),
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
        update_plan_tool(),
        view_image_tool(),
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
        update_plan_tool(),
        view_image_tool(),
        web_fetch_tool(),
        // Anthropic web search tool
        anthropic_web_search_tool(),
        // OpenAI web search tool
        openai_web_search_tool(),
    ]
}

/// Add code search tool to an existing tool list
pub fn add_code_search_tool(tools: &mut Vec<Tool>) {
    let excludes = crate::tools::codesearch::default_exclude_dirs_human();
    let tool_description = format!(
        "Search for patterns in code using ripgrep. Supports regex patterns and file type filtering. Fast search across the entire codebase. By default skips build/vendored directories ({excludes}) and respects .gitignore; set include_ignored=true to search everywhere."
    );
    let max_results_description = format!(
        "Maximum results per file (default: {})",
        crate::tools::codesearch::DEFAULT_MAX_RESULTS_PER_FILE
    );
    let include_ignored_description = format!(
        "When true, bypass the default build/vendored excludes ({excludes}) and ignore files (.gitignore, .ignore). Default: false. Only set this when you specifically need to grep inside build artefacts or vendored code."
    );

    tools.push(Tool::Regular {
        name: "search_code".to_string(),
        description: tool_description,
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
                    "description": max_results_description
                },
                "include_ignored": {
                    "type": "boolean",
                    "description": include_ignored_description
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
    fn update_plan_available_in_read_only_tools() {
        let tools = get_read_only_tools();
        assert!(tools.iter().any(|tool| match tool {
            Tool::Regular { name, .. } => name == "update_plan",
            _ => false,
        }));
    }

    #[test]
    fn update_plan_schema_restricts_status_values() {
        let tools = get_all_tools();
        let plan_tool = tools
            .iter()
            .find(|tool| matches!(tool, Tool::Regular { name, .. } if name == "update_plan"))
            .expect("update_plan tool should be registered");

        let Tool::Regular { input_schema, .. } = plan_tool else {
            panic!("update_plan must be a regular tool");
        };

        assert_eq!(
            input_schema["properties"]["plan"]["items"]["properties"]["status"]["enum"],
            json!(["pending", "in_progress", "completed"])
        );
    }

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
