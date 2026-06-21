# Sofos Code

![CI](https://github.com/alexylon/sofos-code/actions/workflows/rust.yml/badge.svg) &nbsp; [![Crates.io](https://img.shields.io/crates/v/sofos.svg?color=blue)](https://crates.io/crates/sofos)

Sofos Code is a terminal-based AI coding assistant for software projects. It connects Claude or OpenAI models to local tools for reading code, editing files, running approved commands, searching the web, viewing images, and using external tools through the Model Context Protocol (MCP).

Sofos is written in Rust and runs in your terminal. Its access model is explicit: project files are available by default, while external paths and higher-risk actions require approval or configuration.

Sofos runs on macOS, Linux, and Windows. On macOS and Linux, the default shell mode uses an operating-system sandbox when the required platform support is available. On Windows, command confinement is disabled in this release. The bash executor still runs commands through the `sh.exe` provided by Git for Windows, which Sofos can find at the standard install path even when an integrated terminal does not expose Git on `PATH`.

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
  - [Read-only mode tools](#read-only-mode-tools)
  - [MCP tools](#mcp-tools)
- [Security model](#security-model)
  - [Workspace and external paths](#workspace-and-external-paths)
  - [Access modes](#access-modes)
  - [Sandboxing: reads vs. writes](#sandboxing-reads-vs-writes)
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

Sofos provides an AI assistant with controlled access to your project from inside the terminal. It can:

- inspect files and directories;
- search code with ripgrep;
- edit files with exact replacements or Morph Apply;
- create, move, copy, and delete files with permission checks;
- run approved build, test, and inspection commands;
- fetch documentation and use provider-native web search;
- open local image files or remote image URLs;
- accept image pastes from the clipboard;
- keep a visible task plan during multi-step work;
- save and resume conversations;
- connect to external tools through MCP servers.

The assistant acts through visible tool calls. Dangerous commands are blocked, deletion prompts for confirmation, and access outside the workspace is controlled by separate permission scopes.

---

## Key features

- **Terminal interface** — Inline viewport at the bottom of your terminal while normal scrollback remains available.
- **Claude and OpenAI support** — Shared provider layer with provider-specific streaming, reasoning, web search, and cache handling.
- **Streaming Markdown** — Responses render as they arrive, including code blocks, headings, lists, tables, blockquotes, and links.
- **Iterative tool use** — The model can use tools across multiple steps, with a hard limit to prevent endless loops.
- **Safe file editing** — Exact edits, chunked writes, visual diffs, atomic writes, and optional Morph Apply.
- **Explicit permissions** — Separate Read, Write, and Bash grants for paths outside the workspace.
- **Bash safety checks** — Command tiers and structural checks for parent traversal, hidden subcommands, ANSI-C quoting, unconfined redirection, and dangerous Git operations.
- **Access presets** — Five permission modes: `read-only`, `sandboxed-ask`, `sandboxed-retry`, `sandboxed-strict`, and `unsandboxed`.
- **Image vision** — Local image files, remote image URLs, and pasted clipboard images.
- **MCP integration** — Tools from stdio or streamable HTTP MCP servers.
- **Session persistence** — Saved conversations with compatible model, permission preset, and cost counters restored.
- **Cost visibility** — Token totals, cache usage, and provider-specific cost estimates.
- **Context compaction** — Local and provider-supported compaction for older conversation context.

---

## Installation

### Requirements

Set at least one provider API key:

- `ANTHROPIC_API_KEY` for Claude models; or
- `OPENAI_API_KEY` for OpenAI models.

Optional tools and keys:

- `ripgrep`, recommended for fast code search through `search_code`;
- `MORPH_API_KEY`, required for the `morph_edit_file` tool.

### Prebuilt binary

Download the latest binary from [GitHub Releases](https://github.com/alexylon/sofos-code/releases).

```bash
# macOS / Linux
tar xzf sofos-*.tar.gz
sudo mv sofos /usr/local/bin/

# Windows
# Extract the .zip archive and add the extracted folder to PATH.
```

On macOS, Gatekeeper may block the first run. Open System Settings → Privacy & Security, then choose **Allow Anyway** for the Sofos binary.

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

Set a provider key:

```bash
export ANTHROPIC_API_KEY='your-anthropic-key'
# or
export OPENAI_API_KEY='your-openai-key'
```

Optionally enable Morph Apply edits:

```bash
export MORPH_API_KEY='your-morph-key'
```

Start the interactive assistant:

```bash
sofos
```

Use a different model:

```bash
sofos --model gpt-5.5
sofos --model claude-opus-4-8 -e high
```

Run one prompt and exit:

```bash
sofos -p "Review the error handling in src/error.rs"
```

Start with inspection tools only:

```bash
sofos --readonly
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
| `/clear` | Clear the current conversation history and start a new session id. |
| `/compact` | Compact older context to reduce token usage. |
| `/effort` | Open the reasoning-effort picker. The picker lists only the levels supported by the active model. Use **Up / Down** to select, **Enter** to switch, and **Esc** to cancel. |
| `/effort off\|low\|medium\|high\|xhigh\|max` | Switch directly to a reasoning level. Unsupported levels print a clear error. |
| `/model` | Open the model picker. Use **Up / Down** to select, **Enter** to switch, and **Esc** to cancel. Models from the other provider are greyed out because the API client is fixed at startup. |
| `/model <name>` | Switch directly to a model on the active provider. To switch provider, restart Sofos with `--model <name>`. |
| `/permissions` | Open the permission preset picker. The presets are `read-only`, `sandboxed-ask`, `sandboxed-retry`, `sandboxed-strict`, and `unsandboxed`. Use **Up / Down** to select, **Enter** to switch, and **Esc** to cancel. Where sandboxing is unavailable, such as Windows, the `sandboxed-*` presets are shown but disabled. |
| `/permissions <preset>` | Switch directly to a permission preset. |
| `/exit`, `/quit`, `/q`, `Ctrl+D` | Save the session and exit with a cost summary. |
| `Esc` or `Ctrl+C` while busy | Interrupt the current AI turn. |

### Input behaviour

- **Enter** submits the current message.
- **Shift+Enter** inserts a newline when the terminal supports it.
- **Alt+Enter** and **Ctrl+Enter** are newline fallbacks.
- **Ctrl+U** deletes from the cursor to the start of the line.
- **Ctrl+W** deletes the previous word.
- **Ctrl+K** deletes from the cursor to the end of the line.
- These editing shortcuts match common readline behaviour used by bash, zsh, and fish.
- **Alt+Up** and **Alt+Down** move through previously submitted prompts. Sofos preserves the current draft and restores it when you move past the newest entry.
- Typing `/` at the start of the input opens command suggestions. Use **Up / Down** to select, **Enter** to run the selected command, **Tab** to insert it into the input, and **Esc** or **Ctrl+C** to dismiss the list.
- You can keep typing while the model is working. New messages are queued and processed in order.
- If the model is inside a tool loop, a queued message is delivered at the next tool-result boundary. This lets you steer the current turn without interrupting it.
- The status line shows the model, permission mode, reasoning setting, running token totals, and cache token counters when available.

### One-shot prompts

One-shot mode sends a prompt, runs the assistant turn, saves the session, prints a summary, and exits.

```bash
sofos -p "Find the likely cause of the failing tests"
sofos -p "Create a high-level summary of this crate" --readonly
```

### Image vision

Ask about an image by mentioning the file path or URL in your message. Sofos will call `view_image` to open it.

```text
What is wrong in ./screenshots/error.png?
Describe ./docs/architecture-diagram.webp.
Review https://example.com/chart.png
What do you see in the images in ./assets/?
```

For a folder, Sofos lists the directory first, then opens each image one by one.

Clipboard paste:

```text
Ctrl+V    # Inserts a numbered marker such as ①.
```

Supported formats are JPEG, PNG, GIF, and WebP. Local images are limited to 20 MB. Images larger than 2048 pixels on the long side are scaled down proportionally before being sent to the model, so large screenshots do not inflate token usage unnecessarily. Images outside the workspace require Read permission the first time, like any other external file.

---

## CLI reference

```text
-p, --prompt <TEXT>          Run one prompt and exit.
    --readonly               Start in read-only mode with inspection tools only.
    --no-sandbox             Start unsandboxed: run shell commands without operating-system confinement.
-r, --resume                 Resume a previous session.
    --check-connection       Check provider connectivity and exit.
    --api-key <KEY>          Anthropic API key. Overrides ANTHROPIC_API_KEY.
    --openai-api-key <KEY>   OpenAI API key. Overrides OPENAI_API_KEY.
    --morph-api-key <KEY>    Morph API key. Overrides MORPH_API_KEY.
    --model <MODEL>          Model to use. Default: claude-sonnet-4-6.
    --morph-model <MODEL>    Morph model to use. Default: morph-v3-fast.
    --max-tokens <N>         Maximum output tokens per response. Default: 32768.
-e, --reasoning-effort <LV>  off, low, medium, high, xhigh, or max. Default: medium.
```

`--max-tokens` must be greater than `16384` when reasoning effort is enabled. The hidden, deprecated `--thinking-budget` flag still parses for backwards compatibility, but it has no effect and is intentionally omitted from the CLI help.

---

## Models and reasoning effort

Sofos supports these models, shown in `/model` picker order:

| Model | Provider |
|---|---|
| `claude-fable-5` | Anthropic |
| `claude-opus-4-8` | Anthropic |
| `claude-sonnet-4-6` (default) | Anthropic |
| `claude-haiku-4-5` | Anthropic |
| `gpt-5.5` | OpenAI |
| `gpt-5.4` | OpenAI |
| `gpt-5.4-mini` | OpenAI |
| `gpt-5.3-codex` | OpenAI |

`--model <name>` accepts only the values above. Any other value is refused at startup and Sofos prints the supported list. The same list drives the `/model` picker, so the CLI and picker stay consistent.

Sofos exposes six reasoning levels:

```text
off, low, medium, high, xhigh, max
```

The active model determines which levels are valid. Sofos validates the level at startup and when `/effort` is used, so unsupported combinations fail before a provider request is sent.

Examples:

```bash
sofos -e medium                       # Default balance.
sofos -e off                          # Lowest-cost path.
sofos -e high                         # More reasoning for hard tasks.
sofos -e max --model claude-opus-4-8  # Highest Anthropic adaptive level.
sofos -e xhigh --model gpt-5.5        # Highest OpenAI gpt-5 reasoning level.
```

Support matrix:

| Effort | Fable 5 | Opus 4.8 | Sonnet 4.6 | Haiku 4.5 | OpenAI gpt-5 reasoning models |
|---|:---:|:---:|:---:|:---:|:---:|
| `off` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `low` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `medium` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `high` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `xhigh` | ✓ | ✓ | ✗ | ✗ | ✓ |
| `max` | ✓ | ✓ | ✓ | ✗ | ✗ |

Provider mapping:

- **OpenAI** sends `reasoning.effort`. `off` maps to minimal reasoning and suppresses reasoning summaries.
- **Claude Fable 5, Opus 4.8, and Sonnet 4.6** use adaptive thinking. The provider chooses the token budget from the effort level.
- **Claude Haiku 4.5** uses fixed legacy thinking budgets for `low`, `medium`, and `high`. `off` disables extended thinking.

---

## Tools

### Native tools

| Tool | Purpose |
|---|---|
| `list_directory` | List one directory. Use `glob_files` for recursive discovery. |
| `read_file` | Read a file. External paths require Read permission. |
| `glob_files` | Find files recursively with glob patterns. Build and vendor directories are skipped by default. |
| `search_code` | Search code with ripgrep when `rg` is installed. |
| `write_file` | Create, overwrite, or append to a file. External paths require Write permission. |
| `edit_file` | Replace exact text in an existing file. Non-global edits require one unique match. Use `replace_all` only for intentional global replacement. External paths require Read and Write permission. |
| `morph_edit_file` | Apply fast Morph edits when `MORPH_API_KEY` is configured. External paths require Read and Write permission. |
| `create_directory` | Create directories. External paths require Write permission. |
| `move_file` | Move or rename files or directories. External paths require Write permission. |
| `copy_file` | Copy files. External sources require Read permission, and external destinations require Write permission. |
| `delete_file` | Delete a file after confirmation. External paths require Write permission. |
| `delete_directory` | Delete a directory after confirmation. External paths require Write permission. |
| `execute_bash` | Run approved shell commands through the bash permission system. |
| `update_plan` | Show the current task plan with `pending`, `in_progress`, and `completed` statuses. |
| `view_image` | Attach a local image file or an `http(s)://` URL to the conversation so the model can see it. |
| `web_fetch` | Fetch a URL and return readable text. |
| `web_search` | Use provider-native web search. |

Clipboard pastes are not routed through a tool. Pressing Ctrl+V in the prompt attaches the image directly to the message.

### Read-only mode tools

Read-only mode is enabled with `--readonly` or the `read-only` preset in `/permissions`. It limits the native tool set to:

- `list_directory`;
- `read_file`;
- `glob_files`;
- `search_code` when ripgrep is installed;
- `update_plan`;
- `view_image`;
- `web_fetch`;
- `web_search`.

MCP tools are filtered out in read-only mode by default. To make a server's tools available in read-only mode, add one of these values to that server entry in `~/.sofos/config.toml` or `.sofos/config.local.toml`:

- `readonly = "read_only"` when the server is known to expose only read operations;
- `readonly = "allow"` when you explicitly want to allow the server even if it may mutate data.

When read-only mode is enabled, the startup banner lists which MCP servers were filtered out and which were opted in.

### MCP tools

Configured MCP servers can add tools dynamically. Sofos prefixes each MCP tool with the server name and a triple underscore separator so different `(server, tool)` pairs cannot collide:

```text
filesystem___read_file
github___create_issue
```

Server names and tool names that contain the reserved separator are rejected at startup with a warning. If two servers expose the same tool name, the second registration is skipped and the first tool keeps its identifier. Tool listings are cached at startup for the session.

---

## Security model

Sofos is built around explicit access boundaries. The assistant can work with a project without receiving unrestricted access to the host system.

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

A fourth scope, `WebFetch(domain:example.com)`, controls the `web_fetch` tool. The first time the assistant fetches a URL from a host you have not allowed, Sofos shows the host and asks whether to allow it. You can allow it for the current session or remember it for future sessions. Allowing a host also covers its subdomains, so allowing `example.com` also allows `docs.example.com`. Redirects are followed only after the same check on the destination host. In non-interactive runs, a host that is not already allowed is refused.

### Access modes

Use `/permissions` to choose what the assistant may do. The current preset appears in the status line, and you can switch during a session. From least to most permissive:

- **`read-only`** (also `--readonly`) — Inspection tools only. Writes and shell commands are disabled. The prompt shows `:`.
- **`sandboxed-ask`** (default when a sandbox is available) — Reads and writes are allowed in the project, and shell commands run inside the operating-system sandbox on macOS and Linux. Writes stay inside the project and temporary directories, the network is closed, familiar commands run automatically, and unfamiliar commands run without a prompt. If a command needs network access or must write outside the project, the assistant may ask you to approve running that one command outside the sandbox. The prompt shows `>`.
- **`sandboxed-retry`** — Same confinement as `sandboxed-ask`, but a command may be retried once without the sandbox when the failure appears to be caused by the sandbox.
- **`sandboxed-strict`** — Same confinement, with no sandbox lift. A blocked command fails.
- **`unsandboxed`** (also `--no-sandbox`) — Shell commands run without operating-system confinement. Unfamiliar commands prompt for approval. The prompt shows `#`.

Where no operating-system sandbox can run, the `sandboxed-*` presets are unavailable. This includes Windows and Linux hosts without Bubblewrap, user-namespace support, or the required network filter. In those cases, the default is `unsandboxed`, the picker greys out the sandboxed presets, and the status line reports `unsandboxed`.

Operating-system confinement uses the available platform mechanism:

- **macOS** uses the Seatbelt profile compiler (`sandbox-exec`). Writes are limited to the workspace and system temporary directories, the network is closed, and files blocked by `Read(...)` deny rules stay unreadable even when reached indirectly.
- **Linux** uses Bubblewrap (`bwrap`). The constraints match macOS: writes stay inside the workspace and `/tmp`, network access is closed, local daemon sockets such as Docker are blocked, and files blocked by `Read(...)` deny rules stay unreadable. The `bubblewrap` package and kernel support for user namespaces are required.
- **Windows** does not use operating-system command confinement in this release. The default preset is `unsandboxed`, the `sandboxed-*` presets are disabled, familiar commands run automatically, destructive commands are always refused, and any other command prompts for approval before running. The destructive-command blocklist, `Read(...)` deny rules for named paths, and external-path prompts still apply.

On macOS and Linux, the project `.sofos`, `.agents`, `.claude`, and `.codex` directories stay read-only inside the sandbox, even though the rest of the workspace is writable. The `.git` directory is also read-only for any command other than plain Git commands that need to update repository state, so branch switches and local Git configuration still work while other commands cannot write Git hooks. These directories remain readable.

### Sandboxing: reads vs. writes

Sandboxing handles **writes** and **reads** differently, and that distinction is important.

**Writes are fully confined automatically.** The operating system blocks all writes outside the project, regardless of how the command is written. Network access is also closed.

**Reads are not confined in the same way by default.** An out-of-project read is checked only when the command explicitly names the external path. In that case, you'll see an external-path prompt asking you to approve access.

However, reads that reach files indirectly are not prompted or blocked by default. For example:

- a path stored in a variable
- a recursive directory walk that follows a symlink outside the project

Because of this, out-of-project secrets are **not protected by default**.

To give a path the same kernel-level, unevadable protection that writes have, add a `Read(...)` deny rule for that path. See [Permissions](#permissions). Once configured, the sandbox refuses every read of that path, including indirect reads.

Lifting the sandbox for a single command always requires your approval, and clearly destructive commands remain refused.

### Bash command permissions

Bash commands pass through three checks:

1. **Command tier** — Known safe commands run automatically. Known destructive commands are always blocked. Other commands run without a prompt under a sandboxed preset on macOS and Linux, or prompt for approval on Windows and under `unsandboxed` on any platform. Under a sandboxed preset on macOS and Linux, safe commands are confined too.
2. **Structural checks** — Parent traversal, hidden subcommands (command and process substitution), ANSI-C `$'...'` quoting, and dangerous Git operations are always blocked. File output redirection and here-documents are blocked for commands that run without confinement. Under a sandboxed preset on macOS and Linux, a command whose only such issue is writing to a file runs confined and is allowed. On Windows, the command is refused and the assistant should use `write_file` or `edit_file` instead.
3. **Path checks** — Commands that reference external absolute paths or `~/` paths require Bash-path permission, even when the command runs confined. Confinement limits writes, network access, and files blocked by `Read(...)` deny rules, but it does not otherwise close general read access. An external path that is not denied still needs a Bash-path grant.

| Tier | Behaviour | Examples |
|---|---|---|
| Allowed | Runs automatically after structural checks. Confined by the operating system under a sandboxed preset on macOS and Linux. | `cargo`, `npm`, `go`, `ls`, `cat`, `grep`, `rg`, `git status`, `git log`, `git diff` |
| Forbidden | Always blocked. | `rm`, `rmdir`, `chmod`, `chown`, `sudo`, `dd`, `mkfs`, `systemctl`, `kill`, destructive git operations |
| Other | Sandboxed preset on macOS and Linux: runs confined to the project. Sandboxed preset on Windows, or `unsandboxed` anywhere: prompts. | Unfamiliar commands, `cp`, `mv`, `mkdir`, selected git checkout forms |

### Destructive operations

`delete_file` and `delete_directory` always show a confirmation prompt before deletion. If you cancel a deletion in a batch of tool calls, Sofos returns placeholder results for the skipped tools so the next provider request remains valid.

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

  # Web fetch permissions for a host and its subdomains.
  "WebFetch(domain:blog.rust-lang.org)",
]

deny = [
  "Read(./.env)",
  "Read(./.env.*)",
  "Read(./secrets/**)",
  "Bash(dangerous_tool)",
  "WebFetch(domain:metadata.google.internal)",
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
- A bare `"Bash"` in `allow` allows every bash command except built-in forbidden commands. Structural checks still apply.
- A bare `"Bash"` in `deny` rejects every bash command.
- `ask` is valid only for Bash command rules.

### MCP servers

Configure MCP servers in either local or global configuration.

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

Sofos connects to configured servers at startup, lists available tools, prefixes tool names by server, and caches the tool list for the session.

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
- permission preset, with read-only state restored for older sessions;
- token counters and cache counters.

Resume from the command line:

```bash
sofos --resume
```

Or resume from inside Sofos:

```text
/resume
```

On exit, Sofos prints token usage and an estimated cost. The summary includes cache-read information when available, and accounts for provider cache discounts and cache-write premiums. For OpenAI models with tiered pricing, Sofos tracks the largest single-turn input and switches the estimate when the premium threshold is crossed.

---

## Development

### Project structure

For the complete source structure and ownership map, see [`STRUCTURE.md`](STRUCTURE.md).

High-level layout:

```text
src/
├── api/       Provider clients, shared message types, and model metadata.
├── repl/      Turn orchestration, request building, response handling, and TUI worker.
├── tools/     Native tool execution, permissions, filesystem, bash, search, and image handling.
├── mcp/       MCP configuration, clients, manager, and transports.
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
| API key error | Set `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`, or pass `--api-key` or `--openai-api-key`. |
| Cannot connect | Run `sofos --check-connection`. |
| Model rejects reasoning effort | Use `/effort` or `-e` with a level supported by the selected model. |
| Path denied | Add a `Read`, `Write`, or `Bash` rule, or approve the interactive prompt. |
| External edit denied | `edit_file` and `morph_edit_file` need Read and Write permission for external files. |
| Code search unavailable | Install `ripgrep` and ensure `rg` is on `PATH`. |
| Image not opening | Mention the image by path or URL in your message. For a folder, ask Sofos to look in the folder so it can list and open each image. |
| Terminal does not insert newline with Shift+Enter | Use Alt+Enter or Ctrl+Enter. |
| Sandboxed command cannot reach the network or Docker | This is expected in a sandboxed preset on macOS and Linux. Use a workspace-local alternative, approve a one-command sandbox lift when offered, or switch to `unsandboxed` for a trusted operation. |
| Build problems | Run `rustup update`, then `cargo clean` and `cargo build`. |

---

## License

Apache License 2.0. See [`LICENSE`](LICENSE). Reused third-party code is listed in [`THIRD_PARTY_NOTICES`](THIRD_PARTY_NOTICES).

---

## Acknowledgments

Sofos is built with Rust and uses Anthropic Claude or OpenAI models. Optional fast edits are provided through Morph Apply.

---

## Links

- [GitHub](https://github.com/alexylon/sofos-code)
- [Crates.io](https://crates.io/crates/sofos)
- [Release notes](CHANGELOG.md)
- [Source structure](STRUCTURE.md)

---

**Disclaimer:** Sofos Code can make mistakes. Review generated code and tool actions before relying on them.
