# Sofos Code

![](https://github.com/alexylon/sofos-code/actions/workflows/rust.yml/badge.svg) &nbsp; [![Crates.io](https://img.shields.io/crates/v/sofos.svg?color=blue)](https://crates.io/crates/sofos)

A blazingly fast, interactive AI coding assistant powered by Claude or GPT, implemented in pure Rust, that can generate code, edit files, and search the web - all from your terminal.

<div align="center"><img src="/assets/sofos_code.png" style="width: 800px;" alt="Sofos Code"></div>

## Table of Contents

- [Features](#features)
- [Installation](#installation)
- [Usage](#usage)
  - [Quick Start](#quick-start)
  - [Commands](#commands)
  - [Image Vision](#image-vision)
  - [Cost Tracking](#cost-tracking)
  - [CLI Options](#cli-options)
  - [Extended Thinking](#extended-thinking)
- [Custom Instructions](#custom-instructions)
- [Session History](#session-history)
- [Available Tools](#available-tools)
- [Security](#security)
  - [Configuration](#configuration)
- [Development](#development)
- [Troubleshooting](#troubleshooting)
- [Morph Integration](#morph-integration)
- [License](#license)
- [Acknowledgments](#acknowledgments)
- [Links & Resources](#links--resources)

## Features

- **Interactive REPL** - Multi-turn conversations with Claude or GPT
- **Markdown Formatting** - AI responses with syntax highlighting for code blocks
- **Image Vision** - Analyze local or web images
- **Session History** - Auto-save and resume conversations
- **Custom Instructions** - Project and personal context files
- **File Operations** - Read, write, list, create (sandboxed)
- **Ultra-Fast Editing** - Optional Morph Apply integration (10,500+ tokens/sec)
- **Code Search** - Fast regex search with ripgrep
- **Web Search** - Real-time info via Claude's/OpenAI's native search
- **Bash Execution** - Run tests and builds (read-only, sandboxed)
- **Visual Diffs** - Colored change display
- **Iterative Tools** - Up to 200 tool calls per request
- **Cost Tracking** - Session token usage and cost estimates
- **Safe Mode** - Read-only operation mode

## Installation

**Requirements:** Anthropic API key ([get one](https://console.anthropic.com/)) or OpenAI API key ([get one](https://platform.openai.com/))

**Optional** (but highly recommended): `ripgrep` for code search ([install](https://github.com/BurntSushi/ripgrep#installation)), Morph API key for ultra-fast editing ([get one](https://morphllm.com/))

**Install:**

```bash
# Homebrew (could be behind `cargo install`)
brew tap alexylon/tap && brew install sofos

# Cargo (requires Rust 1.70+)
cargo install sofos

# From source
git clone https://github.com/alexylon/sofos-code.git
cd sofos-code && cargo install --path .
```

**Important:** Add `.sofos/` to `.gitignore` (contains session history and personal settings). Keep `.sofosrc` (team-wide instructions).

## Usage

### Quick Start

```bash
# Set your API key (choose one)
export ANTHROPIC_API_KEY='your-anthropic-key'
# or
export OPENAI_API_KEY='your-openai-key'

# Optional: Enable ultra-fast editing
export MORPH_API_KEY='your-morph-key'

# Start Sofos
sofos
```

### Commands

- `/resume` - Resume previous session
- `/clear` - Clear conversation history
- `/think [on|off]` - Toggle extended thinking (shows status if no arg)
- `/s` - Safe mode (read-only, prompt: **`λ:`**, blinking underscore (`_`) cursor)
- `/n` - Normal mode (all tools, prompt: **`λ>`**, default cursor)
- `/exit`, `/quit`, `/q`, `Ctrl+D` - Exit with cost summary
- `ESC` - Interrupt AI response

**Tab completion:** Press Tab for command suggestions, Shift+Tab to navigate backwards.

### Image Vision

Include image paths or URLs directly in your message:

```bash
# Local images
What's in this screenshot.png?
Describe ./images/diagram.jpg

# Paths with spaces - use quotes
What do you see in "/Users/alex/Documents/my image.png"?

# Web images
Analyze https://example.com/chart.png
```

**Formats:** JPEG, PNG, GIF, WebP (max 20MB local) | **Spaces:** Wrap in quotes `"path/with space.png"` | **Permissions:** Outside workspace requires config

### Cost Tracking

Exit summary shows token usage and estimated cost (based on official API pricing).

### CLI Options

```
-p, --prompt <TEXT>          One-shot mode
-s, --safe-mode              Start in read-only mode (only read/list/web-search/image tools; no writes or bash commands)
-r, --resume                 Resume a previous session
    --api-key <KEY>          Anthropic API key (overrides env var)
    --openai-api-key <KEY>   OpenAI API key (overrides env var)
    --morph-api-key <KEY>    Morph API key (overrides env var)
    --model <MODEL>          Model to use (default: claude-sonnet-4-5)
    --morph-model <MODEL>    Morph model (default: morph-v3-fast)
    --max-tokens <N>         Max response tokens (default: 8192)
-t, --enable-thinking        Enable extended thinking (default: false)
    --thinking-budget <N>    Token budget for thinking (Claude only, default: 5120, must be < max-tokens)
-v, --verbose                Verbose logging
```

### Extended Thinking

Enable for complex reasoning tasks (disabled by default):

```bash
sofos -t                                             # Default 5120 token budget (Claude only)
sofos -t --thinking-budget 10000 --max-tokens 16000  # Custom budget (Claude only)
```

**Note:** Extended thinking works with both Claude and OpenAI models. 
For Claude, it enables the thinking protocol and `--thinking-budget` controls token allocation. 
For OpenAI (gpt-5 models), `/think on` sets high reasoning effort and `/think off` sets low reasoning effort. 
The `--thinking-budget` parameter only applies to Claude models.

## Custom Instructions

**`.sofosrc`** (project root, version controlled) - Team-wide conventions, architecture  
**`.sofos/instructions.md`** (gitignored) - Personal preferences

Both loaded at startup and appended to system prompt.

## Session History

Conversations auto-saved to `.sofos/sessions/`. Resume with `sofos -r` or `/resume`.

## Available Tools

**File Operations:**
- `read_file` - Read file contents
- `write_file` - Create or overwrite files
- `morph_edit_file` - Ultra-fast code editing (requires MORPH_API_KEY)
- `list_directory` - List directory contents
- `create_directory`, `delete_file`, `delete_directory`, `move_file`, `copy_file` - Standard file ops

**Code & Search:**
- `search_code` - Fast regex-based code search (requires `ripgrep`)
- `web_search` - Real-time web information via Claude's/OpenAI's native search
- `execute_bash` - Run tests and build commands (read-only, sandboxed)

**Image Vision:**
- `image` - View and analyze images (JPEG, PNG, GIF, WebP)

**Note:** Only `read_file`, `list_directory`, and `image` can access paths outside workspace when explicitly allowed in config. All other operations are workspace-only.

Safe mode (`--safe-mode` or `/s`) restricts to: `list_directory`, `read_file`, `web_search`, `image`.

## Security

**Sandboxing (by default):**
- ✅ Full access to workspace files/directories
- ✅ Read-only access to outside workspace (requires explicit config)
- ❌ No writes, moves, or deletes outside workspace
- ❌ Bash always sandboxed to workspace

**Bash Permissions (3-Tier System):**

1. **Allowed (auto-execute):** Build tools (cargo, npm, go), read-only commands (ls, cat, grep), system info (pwd, date), git read-only
2. **Forbidden (always blocked):** rm, mv, cp, chmod, sudo, mkdir, cd, kill, shutdown
3. **Ask (prompt user):** Unknown commands require approval; can be remembered in config

### Configuration

Permissions are stored in `.sofos/config.local.toml` (workspace-specific, gitignored) or `~/.sofos/config.toml` (global, optional). Local config overrides global.

**Example:**

```toml
[permissions]
allow = [
  # Read permissions - for accessing files/directories outside workspace
  "Read(~/.zshrc)",           # Specific file
  "Read(~/.config/**)",       # Recursive
  "Read(/etc/hosts)",         # Absolute path
  
  # Bash permissions - for command execution
  "Bash(custom_command)",
  "Bash(pattern:*)",
]

deny = [
  # Read denials
  "Read(./.env)",
  "Read(./.env.*)",
  "Read(./secrets/**)",
  
  # Bash denials
  "Bash(dangerous_command)",
]

ask = [
  # Only for Bash commands (prompts for approval)
  "Bash(unknown_tool)",
]
```

**Rules:**
- Workspace files: allowed by default unless in `deny` list
- Outside workspace: denied by default unless in `allow` list
- Glob patterns supported: `*` (single level), `**` (recursive)
- Tilde expansion: `~` → home directory
- `ask` only works for Bash commands, not Read permissions

## Development

```bash
cargo test                    # Run tests
cargo build --release         # Build release
RUST_LOG=debug sofos          # Debug logging
```

**Structure:** `src/` (api, tools, commands, repl, ui, conversation, history, config, etc.), `tests/`, `assets/`, `.sofos/` (gitignored), `.sofosrc` (version controlled)

See `.sofosrc` for detailed conventions.

## Troubleshooting

- **API errors:** Check connection and API key
- **Path errors:** Use relative paths for workspace, or add `Read(path)` to config for outside access
- **Build errors:** `rustup update && cargo clean && cargo build`
- **Images with spaces:** Wrap path in quotes

## Morph Integration

Optional ultra-fast code editing via Morph Apply API (10,500+ tokens/sec, 96-98% accuracy). Enable with `MORPH_API_KEY`.

## License

MIT License

## Acknowledgments

Built with Rust and powered by Anthropic's Claude or OpenAI's GPT. Morph Apply integration for fast edits. Inspired by Aider and similar tools.

## Links & Resources

- [GitHub](https://github.com/alexylon/sofos-code)
- [Crates.io](https://crates.io/crates/sofos)

---

**Disclaimer:** Sofos Code may make mistakes. Always review generated code before use.

[![forthebadge](https://forthebadge.com/images/badges/made-with-rust.svg)](https://forthebadge.com)
