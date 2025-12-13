# Sofos Code

![](https://github.com/alexylon/sofos-code/actions/workflows/rust.yml/badge.svg)

A blazingly fast, interactive AI coding assistant powered by Claude or GPT, implemented in pure Rust, that can generate code, edit files, and search the web - all from your terminal.

<div align="center"><img src="/assets/sofos_code.png" style="width: 800px;" alt="Sofos Code"></div>

## Features

- **Interactive REPL** - Multi-turn conversations with Claude or GPT
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

- Rust 1.70+ ([install guide](https://rust-lang.org/tools/install/))
- At least one LLM API key:
  - Anthropic API key ([get one](https://console.anthropic.com/)) for Claude models
  - OpenAI API key ([get one](https://platform.openai.com/)) for OpenAI models

### Optional (but strongly recommended)

- `ripgrep` for code search ([install guide](https://github.com/BurntSushi/ripgrep#installation))
- Morph API key for ultra-fast editing ([get one](https://morphllm.com/))

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
- `src/api/` - Anthropic API client and types
- `src/tools/` - Tool implementations (filesystem, bash, code search)
- `src/conversation.rs` - Conversation history management
- `src/history.rs` - Session persistence and custom instructions
- `src/repl.rs` - Main REPL loop and display logic
- `src/syntax.rs` - Syntax highlighting

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

---

**Disclaimer:** Sofos Code may make mistakes. Always review generated code before use.

[![forthebadge](https://forthebadge.com/images/badges/made-with-rust.svg)](https://forthebadge.com)
