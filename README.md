# Sofos Code

![](https://github.com/alexylon/sofos-code/actions/workflows/rust.yml/badge.svg)

A blazingly fast, interactive AI coding assistant powered by Claude, implemented in pure Rust, that can generate code, edit files, and search the web - all from your terminal.

<div align="center"><img align="center" src="/assets/sofos_code.png" width="700" alt="Ferrocrypt"></div>

## Features

- **Interactive REPL** - Multi-turn conversations with Claude
- **File Operations** - Read, write, list, and create files/directories (sandboxed to current directory)
- **Ultra-Fast Editing** - Optional Morph Apply integration (10,500+ tokens/sec, 96-98% accuracy)
- **Code Search** - Fast regex-based code search using `ripgrep` (optional)
- **Web Search** - Real-time information via DuckDuckGo
- **Bash Execution** - Run tests and build commands safely (read-only, sandboxed)
- **Secure** - All operations restricted to workspace, prevents directory traversal

## Installation

**Requirements:** 

- Rust 1.70+
- Anthropic API key ([get one](https://console.anthropic.com/))

**Optional:** 

- `ripgrep` for code search ([install guide](https://github.com/BurntSushi/ripgrep#installation))
- Morph API key ([get one](https://morphllm.com/))

```bash
git clone https://github.com/alexylon/sofos-code.git
cd sofos-code
cargo install --path .
```

## Usage

### Set API key

```bash
export ANTHROPIC_API_KEY='your-api-key'
```

### Enable Ultra-Fast Editing (Optional)

When enabled, Claude can use the `morph_edit_file` tool for lightning-fast, accurate code modifications.

```bash
export MORPH_API_KEY='your-morph-key'
```

### Start interactive mode

```bash
sofos
```

### One-shot mode

```bash
sofos --prompt "Create a hello world Rust program"
```

### Commands

- `clear` - Clear conversation history
- `exit` or `quit` - Exit
- `Ctrl+D` - Exit

### Options

```
--api-key <KEY>         Anthropic API key (overrides ANTHROPIC_API_KEY)
--morph-api-key <KEY>   Morph API key (overrides MORPH_API_KEY)
-p, --prompt <TEXT>     One-shot mode
--model <MODEL>         Claude model (default: claude-sonnet-4-5)
--morph-model <MODEL>   Morph model (default: morph-v3-fast)
--max-tokens <N>        Max response tokens (default: 4096)
-v, --verbose           Verbose logging
```

## Available Tools

Claude can automatically use these tools:

**File Operations:**
- `read_file` - Read file contents
- `write_file` - Create or overwrite files
- `morph_edit_file` - Ultra-fast code editing (requires MORPH_API_KEY)
- `list_directory` - List directory contents
- `create_directory` - Create directories
- `delete_file` - Delete files (asks for confirmation)
- `delete_directory` - Delete directories recursively (asks for confirmation)
- `move_file` - Move or rename files/directories
- `copy_file` - Copy files

**Code & Search:**
- `search_code` - Fast regex-based code search (requires `ripgrep`)
- `web_search` - Search the internet
- `execute_bash` - Run tests and build commands (read-only, sandboxed)

## Security

All file operations are sandboxed to your current working directory:

- ✅ Can access files in current directory and subdirectories
- ❌ Cannot access parent directories (`../`)
- ❌ Cannot access absolute paths (`/etc/passwd`)
- ❌ Cannot follow symlinks outside workspace

Bash command execution is restricted to read-only operations:

- ✅ Can run tests and build commands (`cargo test`, `cargo build`)
- ✅ Can read files and list directories (`cat`, `ls`, `grep`)
- ❌ Cannot use `sudo` or privilege escalation
- ❌ Cannot modify files (`rm`, `mv`, `cp`, `chmod`, `mkdir`, `touch`)
- ❌ Cannot access absolute paths or parent directories
- ❌ Cannot change directories (`cd`, `pushd`, `popd`)
- ❌ Cannot use output redirection (`>`, `>>`)

**Best Practice:** Run `sofos` from your project directory, use git to track changes.

## Development

```bash
# Run tests
cargo test

# Build release
cargo build --release

# Debug logging
RUST_LOG=debug sofos
```

## Troubleshooting

**API errors:** Check internet connection and API key

**Path errors:** Use relative paths only, no `..` or absolute paths

**Build errors:** Run `rustup update && cargo clean && cargo build`

## License

MIT License

## Morph Integration

Sofos integrates with Morph's Apply API for ultra-fast code editing:

- **10,500+ tokens/sec** - Lightning-fast edits
- **96-98% accuracy** - Reliable code modifications
- **Direct REST API** - No additional dependencies
- **Optional** - Enable with `MORPH_API_KEY`

Uses the `morph_edit_file` tool when available.

## Acknowledgments

Built with Rust and powered by Anthropic's Claude. Morph Apply integration for fast edits. Inspired by Aider and similar tools.

---

**Disclaimer:** Sofos Code may make mistakes. Always review generated code before use.

[![forthebadge](https://forthebadge.com/images/badges/made-with-rust.svg)](https://forthebadge.com)
