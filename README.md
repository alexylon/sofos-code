# Sofos Code - AI-Powered CLI Coding Assistant

An interactive AI coding assistant powered by Claude that can write code, edit files, and search the web - all from your terminal.

## Features

- **Interactive REPL** - Multi-turn conversations with Claude
- **File Operations** - Read, write, list, and create files/directories (sandboxed to current directory)
- **Web Search** - Real-time information via DuckDuckGo
- **Secure** - All file operations restricted to workspace, prevents directory traversal

## Installation

**Requirements:** Rust 1.70+, Anthropic API key ([get one](https://console.anthropic.com/))

```bash
git clone https://github.com/alexylon/sofos-code.git
cd sofos-code
cargo install --path .
```

## Usage

```bash
# Set API key
export ANTHROPIC_API_KEY='your-api-key'

# Start interactive mode
sofos

# One-shot mode
sofos --prompt "Create a hello world Python script"
```

### Commands

- `clear` - Clear conversation history
- `exit` or `quit` - Exit
- `Ctrl+D` - Exit

### Options

```
--api-key <KEY>     API key (overrides env var)
-p, --prompt <TEXT> One-shot mode
--model <MODEL>     Claude model (default: claude-3-5-sonnet-20241022)
--max-tokens <N>    Max response tokens (default: 4096)
-v, --verbose       Verbose logging
```

## Available Tools

Claude can automatically use these tools:

- `read_file` - Read file contents
- `write_file` - Create or overwrite files
- `list_directory` - List directory contents
- `create_directory` - Create directories
- `web_search` - Search the internet

## Security

All file operations are sandboxed to your current working directory:

- ✅ Can access files in current directory and subdirectories
- ❌ Cannot access parent directories (`../`)
- ❌ Cannot access absolute paths (`/etc/passwd`)
- ❌ Cannot follow symlinks outside workspace

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

## Acknowledgments

Built with Rust and powered by Anthropic's Claude. Inspired by Aider and similar tools.

---

**Disclaimer:** Sofos Code may make mistakes. Always review generated code before use.

[![forthebadge](https://forthebadge.com/images/badges/made-with-rust.svg)](https://forthebadge.com)
