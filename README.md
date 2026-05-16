# Sofos Code

![CI](https://github.com/alexylon/sofos-code/actions/workflows/rust.yml/badge.svg) &nbsp; [![Crates.io](https://img.shields.io/crates/v/sofos.svg?color=blue)](https://crates.io/crates/sofos)

Sofos Code is a terminal-based AI coding assistant for software projects. It connects Claude or OpenAI models to a secure local toolset for reading code, editing files, running approved commands, searching the web, and working with external tools through Model Context Protocol (MCP).

Sofos is written in Rust, runs in your terminal, and is designed around explicit permissions: workspace access is available by default, while external paths and higher-risk actions require user approval or configuration.

Tested on macOS. Supported on Linux. Windows support is experimental: the bash executor and the terminal UI assume Unix-style facilities, so some features may not work or may need adjustments.

<div align="center"><img src="/assets/screenshot.png" style="width: 800px;" alt="Sofos Code terminal screenshot"></div>

---

## Table of contents

- [What Sofos does](#what-sofos-does)
- [Key features](#key-features)
- [Installation](#installation)
  - [Requirements](#requirements)
  - [Prebuilt binary](#prebuilt-binary)
  - [Install with Cargo](#install-with-cargo)
  - [Install from source](#install-from-source)
- [Quick start](#quick-start)
- [Usage](#usage)
  - [Interactive commands](#interactive-commands)
  - [Input behaviour](#input-behaviour)
  - [One-shot prompts](#one-shot-prompts)
  - [Image vision](#image-vision)
- [CLI reference](#cli-reference)
- [Models and reasoning effort](#models-and-reasoning-effort)
- [Tools](#tools)
  - [Native tools](#native-tools)
  - [Safe mode tools](#safe-mode-tools)
  - [MCP tools](#mcp-tools)
- [Security model](#security-model)
  - [Workspace and external paths](#workspace-and-external-paths)
  - [Bash command permissions](#bash-command-permissions)
  - [Destructive operations](#destructive-operations)
- [Configuration](#configuration)
  - [Custom instructions](#custom-instructions)
  - [Permissions](#permissions)
  - [MCP servers](#mcp-servers)
- [Sessions and cost tracking](#sessions-and-cost-tracking)
- [Development](#development)
  - [Project structure](#project-structure)
  - [Release process](#release-process)
- [Troubleshooting](#troubleshooting)
- [License](#license)
- [Acknowledgments](#acknowledgments)
- [Links](#links)

---

## What Sofos does

Sofos provides an AI assistant inside your terminal with controlled access to your project. It can:

- inspect files and directories;
- search code with ripgrep;
- edit files through exact replacements or Morph Apply;
- create, move, copy, and delete files with permission checks;
- run safe build and test commands;
- fetch documentation and use provider-native web search;
- review local, clipboard, or web images;
- update a visible task plan during multi-step work;
- save and resume conversations;
- connect to external tools through Model Context Protocol servers.

The assistant can act through tools, but it does not do so silently: tool calls are shown to the user, dangerous commands are blocked, and operations outside the workspace are gated by independent permission scopes.

---

## Key features

- **Terminal UI** — inline viewport at the bottom of your terminal; normal terminal scrollback remains available.
- **Claude and OpenAI support** — one provider abstraction with provider-specific streaming, reasoning, web search, and cache handling.
- **Live streaming Markdown** — assistant responses render as they arrive, including code blocks, headings, lists, blockquotes, and links.
- **Tool loop execution** — the model can use tools iteratively, with a hard maximum to prevent infinite loops.
- **Safe file editing** — targeted `edit_file`, chunked `write_file`, visual diffs, atomic writes, and optional Morph Apply.
- **Strong permission model** — independent Read, Write, and Bash grants for paths outside the workspace.
- **Bash safety** — allowed, denied, and ask tiers, plus structural checks for parent traversal, redirection, and dangerous git operations.
- **Safe mode** — read-only native tools for review-only sessions.
- **Image vision** — local images, web images, and clipboard paste.
- **MCP integration** — connect additional tool servers through stdio or streamable HTTP.
- **Session persistence** — saved conversations, resume picker, restored safe mode, restored model where compatible, and persisted cost counters.
- **Cost visibility** — token totals, cache hit reporting, and provider-specific price estimates.
- **Context compaction** — local and provider-supported compaction to keep long sessions usable.

---

## Installation

### Requirements

You need at least one provider API key:

- `ANTHROPIC_API_KEY` for Claude models; or
- `OPENAI_API_KEY` for OpenAI models.

Optional but recommended:

- `ripgrep` for fast code search through the `search_code` tool;
- `MORPH_API_KEY` for the optional `morph_edit_file` fast edit tool.

### Prebuilt binary

Download the latest binary from GitHub Releases.

```bash
# macOS / Linux
tar xzf sofos-*.tar.gz
sudo mv sofos /usr/local/bin/

# Windows
# Extract the .zip archive and add the extracted folder to PATH.
```

On macOS, the first run may be blocked by Gatekeeper. Open System Settings → Privacy & Security and choose **Allow Anyway** for the Sofos binary.

### Install with Cargo

```bash
cargo install sofos
```

### Install from source

```bash
git clone https://github.com/alexylon/sofos-code.git
cd sofos-code
cargo install --path .
```

Keep `.sofos/` out of version control. It stores sessions, local permissions, and personal settings. `AGENTS.md` is project-level context and is intended to be version controlled.

---

## Quick start

```bash
# Choose one provider.
export ANTHROPIC_API_KEY='your-anthropic-key'
# or
export OPENAI_API_KEY='your-openai-key'

# Optional: enable Morph Apply edits.
export MORPH_API_KEY='your-morph-key'

# Start the interactive assistant.
sofos
```

Use a different model:

```bash
sofos --model gpt-5.5
sofos --model claude-opus-4-7 -e high
```

Run a single prompt and exit:

```bash
sofos -p "Review the error handling in src/error.rs"
```

Start in read-only native-tool mode:

```bash
sofos --safe-mode
```

Resume a saved session:

```bash
sofos --resume
```

---

## Usage

### Interactive commands

| Command | Description |
|---|---|
| `/resume` | Open the session picker and resume a saved conversation. |
| `/clear` | Clear the current conversation history and start a fresh session id. |
| `/compact` | Compact older context to reduce token usage. |
| `/think` | Show the current reasoning-effort setting. |
| `/think off\|low\|medium\|high\|xhigh\|max` | Change reasoning effort when the active model supports the selected level. |
| `/s` | Enable safe mode: read-only native tools. Prompt changes to `:`. |
| `/n` | Return to normal mode. Prompt changes to `>`. |
| `/exit`, `/quit`, `/q`, `Ctrl+D` | Save the session and exit with a cost summary. |
| `ESC` or `Ctrl+C` while busy | Interrupt the current AI turn. |

### Input behaviour

- **Enter** submits the current message.
- **Shift+Enter** inserts a newline when the terminal supports it.
- **Alt+Enter** or **Ctrl+Enter** can be used as newline fallbacks.
- You can keep typing while the model is working. New messages are queued and processed in order.
- If the model is inside a tool loop, a queued message is delivered at the next tool-result boundary so it can steer the current turn without interrupting it.
- The status line shows the model, mode, reasoning setting, and running token totals.

### One-shot prompts

One-shot mode sends a prompt, runs the assistant turn, saves the session, prints a summary, and exits.

```bash
sofos -p "Find the likely cause of the failing tests"
sofos -p "Create a high-level summary of this crate" --safe-mode
```

### Image vision

Include image paths or URLs directly in your message, or paste images from the clipboard.

```text
What is wrong in ./screenshots/error.png?
Describe "./docs/architecture diagram.webp".
Review https://example.com/chart.png
```

Clipboard paste:

```text
Ctrl+V    # Inserts a numbered marker such as ①.
```

Supported formats: JPEG, PNG, GIF, and WebP. Local images are capped at 20 MB. Paths with spaces should be quoted. Images outside the workspace require Read permission.

---

## CLI reference

```text
-p, --prompt <TEXT>          Run one prompt and exit.
-s, --safe-mode              Start with read-only native tools.
-r, --resume                 Resume a previous session.
    --check-connection       Check provider connectivity and exit.
    --api-key <KEY>          Anthropic API key; overrides ANTHROPIC_API_KEY.
    --openai-api-key <KEY>   OpenAI API key; overrides OPENAI_API_KEY.
    --morph-api-key <KEY>    Morph API key; overrides MORPH_API_KEY.
    --model <MODEL>          Model to use. Default: claude-sonnet-4-6.
    --morph-model <MODEL>    Morph model to use. Default: morph-v3-fast.
    --max-tokens <N>         Maximum output tokens per response. Default: 32768.
-e, --reasoning-effort <LV>  off, low, medium, high, xhigh, or max. Default: medium.
```

`--max-tokens` must be greater than `16384` when reasoning effort is enabled. The deprecated hidden `--thinking-budget` flag still parses for backwards compatibility but has no effect and is intentionally omitted from the CLI help.

---

## Models and reasoning effort

Sofos exposes six reasoning levels:

```text
off, low, medium, high, xhigh, max
```

The active model determines which levels are accepted. Sofos validates the level at startup and when `/think` is used, so unsupported combinations fail before reaching the provider API.

Examples:

```bash
sofos -e medium                       # Default balance.
sofos -e off                          # Lowest-cost path.
sofos -e high                         # More reasoning for hard tasks.
sofos -e max --model claude-opus-4-7  # Highest Anthropic adaptive level.
sofos -e xhigh --model gpt-5.5        # Highest OpenAI gpt-5 reasoning level.
```

Support matrix:

| Effort | Opus 4.7 | Opus 4.6 | Sonnet 4.6 | Haiku 4.5 / older Claude | OpenAI gpt-5 reasoning models |
|---|:---:|:---:|:---:|:---:|:---:|
| `off` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `low` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `medium` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `high` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `xhigh` | ✓ | ✗ | ✗ | ✗ | ✓ |
| `max` | ✓ | ✓ | ✓ | ✗ | ✗ |

Provider mapping:

- **OpenAI** sends `reasoning.effort`; `off` maps to minimal reasoning and suppresses reasoning summaries.
- **Claude Opus 4.7, Opus 4.6, and Sonnet 4.6** use adaptive thinking. The provider chooses the token budget from the effort level.
- **Older Claude models** use fixed legacy thinking budgets for `low`, `medium`, and `high`. `off` disables extended thinking.

---

## Tools

### Native tools

| Tool | Purpose |
|---|---|
| `list_directory` | List one directory. Use `glob_files` for recursive discovery. |
| `read_file` | Read a file. External paths require Read permission. |
| `glob_files` | Find files recursively with glob patterns. Skips build and vendor directories by default. |
| `search_code` | Search code with ripgrep when `rg` is installed. |
| `write_file` | Create, overwrite, or append to a file. External paths require Write permission. |
| `edit_file` | Replace exact text in an existing file. External paths require both Read and Write permission. |
| `morph_edit_file` | Apply fast Morph edits when `MORPH_API_KEY` is configured. External paths require both Read and Write permission. |
| `create_directory` | Create directories. External paths require Write permission. |
| `move_file` | Move or rename files or directories. External paths require Write permission. |
| `copy_file` | Copy files. External sources require Read permission; external destinations require Write permission. |
| `delete_file` | Delete a file after confirmation. External paths require Write permission. |
| `delete_directory` | Delete a directory after confirmation. External paths require Write permission. |
| `execute_bash` | Run approved shell commands through the bash permission system. |
| `update_plan` | Show the current multi-step task plan with `pending`, `in_progress`, and `completed` statuses. |
| `web_fetch` | Fetch a URL and return readable text. |
| `web_search` | Provider-native web search. |

Image vision is not a tool. Sofos detects supported image paths and URLs in user messages and converts them into image content blocks before sending the request.

### Safe mode tools

Safe mode is enabled with `--safe-mode` or `/s`. It restricts the native tool set to:

- `list_directory`;
- `read_file`;
- `glob_files`;
- `search_code` when ripgrep is installed;
- `update_plan`;
- `web_fetch`;
- `web_search`.

MCP tools are filtered out in safe mode by default. To make a particular server's tools available in safe mode, add `safe_mode = "read_only"` (server is known to expose only read operations) or `safe_mode = "allow"` (explicit opt-in even when the server may mutate) to its entry in `~/.sofos/config.toml` or `.sofos/config.local.toml`. Sofos lists which servers are filtered out and which are opted in on the startup banner whenever safe mode is on.

### MCP tools

Configured MCP servers can add tools dynamically. Sofos prefixes each MCP tool with the server name using a triple underscore separator so distinct `(server, tool)` pairs cannot collide on the prefixed name:

```text
filesystem___read_file
github___create_issue
```

Server names and tool names that contain the reserved separator are rejected at startup with a warning. If two servers expose the same tool name, the second registration is skipped so the first one keeps its identifier. Tool listings are cached at startup for the session.

---

## Security model

Sofos is built around explicit access boundaries. The assistant can be useful without receiving unrestricted access to the host system.

### Workspace and external paths

- Files inside the current workspace are available by default unless blocked by a deny rule.
- External paths use three independent scopes:
  - `Read(path)` for reading files and listing directories;
  - `Write(path)` for writing, editing, creating, moving, copying, and deleting;
  - `Bash(path)` for bash commands that reference external paths.
- Read access does not imply Write or Bash access.
- Write access does not imply Read or Bash access.
- Tools that both read and write external files, such as `edit_file` and `morph_edit_file`, require both Read and Write grants.
- External access can be allowed for the current session or remembered in configuration.

### Bash command permissions

Bash commands pass through three layers:

1. **Command tier** — known safe commands may run automatically; dangerous commands are blocked; unknown commands prompt.
2. **Structural checks** — parent traversal, file output redirection, here-documents, and dangerous git operations are blocked regardless of command tier.
3. **Path checks** — commands that reference external absolute or `~/` paths require Bash-path permission.

Default behaviour:

| Tier | Behaviour | Examples |
|---|---|---|
| Allowed | Runs automatically after structural checks. | `cargo`, `npm`, `go`, `ls`, `cat`, `grep`, `rg`, `git status`, `git log`, `git diff` |
| Forbidden | Always blocked. | `rm`, `rmdir`, `chmod`, `chown`, `sudo`, `dd`, `mkfs`, `systemctl`, `kill`, destructive git operations |
| Ask | Prompts the user. | Unknown commands, external paths, `cp`, `mv`, `mkdir`, selected git checkout forms |

### Destructive operations

`delete_file` and `delete_directory` always show a confirmation prompt before deletion. If the user cancels a deletion in a batch of tool calls, Sofos returns synthetic tool results for the skipped tools so the next provider request remains valid.

---

## Configuration

Sofos reads configuration from:

```text
.sofos/config.local.toml     # Workspace-specific, ignored by git.
~/.sofos/config.toml         # Global, optional.
```

Local configuration is loaded in addition to global configuration. Keep `.sofos/` out of version control.

### Custom instructions

Two instruction files are loaded at startup and appended to the system prompt:

| File | Purpose |
|---|---|
| `AGENTS.md` | Project-level instructions. Version controlled. |
| `.sofos/instructions.md` | Personal instructions. Ignored by git. |

Use `AGENTS.md` for team-wide conventions, architecture notes, and project-specific rules. Use `.sofos/instructions.md` for private preferences or machine-local context.

### Permissions

Example permission configuration:

```toml
[permissions]
allow = [
  # Read permissions.
  "Read(~/.zshrc)",
  "Read(~/.config/**)",
  "Read(/etc/hosts)",

  # Write permissions.
  "Write(/tmp/sofos-output/**)",

  # Bash path permissions.
  "Bash(/var/log/**)",

  # Bash command permissions.
  "Bash(custom_tool)",
  "Bash(cargo:*)",
]

deny = [
  "Read(./.env)",
  "Read(./.env.*)",
  "Read(./secrets/**)",
  "Bash(dangerous_tool)",
]

ask = [
  # Ask only applies to Bash commands.
  "Bash(unknown_tool)",
]
```

Rules:

- Deny rules take priority over allow rules.
- `Read`, `Write`, and `Bash` path scopes are independent.
- `*` matches within one path segment.
- `**` matches recursively.
- `Read(/path/**)` also covers `/path` itself.
- `Bash(/path/**)` grants bash path access, not command execution by itself.
- `Bash(command)` grants one exact command.
- `Bash(command:*)` grants commands by base name.
- A bare `"Bash"` in `allow` allows every bash command except built-in forbidden commands; structural checks still apply.
- A bare `"Bash"` in `deny` rejects every bash command.
- `ask` is valid only for Bash command rules.

### MCP servers

Configure MCP servers in either local or global config.

Stdio server:

```toml
[mcp-servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { "GITHUB_TOKEN" = "ghp_YOUR_TOKEN" }
```

HTTP server:

```toml
[mcp-servers.internal-api]
url = "https://api.example.com/mcp"
headers = { "Authorization" = "Bearer token" }
```

Sofos connects at startup, lists available tools, prefixes tool names by server, and caches the list for the session.

---

## Sessions and cost tracking

Sessions are saved automatically under:

```text
.sofos/sessions/
```

A saved session includes:

- provider-facing conversation messages;
- display history for replay;
- system prompt;
- model name where available;
- safe-mode state;
- token counters and cache counters.

Resume with:

```bash
sofos --resume
```

or from inside Sofos:

```text
/resume
```

On exit, Sofos prints token usage and an estimated cost. The summary includes cache-read information when available and accounts for provider cache discounts and cache-write premiums. For OpenAI models with tiered pricing, Sofos tracks the largest single-turn input and switches the estimate when the premium threshold is crossed.

---

## Development

### Project structure

For the complete source structure and ownership map, see [`STRUCTURE.md`](STRUCTURE.md).

High-level layout:

```text
src/
├── api/       Provider clients, shared message types, model metadata.
├── repl/      Turn orchestration, request building, response handling, TUI worker.
├── tools/     Native tool execution, permissions, filesystem, bash, search, image handling.
├── mcp/       Model Context Protocol configuration, clients, manager, transports.
├── session/   Runtime session state and on-disk session persistence.
├── ui/        Markdown, syntax highlighting, diffs, cost summaries, and display helpers.
└── commands/  Slash-command parsing and dispatch.
```

Useful commands:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all
cargo build --release
```

Debug logging:

```bash
RUST_LOG=debug sofos
```

### Release process

This project uses `cargo-release`.

```bash
# Preview a patch release.
cargo release patch

# Execute a patch release.
cargo release patch --execute

# Release a specific increment.
cargo release minor --execute
```

See [`RELEASE.md`](RELEASE.md) for the full process.

---

## Troubleshooting

| Problem | What to check |
|---|---|
| API key error | Set `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`, or pass `--api-key` / `--openai-api-key`. |
| Cannot connect | Run `sofos --check-connection`. |
| Model rejects reasoning effort | Use `/think` or `-e` with a level supported by the selected model. |
| Path denied | Add a `Read`, `Write`, or `Bash` rule, or approve the interactive prompt. |
| External edit denied | `edit_file` and `morph_edit_file` need both Read and Write for external files. |
| Code search unavailable | Install `ripgrep` and ensure `rg` is on `PATH`. |
| Image path with spaces fails | Quote the path: `"path/with spaces/image.png"`. |
| Terminal does not insert newline with Shift+Enter | Use Alt+Enter or Ctrl+Enter. |
| Build problems | Run `rustup update`, then `cargo clean` and `cargo build`. |

---

## License

MIT License. See [`LICENSE`](LICENSE).

---

## Acknowledgments

Sofos is built with Rust and powered by Anthropic Claude or OpenAI models. Optional fast edits are provided through Morph Apply.

---

## Links

- [GitHub](https://github.com/alexylon/sofos-code)
- [Crates.io](https://crates.io/crates/sofos)
- [Release notes](CHANGELOG.md)
- [Source structure](STRUCTURE.md)

---

**Disclaimer:** Sofos Code can make mistakes. Review generated code and tool actions before relying on them.
