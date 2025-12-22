# Sofos Code

![](https://github.com/alexylon/sofos-code/actions/workflows/rust.yml/badge.svg) &nbsp; [![Crates.io](https://img.shields.io/crates/v/sofos.svg?color=blue)](https://crates.io/crates/sofos)

A blazingly fast, interactive AI coding assistant powered by Claude or GPT, implemented in pure Rust, that can generate code, edit files, and search the web - all from your terminal.

<div align="center"><img src="/assets/sofos_code.png" style="width: 800px;" alt="Sofos Code"></div>

## Table of Contents

- [Features](#features)
- [Installation](#installation)
  - [Requirements](#requirements)
  - [Optional (but strongly recommended)](#optional-but-strongly-recommended)
  - [Important: Gitignore Setup](#important-gitignore-setup)
- [Usage](#usage)
  - [Quick Start](#quick-start)
  - [Commands](#commands)
  - [Image Vision](#image-vision)
- [Cost Tracking](#cost-tracking)
  - [Options](#options)
- [Extended Thinking](#extended-thinking)
- [Custom Instructions](#custom-instructions)
- [Session History](#session-history)
- [Available Tools](#available-tools)
- [Security](#security)
  - [Bash Command Permissions (3-Tier System)](#bash-command-permissions-3-tier-system)
  - [Config File](#config-file)
- [Development](#development)
- [Troubleshooting](#troubleshooting)
- [Morph Integration](#morph-integration)
- [License](#license)
- [Acknowledgments](#acknowledgments)
- [Links & Resources](#links--resources)

## Features

- **Interactive REPL** - Multi-turn conversations with Claude or GPT
- **Image Vision** - Analyze local or web images by including paths/URLs in your message
- **Session History** - Automatic session saving and resume previous conversations
- **Custom Instructions** - Project and personal instruction files for context-aware assistance
- **File Operations** - Read, write, list, and create files/directories (sandboxed to current directory)
- **Ultra-Fast Editing** - Optional Morph Apply integration (10,500+ tokens/sec, 96-98% accuracy)
- **Code Search** - Fast regex-based code search using `ripgrep` (optional)
- **Web Search** - Real-time web information via Claude's and OpenAI's native search tools
- **Bash Execution** - Run tests and build commands safely (read-only, sandboxed)
- **Visual Diff Display** - See exactly what changed with colored diffs (red for deletions, blue for additions)
- **Iterative Tool Execution** - Sofos can use up to 200 tools per request for complex multi-file operations
- **Session Usage** – After exiting Sofos, a session usage is displayed, including the input and output tokens used and the estimated cost.
- **Secure** - All operations restricted to workspace, prevents directory traversal
- **Safe Mode** - Start or switch to a write-protected mode that limits tools to listing/reading files and web search; 
prompt changes from **`λ>`** to **`λ:`** as a visual cue

## Installation

### Requirements

- At least one LLM API key:
  - Anthropic API key ([get one](https://console.anthropic.com/)) for Claude models
  - OpenAI API key ([get one](https://platform.openai.com/)) for OpenAI models

### Optional (but strongly recommended)

- `ripgrep` for code search ([install guide](https://github.com/BurntSushi/ripgrep#installation))
- Morph API key for ultra-fast editing ([get one](https://morphllm.com/))

**Install with Homebrew**

```bash
brew tap alexylon/tap
brew install sofos
```

**Install from crates.io:**

*Requires Rust 1.70+ ([install guide](https://rust-lang.org/tools/install/))*

```bash
cargo install sofos
```

**Or build from source:**

```bash
git clone https://github.com/alexylon/sofos-code.git
cd sofos-code
cargo install --path .
```

### Important: Gitignore Setup

**Add `.sofos/` to your `.gitignore`** to avoid committing session history and personal settings:

```bash
# Add to your .gitignore
.sofos/
```

This directory contains sensitive data like conversation transcripts and personal instructions that shouldn't be shared.

**Note:** The `.sofosrc` file *should* be committed, as it contains team-wide project instructions.

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

- `/resume`   - Resume the previous session
- `/clear`    - Clear the conversation history
- `/think on` - Enable extended thinking
- `/think off`- Disable extended thinking
- `/think`    - Show the current thinking status
- `/s`        - Switch to safe mode (disables write/edit/delete tools). Prompt changes to **`λ:`**
- `/n`        - Switch back to normal mode (re-enables all tools). Prompt changes to **`λ>`**
- `/exit`, `/quit`, `/q`, or `Ctrl+D` - Exit and display a session cost summary
- `ESC` (while AI is responding) - Interrupt the response and provide additional guidance; the assistant will remember what was done before the interruption

**Command shortcuts:**

- Press **Tab** to show available commands (including partial matches on incomplete input) and navigate the completion menu; **Shift+Tab** moves to the previous item.
- Hit **Enter** to expand the currently highlighted completion.

### Image Vision

Include image paths or URLs directly in your message to have the AI analyze them:

```bash
# Local images (relative to workspace)
What's in this screenshot.png?
Describe ./images/diagram.jpg and explain the architecture

# Web images
Analyze this https://example.com/chart.png
What do you see in https://i.imgur.com/abc123.jpg?
```

**Supported formats:** JPEG, PNG, GIF, WebP (max 20MB for local files)

**Permission rules apply:** Local images outside workspace require explicit allow in config.

**Visual feedback:** When an image is detected, you'll see:
- `🔍 Detected 1 image reference(s)`
- `📷 Image loaded: local: path/to/image.png` (on success)
- `⚠️  Failed to load image: <error>` (on failure)

## Cost Tracking

Sofos automatically tracks token usage and calculates session costs. When you exit with `quit`, `exit`, or `Ctrl+D`, you'll see a summary:

```
──────────────────────────────────────────────────
Session Summary
──────────────────────────────────────────────────
Input tokens:      12,345
Output tokens:      5,678
Total tokens:      18,023

Estimated cost:     $0.1304
──────────────────────────────────────────────────
```

**Cost Calculation:**
- Costs are calculated based on official model pricing
- Claude models use official Anthropic pricing (e.g., Sonnet 4.5: $3/$15 per million input/output tokens)
- OpenAI models use official OpenAI pricing ($1.25/$10 per million tokens for gpt-5.1-codex and gpt-5.1-codex-max models)
- Accurate for standard API usage

### Options

```
-p, --prompt <TEXT>          One-shot mode
-s, --safe-mode              Start in read-only mode (only read/list/web-search tools; no writes or bash commands)
-r, --resume                 Resume a previous session
    --api-key <KEY>          Anthropic API key (overrides env var)
    --openai-api-key <KEY>   OpenAI API key (overrides env var)
    --morph-api-key <KEY>    Morph API key (overrides env var)
    --model <MODEL>          Model to use (default: claude-sonnet-4-5)
    --morph-model <MODEL>    Morph model (default: morph-v3-fast)
    --max-tokens <N>         Max response tokens (default: 8192)
-t, --enable-thinking        Enable extended thinking (default: false)
    --thinking-budget <N>    Token budget for thinking (default: 5120, must be < max-tokens)
-v, --verbose                Verbose logging
```

## Extended Thinking

Enable extended thinking for complex reasoning tasks that benefit from deeper analysis (disabled by default):

```bash
# Enable with default 5120 token budget
sofos --enable-thinking
# or use short flag
sofos -t

# Customize thinking budget (must be less than max-tokens)
sofos -t --thinking-budget 10000 --max-tokens 16000
```

**Note:** Thinking tokens count toward your API usage. Only enable for tasks that benefit from deeper reasoning.

## Custom Instructions

Provide project context to Sofos using instruction files:

**`.sofosrc`** (project root, version controlled)
- Shared with entire team
- Contains project conventions, architecture, coding standards
- See `.sofosrc` of this project's root for example

**`.sofos/instructions.md`** (gitignored, personal)
- Private to your workspace
- Your personal preferences and notes

Both files are loaded automatically at startup and appended to the system prompt.

## Session History

All conversations are automatically saved to `.sofos/sessions/` with both API messages (for continuing conversations) and display messages (for showing the original UI). 
Resume with `sofos --resume` or type `resume` in the REPL.

## Available Tools

Sofos can automatically use these tools:

**File Operations:**
- `read_file` - Read file contents
- `write_file` - Create or overwrite files
- `morph_edit_file` - Ultra-fast code editing (requires MORPH_API_KEY)
- `list_directory` - List directory contents
- `create_directory` - Create directories
- `delete_file` / `delete_directory` - Delete files/directories (with confirmation)
- `move_file` / `copy_file` - Move or copy files

**Code & Search:**
- `search_code` - Fast regex-based code search (requires `ripgrep`)
- `web_search` - Search the web for up-to-date information using Claude’s or OpenAI’s native web search
- `execute_bash` - Run tests and build commands (read-only, sandboxed)

When safe mode is enabled (via `--safe-mode` or `/s`), only `list_directory`, `read_file`, and `web_search` are available until you switch back with `/n`.

## Security

All file operations are sandboxed to your current working directory:

- ✅ Can access files in current directory and subdirectories
- ❌ Cannot access parent directories (`../`)
- ❌ Cannot access absolute paths (`/etc/passwd`)
- ❌ Cannot follow symlinks outside workspace

Bash execution is restricted to read-only operations:

- ✅ Can run tests and build commands (`cargo test`, `npm test`, etc.)
- ✅ Can read files and list directories (`cat`, `ls`, `grep`)
- ❌ Cannot use `sudo` or privilege escalation
- ❌ Cannot modify files (`rm`, `mv`, `cp`, `chmod`, `mkdir`, `touch`)
- ❌ Cannot change directories or use output redirection

### Bash Command Permissions (3-Tier System)

Sofos uses a 3-tier permission system for bash commands:

**Tier 1: Allowed (Predefined Safe Commands)**
These commands are automatically allowed without prompting:
- Build tools: `cargo`, `npm`, `go`, `make`, `python`, `pip`, etc.
- Read-only file operations: `ls`, `cat`, `grep`, `find`, `wc`, etc.
- System info: `pwd`, `whoami`, `date`, `env`, etc.
- Text processing: `sed`, `awk`, `sort`, `cut`, etc.
- Safe git commands (read-only)

**Tier 2: Forbidden (Predefined Dangerous Commands)**
These commands are always blocked:
- File deletion/modification: `rm`, `mv`, `cp`, `touch`, `mkdir`
- Permissions: `chmod`, `chown`, `chgrp`
- System control: `shutdown`, `reboot`, `systemctl`
- User management: `useradd`, `userdel`, `passwd`
- Process control: `kill`, `killall`
- Directory navigation: `cd`, `pushd`, `popd` (breaks sandbox)

**Tier 3: Unknown Commands (User Confirmation)**
Commands not in the predefined lists will prompt you for permission. You can:
- Allow once (temporary permission for this session)
- Remember decision (saved to `.sofos/config.local.toml` for future sessions)
- Deny once or permanently

### Config File

Your permission decisions are stored in configuration files:

**`~/.sofos/config.toml`** (global, optional)
- Applies to all Sofos workspaces on your machine
- Useful for personal preferences (e.g., always allow reading `~/.zshrc`)
- Gitignored by default (in your home directory)

**`.sofos/config.local.toml`** (workspace-specific, gitignored)
- Applies only to the current workspace
- Local settings override global settings when they conflict
- Same rule → local takes precedence
- Different rules → both apply

Example global config (`~/.sofos/config.toml`):
```toml
[permissions]
allow = [
  "Bash(custom_tool)",
  "Read(~/.zshrc)",
  "Read(~/.config/**)",
]
deny = []
ask = []
```

Example local config (`.sofos/config.local.toml`):
```toml
[permissions]
allow = [
  "Bash(custom_command_1)", 
  "Bash(custom_command_2:*)",
  "Read(/etc/hosts)",
]
deny = [
  "Bash(dangerous_command)",
  "Read(./.env)",
  "Read(./.env.*)",
  "Read(./secrets/**)",
]
ask = ["Bash(unknown_tool)"]
```

**Read Permission Rules:**
- Files inside workspace: allowed by default, denied if matched by `deny` rule
- Files outside workspace: denied by default, allowed only if matched by `allow` rule
- Supports glob patterns (`*` for single level, `**` for recursive)
- Supports tilde expansion (`~` expands to home directory)
- Paths are canonicalized before checking (symlinks resolved, `..` normalized)
- For outside workspace files, use absolute paths or tilde paths in config
- `ask` list only applies to Bash commands, not Read operations

**Bash Command Rules:**
- Always sandboxed to workspace (cannot access outside files even if Read allow rule exists)
- Commands in `allow` execute without prompts
- Commands in `deny` are always blocked
- Commands in `ask` prompt for permission each time
- Path arguments in commands are checked against Read deny rules

**Interactive Decisions:**
- When you approve/deny an unknown bash command with "remember", it's saved to `.sofos/config.local.toml`
- Global config is never modified automatically - edit it manually as needed

Both files are gitignored and local to your system.

Git commands are restricted to read-only operations:

- ✅ Can view history and status (`git status`, `git log`, `git diff`, `git show`)
- ✅ Can list branches and remotes (`git branch`, `git remote -v`)
- ✅ Can search and blame (`git grep`, `git blame`)
- ❌ Cannot push, pull, fetch, or clone (network operations)
- ❌ Cannot commit, add, or modify files (`git commit`, `git add`, `git reset --hard`)
- ❌ Cannot change branches or stash (`git checkout -b`, `git stash`, `git switch`)
- ❌ Cannot configure remotes (`git remote add`, `git remote set-url`)

**Best Practice:** Run `sofos` from your project directory and use git to track changes.

## Development

```bash
# Run tests
cargo test

# Build release
cargo build --release

# Debug logging
RUST_LOG=debug sofos
```

**Project Structure:**
- `src/` - Core Rust code
  - `api/` - Anthropic/OpenAI/Morph clients, shared API types/utilities
  - `tools/` - Sandboxed tools (filesystem, bash exec, web/code search, permissions, utils, tests)
  - `commands/` - Built-in REPL commands (e.g. `/resume`, `/clear`, safe mode)
  - `repl.rs` / `ui.rs` - Interactive REPL + terminal UI
  - `request_builder.rs` / `response_handler.rs` - LLM request/response plumbing + tool loop
  - `conversation.rs` / `history.rs` / `session_state.rs` / `session_selector.rs` - Conversation state, persistence, resume UI
  - `prompt.rs` / `model_config.rs` / `config.rs` / `cli.rs` - Prompt building, model selection, config + CLI flags
  - `diff.rs` / `syntax.rs` / `error.rs` / `error_ext.rs` - Diff rendering, syntax highlighting, error types/helpers
  - `main.rs` - Binary entry point
- `tests/` - Integration tests
- `assets/` - README images

See `.sofosrc` for detailed project conventions.

## Troubleshooting

- **API errors:** Check internet connection and API key
- **Path errors:** Use relative paths only, no `..` or absolute paths
- **Build errors:** Run `rustup update && cargo clean && cargo build`

## Morph Integration

Optional integration with Morph's Apply API for ultra-fast code editing:

- **10,500+ tokens/sec** - Lightning-fast edits
- **96-98% accuracy** - Reliable code modifications
- **Direct REST API** - No additional dependencies
- **Optional** - Enable with `MORPH_API_KEY`

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
