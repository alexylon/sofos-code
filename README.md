# Sofos Code

![CI](https://github.com/alexylon/sofos-code/actions/workflows/rust.yml/badge.svg) &nbsp; [![Crates.io](https://img.shields.io/crates/v/sofos.svg?color=blue)](https://crates.io/crates/sofos)

A blazingly fast, interactive AI coding assistant powered by Claude or GPT, implemented in pure Rust, that can generate code, edit files, and search the web - all from your terminal.

Tested on macOS; supported on Linux and Windows.

<div align="center"><img src="/assets/screenshot.png" style="width: 800px;" alt="Sofos Code"></div>

## Table of Contents

- [Features](#features)
- [Install](#install)
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
- [MCP Servers](#mcp-servers)
- [Security](#security)
- [Configuration](#configuration)
- [Development](#development)
- [Release](#release)
- [Troubleshooting](#troubleshooting)
- [License](#license)
- [Acknowledgments](#acknowledgments)
- [Links & Resources](#links--resources)

## Features

- **Interactive TUI** - Multi-turn conversations with Claude or GPT in an inline viewport at the bottom of your terminal; your emulator owns the scrollback, scrollbar, mouse wheel, and copy-paste
- **Keep Typing During AI Turns** - Messages queue FIFO while the model works; mid tool-loop messages steer the current turn without interrupting
- **Live Status Line** - Model, mode, reasoning config, and running token totals shown under the input
- **Markdown Formatting** - AI responses with syntax highlighting for code blocks
- **Image Vision** - Analyze local or web images, paste from clipboard with Ctrl+V
- **Session History** - Auto-save with an in-TUI resume picker (`/resume` or `sofos -r`)
- **Custom Instructions** - Project and personal context files
- **File Operations** - Read, write, edit, list, glob, create, move, copy, delete (sandboxed; external paths via permission grants)
- **Targeted Edits** - Diff-based `edit_file` for precise string replacements
- **Ultra-Fast Editing** - Optional Morph Apply integration (10,500+ tokens/sec)
- **File Search** - Find files by glob pattern (`**/*.rs`)
- **Code Search** - Fast regex search with ripgrep
- **Web Search** - Real-time info via Claude's/OpenAI's native search
- **Web Fetch** - Read documentation and web pages
- **Bash Execution** - Run tests and builds, sandboxed behind a 3-tier permission system
- **MCP Integration** - Connect to external tools via Model Context Protocol
- **Visual Diffs** - Syntax-highlighted diffs with line numbers
- **Iterative Tools** - Up to 200 tool calls per request
- **Context Compaction** - Summarizes older messages instead of dropping them
- **Cost Tracking** - Session token usage and cost estimates
- **Safe Mode** - Read-only operation mode

## Install

**Requirements:** Anthropic API key ([get one](https://console.anthropic.com/)) or OpenAI API key ([get one](https://platform.openai.com/))

**Optional** (but highly recommended): `ripgrep` for code search ([install](https://github.com/BurntSushi/ripgrep#installation)), Morph API key for ultra-fast editing ([get one](https://morphllm.com/))

### Prebuilt binary

Download from [GitHub Releases](https://github.com/alexylon/sofos-code/releases/latest) (macOS, Linux, Windows):

```bash
# macOS / Linux
tar xzf sofos-*.tar.gz
sudo mv sofos /usr/local/bin/

# Windows — extract the .zip, then add the folder to your PATH
```

> **macOS:** On first run, macOS may block the binary. Go to System Settings → Privacy & Security and click *Allow Anyway*.

### With Rust

```bash
cargo install sofos
```

### From source

```bash
git clone https://github.com/alexylon/sofos-code.git
cd sofos-code && cargo install --path .
```

**Important:** Add `.sofos/` to `.gitignore` (contains session history and personal settings). Keep `AGENTS.md` (team-wide instructions).

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
- `/think [off|low|medium|high|xhigh|max]` - Set reasoning effort (shows status if no arg). `xhigh` is only accepted on Opus 4.7 / gpt-5.x; `max` is only accepted on Opus 4.7, Opus 4.6, and Sonnet 4.6.
- `/compact` - Summarize older messages via the LLM to reclaim context tokens. Triggers automatically at the per-model auto-compact threshold (~250K tokens on 1M-window models, ~170K on Haiku, ~250K on Codex). On Claude Opus 4.7 / 4.6 / Sonnet 4.6 the API itself runs the summarization server-side via the `compact-2026-01-12` beta — no extra round-trip.
- `/s` - Safe mode (read-only, prompt: **`:`**)
- `/n` - Normal mode (all tools, prompt: **`>`**)
- `/exit`, `/quit`, `/q`, `Ctrl+D` - Exit with cost summary
- `ESC` or `Ctrl+C` (while busy) - Interrupt AI response

**Message queueing:** Keep typing while the AI is working. Pressing Enter queues the message; queued messages are sent automatically once the current turn finishes. The hint line shows the queue count.

**Multi-line input:** `Shift+Enter` inserts a newline; `Enter` alone submits. On terminals that do not deliver `Shift+Enter` distinctly (Apple Terminal.app and many defaults), use `Alt+Enter` or `Ctrl+Enter` as a fallback newline binding.

**Scrollback:** Sofos runs as an inline viewport at the bottom of your terminal — the rest of the terminal is normal scrollback, so use your terminal emulator's own scrollbar, mouse wheel, and text selection / copy-paste.

**Status line:** Shown below the input box. Updates live as you change state (`/s`, `/n`, `/think`) — model, mode (`normal`/`safe`), reasoning config (`effort: off|low|medium|high|xhigh|max` for OpenAI and the Anthropic adaptive models; `thinking: <N> tok` for older Claude models with manual budgets), and running token totals.

### Image Vision

Include image paths or URLs directly in your message, or paste images from clipboard:

```bash
# Paste from clipboard
Ctrl+V                        # Shows ① marker, paste multiple for ①②③
                               # Delete a marker to remove that image

# Local images
What's in this screenshot.png?
Describe ./images/diagram.jpg

# Paths with spaces - use quotes
What do you see in "/Users/alex/Documents/my image.png"?

# Web images
Analyze https://example.com/chart.png
```

**Formats:** JPEG, PNG, GIF, WebP (max 20MB local) | **Clipboard:** Ctrl+V pastes images on macOS, Linux, and Windows | **Spaces:** Wrap in quotes `"path/with space.png"` | **Permissions:** Outside workspace requires config

### Cost Tracking

Exit summary shows token usage and estimated cost based on official API pricing. When the provider prompt cache served any tokens during the session, a `cache read: N (M% hit)` row appears under the input total, and the estimated cost reflects the cache discount (10% of base input on both providers, plus 125% for Anthropic 5-min writes and 200% for 1-hour writes).

**Tiered pricing detection.** GPT-5.4 and GPT-5.5 charge a session-wide premium (2× input, 1.5× output) once any single prompt crosses 272K input tokens. Sofos tracks the largest single-turn input observed and switches the cost calculator to premium rates if the cliff is ever crossed, so the displayed cost reflects what OpenAI actually bills.

### CLI Options

```
-p, --prompt <TEXT>          One-shot mode
-s, --safe-mode              Start in read-only mode (native writes and bash disabled)
-r, --resume                 Resume a previous session
    --check-connection       Check API connectivity and exit
    --api-key <KEY>          Anthropic API key (overrides env var)
    --openai-api-key <KEY>   OpenAI API key (overrides env var)
    --morph-api-key <KEY>    Morph API key (overrides env var)
    --model <MODEL>          Model to use (default: claude-sonnet-4-6)
    --morph-model <MODEL>    Morph model (default: morph-v3-fast)
    --max-tokens <N>         Max response tokens (default: 32768; must be > 16384 when reasoning effort is enabled)
-e, --reasoning-effort <LV>  Reasoning effort: off, low, medium, high, xhigh, max (default: medium). Per-model support varies — see the Reasoning Effort section.
    --thinking-budget <N>    Vestigial. Currently inert on every path: legacy Claude uses a fixed per-tier budget (Low=1024, Medium=5120, High=16384), Claude Opus 4.7+ uses adaptive thinking, OpenAI uses `reasoning.effort`. Kept for backwards-compatibility; will be removed.
-v, --verbose                Verbose logging
```

### Reasoning Effort

Sofos exposes six levels — `off`, `low`, `medium`, `high`, `xhigh`, `max`. The first four are universal; `xhigh` and `max` have model-specific support and sofos rejects mismatched pairs at startup or at the `/think` command.

```bash
sofos -e medium                             # Default — sensible cost/quality balance
sofos -e high                               # Hard tasks, willing to pay more
sofos -e off                                # Cheapest path; no reasoning summary
sofos -e max --model claude-opus-4-7        # Absolute-capability rung on Anthropic adaptive
sofos -e xhigh --model gpt-5.5              # Top rung on OpenAI gpt-5.x

# Mid-session
/think high                                 # Bump up
/think off                                  # Drop to minimal
/think                                      # Show current
```

**Per-model support matrix:**

| Effort   | Opus 4.7 | Opus 4.6 | Sonnet 4.6 | Haiku 4.5 / older | gpt-5.4 / 5.5 / codex |
| -------- | :------: | :------: | :--------: | :---------------: | :-------------------: |
| `off`    | ✓        | ✓        | ✓          | ✓                 | ✓                     |
| `low`    | ✓        | ✓        | ✓          | ✓                 | ✓                     |
| `medium` | ✓ (def)  | ✓ (def)  | ✓ (def)    | ✓ (def)           | ✓ (def)               |
| `high`   | ✓        | ✓        | ✓          | ✓                 | ✓                     |
| `xhigh`  | ✓        | ✗        | ✗          | ✗                 | ✓                     |
| `max`    | ✓        | ✓        | ✓          | ✗                 | ✗                     |

**Per-provider wire mapping:**

- **OpenAI (gpt-5 family, incl. codex)** — sends `reasoning.effort` matching the level (`minimal` for `off`, `low` / `medium` / `high` / `xhigh` otherwise) and `summary: "auto"` when on, omitted when off. `max` isn't accepted by OpenAI.
- **Claude Opus 4.7, Opus 4.6, Sonnet 4.6** — adaptive thinking; the server picks the budget based on the prompt, and sofos sends `output_config.effort` matching the level (`off` collapses to `low`, the lowest the API accepts). `xhigh` is Opus 4.7 only on Anthropic. `--thinking-budget` is ignored.
- **Older Claude (Haiku 4.5, Sonnet 4.5, Opus 4.5)** — `off` disables extended thinking; `low`, `medium`, and `high` each map to a distinct legacy `budget_tokens` value (`1024 / 5120 / 16384`) so the slider has a visible effect. `xhigh` / `max` are rejected upstream because legacy models don't expose them. `--thinking-budget` is ignored — the per-tier values are the source of truth.

## Custom Instructions

Two files are loaded at startup and appended to the system prompt:

- **[`AGENTS.md`](https://agents.md)** (project root, version controlled) — project context for AI agents: team-wide conventions, architecture, domain vocabulary.
- **`.sofos/instructions.md`** (gitignored) — personal preferences that shouldn't be shared with the team.

## Session History

Conversations auto-saved to `.sofos/sessions/`. Resume with `sofos -r` or `/resume`.

## Available Tools

**File Operations** (accept absolute and `~/` paths with a `Read` or `Write` grant as appropriate — see Security and Configuration):
- `read_file` - Read file contents
- `list_directory` - List a single directory's contents
- `glob_files` - Find files recursively by glob pattern (`**/*.rs`, `src/**/test_*.py`)
- `write_file` - Create or overwrite files (append mode for chunked writes)
- `edit_file` - Targeted string replacement edits (no API key needed)
- `morph_edit_file` - Ultra-fast code editing (requires MORPH_API_KEY)
- `create_directory` - Create a directory (and missing parents)
- `move_file`, `copy_file` - Move or copy files

**Workspace-only file ops** (absolute / `~/` paths are rejected, even with grants — destructive ops are deliberately scoped to the workspace):
- `delete_file`, `delete_directory` - Delete files or directories (prompt for confirmation)

**Code & Search:**
- `search_code` - Fast regex-based code search (requires `ripgrep`)
- `web_search` - Real-time web information via Claude's/OpenAI's native search
- `web_fetch` - Fetch URL content as readable text (documentation, APIs)
- `execute_bash` - Run bash commands, sandboxed through the 3-tier permission system (safe commands auto-run, destructive ones blocked, unknown ones prompt)

**MCP Tools:**
- Tools from configured MCP servers (prefixed with server name, e.g., `filesystem_read_file`)

**Image Vision:** not a tool — sofos detects image paths (JPEG, PNG, GIF, WebP, up to 20 MB local) in your user messages and loads them automatically as image content blocks. Clipboard paste (Ctrl+V) works the same way. See [Image Vision](#image-vision) under Usage.

**Note:** Tools can access paths outside the workspace when allowed via interactive prompt or config. Three independent scopes (`Read` / `Write` / `Bash`) gate this access — see [Security](#security) for the full model.

Safe mode (`--safe-mode` or `/s`) restricts the native tool set to read-only operations: `list_directory`, `read_file`, `glob_files`, `web_fetch`, `web_search` (Anthropic + OpenAI provider-native variants), and `search_code` when `ripgrep` is available. MCP tools are **not** filtered by safe mode — if you've configured MCP servers with mutating tools, those remain available.

## MCP Servers

Connect to external tools via MCP (Model Context Protocol). Configure in `~/.sofos/config.toml` or `.sofos/config.local.toml` (see the example in the "Configuration" section).

Tools auto-discovered, prefixed with server name (e.g., `filesystem_read_file`). See `examples/mcp_quickstart.md`.

**Popular servers:** https://github.com/modelcontextprotocol/servers

## Security

**Sandboxing (by default):**
- ✅ Full access to workspace files/directories
- ✅ External access via interactive prompts — user is asked to allow/deny, with option to remember in config
- Three separate scopes: `Read` (read/list), `Write` (write/create/move/delete), `Bash` (commands with external paths)
- Each scope is independently granted — Read access does not imply Write or Bash access, and vice versa
- Tools that both read and write a file on external paths (`edit_file`, `morph_edit_file`) require **both** `Read` and `Write` grants on the path

**Bash Permissions (3-Tier System):**

1. **Allowed (auto-execute):** Build tools (cargo, npm, go), read-only commands (ls, cat, grep), system info (pwd, date), git read-only commands (`status`, `log`, `diff`, `show`, `branch`, …).
2. **Forbidden (always blocked):** file destruction (`rm`, `rmdir`, `touch`, `ln`); permissions (`chmod`, `chown`, `chgrp`); disk / partition (`dd`, `mkfs`, `fdisk`, `parted`, `mkswap`, `mount`, `umount`); system control (`shutdown`, `reboot`, `halt`, `systemctl`, `service`); user management (`useradd`, `usermod`, `passwd`, …); process signals (`kill`, `killall`, `pkill`); privilege escalation (`sudo`, `su`); directory navigation (`cd`, `pushd`, `popd`); destructive git operations (`git push`, `git reset --hard`, `git clean`, `git checkout -f`, `git checkout -b`, `git switch`, `git rebase`, `git commit`, …).
3. **Ask (prompt user):** `cp`, `mv`, `mkdir`, `git checkout <branch>` / `git checkout HEAD~N` / `git checkout -- <path>`, commands referencing paths outside the workspace, and any unknown command. Approvals can be session-scoped or remembered in config. Commands whose args wouldn't repeat — `sed -n 'N,Mp'`, `head -n N`, `tail -n N`, `grep -A/-B/-C N`, `awk 'NR==N'` — drop the "and remember" options and show a plain Yes / No, since the exact command string won't match the next call.

## Configuration

Permissions are stored in `.sofos/config.local.toml` (workspace-specific, gitignored) or `~/.sofos/config.toml` (global, optional). Local config overrides global.

**Example:**

```toml
[permissions]
allow = [
  # Read permissions - for reading/listing files outside workspace
  "Read(~/.zshrc)",           # Specific file
  "Read(~/.config/**)",       # Recursive
  "Read(/etc/hosts)",         # Absolute path
  
  # Write permissions - for writing/editing files outside workspace
  "Write(/tmp/output/**)",    # Allow writes to specific external dir
  
  # Bash path permissions - for commands referencing external paths
  "Bash(/var/log/**)",        # Allow bash commands to access this dir
  
  # Bash command permissions - for command execution
  "Bash(custom_command)",     # Specific command
  "Bash(pattern:*)",          # Wildcard pattern
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

[mcp-servers.company-internal]
command = "/usr/local/bin/company-mcp-server"
args = ["--config", "/etc/company/mcp-config.json"]
env = { "COMPANY_API_URL" = "https://internal.company.com" }

[mcp-servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { "GITHUB_TOKEN" = "ghp_YOUR_TOKEN" }

[mcp-servers.api]
url = "https://api.example.com/mcp"
headers = { "Authorization" = "Bearer token123" }
```

**Rules\*:**
- Workspace files: allowed by default unless in `deny` list
- Outside workspace: prompts interactively on first access, or pre-configure in `allow` list
- Three scopes: `Read(path)` for reading, `Write(path)` for writing, `Bash(path)` for bash access — each independent
- `Bash(path)` entries with globs (e.g. `Bash(/tmp/**)`) grant path access; plain entries (e.g. `Bash(npm test)`) grant command access
- Bare `"Bash"` in `allow` auto-allows every command except those in the built-in forbidden set (`rm`, `chmod`, `sudo`, …); bare `"Bash"` in `deny` auto-rejects every bash command. Deny wins when both lists carry the blanket entry. Structural safety (`>` redirection, `<<`, `git push`, parent traversal, external-path prompts) still applies under blanket-allow.
- Glob patterns supported: `*` (single level), `**` (recursive)
- Tilde expansion: `~` → `$HOME` on Unix, `%USERPROFILE%` on Windows
- `ask` only works for Bash commands

\* These rules do not restrict MCP server command paths

## Development

```bash
cargo test                    # Run tests
cargo build --release         # Build release
RUST_LOG=debug sofos          # Debug logging
```

**Structure:**

```
src/
├── main.rs              # Entry point
├── cli.rs               # CLI argument parsing
├── clipboard.rs         # Clipboard image paste (Ctrl+V)
├── error.rs             # Error types
├── config.rs            # Configuration
│
├── api/                 # API clients
│   ├── anthropic/       # Claude API client
│   │   ├── client.rs        # HTTP entry points
│   │   ├── wire.rs          # Request and response shapes
│   │   └── stream.rs        # SSE parser for streaming
│   ├── openai/          # OpenAI API client
│   │   ├── client.rs        # HTTP entry points
│   │   ├── wire.rs          # Request and response shapes
│   │   └── stream.rs        # SSE parser for streaming
│   ├── morph.rs         # Morph Apply API client
│   ├── model_info.rs    # Per-model capabilities and pricing
│   ├── truncate.rs      # Tool-result trimming for context budgets
│   ├── types.rs         # Message types and serialization
│   └── utils.rs         # Retries, error handling
│
├── mcp/                 # MCP (Model Context Protocol)
│   ├── config.rs        # Server configuration loading
│   ├── protocol.rs      # Protocol types (JSON-RPC)
│   ├── client.rs        # Client implementations (stdio, HTTP)
│   ├── manager.rs       # Server connection management
│   └── transport/       # Wire transports
│       ├── stdio.rs         # Child-process stdio transport
│       └── http.rs          # Streamable HTTP transport
│
├── repl/                # REPL components
│   ├── mod.rs           # Repl struct, config, status, reasoning and safe-mode handlers
│   ├── turn.rs          # process_message: per-turn driver and image-retry path
│   ├── compaction.rs    # Conversation compaction (truncate plus summarise)
│   ├── sessions.rs      # Save, load and resume saved sessions
│   ├── conversation/    # Message history
│   │   ├── messages.rs      # Add, restore and clear messages
│   │   ├── compaction.rs    # Truncate tool results and replace prefix with summary
│   │   ├── lifecycle.rs     # System prompt and feature wiring
│   │   └── tokens.rs        # Token-budget tracking
│   ├── request_builder.rs   # API request construction
│   ├── response_handler.rs  # Response and tool iteration
│   └── tui/             # Ratatui front end
│       ├── mod.rs             # Entry point and wiring
│       ├── app.rs             # UI state (log, input, queue, picker)
│       ├── ui.rs              # Rendering
│       ├── event.rs           # Job / UiEvent channel payloads
│       ├── event_loop.rs      # Main event pump
│       ├── input.rs           # Input box state and editing
│       ├── keymap.rs          # Key bindings
│       ├── worker.rs          # Background thread that owns the Repl
│       ├── output.rs          # Stdout/stderr capture via dup2
│       ├── inline_terminal.rs # Custom ratatui Terminal (resize-safe)
│       ├── inline_tui.rs      # Frame driver and history log
│       ├── scrollback.rs      # DECSTBM-based insert-above-viewport
│       └── sgr.rs             # SGR escape helpers
│
├── session/             # Session management
│   ├── history/         # On-disk session storage
│   │   ├── manager.rs       # Save, load and list orchestration
│   │   ├── model.rs         # Serialised session shape
│   │   ├── index.rs         # Sessions index file
│   │   ├── preview.rs       # First-line previews for the picker
│   │   └── instructions.rs  # Custom instructions loader
│   ├── state.rs         # Runtime session state
│   └── selector.rs      # Session selection TUI
│
├── tools/               # Tool implementations
│   ├── executor.rs      # Tool dispatch
│   ├── resolve.rs       # Path resolution and workspace gating
│   ├── filesystem.rs    # File operations (read, write, edit, chunked append)
│   ├── bash/            # Bash execution
│   │   ├── executor.rs      # Spawn and capture
│   │   ├── validate.rs      # Forbidden-command and structural checks
│   │   └── output.rs        # Output formatting and truncation
│   ├── codesearch.rs    # Code search (ripgrep)
│   ├── image.rs         # Image detection and loading for message content
│   ├── permissions/     # 3-tier permission system
│   │   ├── manager.rs       # PermissionManager core
│   │   ├── settings.rs      # Config loading
│   │   ├── pattern.rs       # Rule-string parsing
│   │   ├── scope.rs         # Read / Write / Bash-path matching
│   │   └── command_parse.rs # Shell tokenisation and compound splitting
│   ├── morph_validate.rs # Pre-flight checks for morph_edit_file
│   ├── tool_name.rs     # Type-safe tool name enum
│   ├── types.rs         # Tool definitions for the API
│   ├── utils.rs         # Confirmations, truncation, HTML-to-text
│   ├── test_support.rs  # Shared test helpers
│   └── tests.rs         # Tool integration tests
│
├── ui/                  # UI components
│   ├── mod.rs           # UI utilities, prompts and banners
│   ├── markdown.rs      # Markdown renderer (block and streaming)
│   ├── syntax.rs        # Syntax highlighting
│   ├── cost.rs          # Cost calculation and session summary
│   ├── session_display.rs # Replay saved sessions in the TUI
│   └── diff.rs          # Syntax-highlighted diffs with line numbers
│
└── commands/            # Built-in commands (/clear, /resume, /compact, /think, /s, /n)
    └── builtin.rs       # Command implementations
```

See `AGENTS.md` for detailed conventions.

## Release

This project uses **cargo-release** for automated versioning and publishing.

**Quick commands:**

```bash
# Preview the release
cargo release patch

# Execute the release
cargo release patch --execute

# Release specific version
cargo release [patch|minor|major] --execute
```

The release workflow automatically:
1. Bumps version in `Cargo.toml`
2. Runs tests and formatting checks
3. Updates `CHANGELOG.md`
4. Publishes to crates.io
5. Creates release commit and Git tag
6. Pushes to remote repository

**For detailed instructions**, see [RELEASE.md](RELEASE.md).

## Troubleshooting

- **API errors:** Check connection and API key
- **Path errors:** Use relative paths for workspace; external paths prompt interactively or can be pre-allowed with `Read`/`Write`/`Bash` entries in config
- **Build errors:** `rustup update && cargo clean && cargo build`
- **Images with spaces:** Wrap path in quotes

## License

MIT License

## Acknowledgments

Built with Rust and powered by Anthropic's Claude or OpenAI's GPT. Morph Apply integration for fast edits.

## Links & Resources

- [GitHub](https://github.com/alexylon/sofos-code)
- [Crates.io](https://crates.io/crates/sofos)

---

**Disclaimer:** Sofos Code may make mistakes. Always review generated code before use.

[![forthebadge](https://forthebadge.com/images/badges/made-with-rust.svg)](https://forthebadge.com)
