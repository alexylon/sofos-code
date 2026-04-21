# Changelog

All notable changes to Sofos are documented in this file.

## [Unreleased]

### Changed

- **`read_file` output cap raised to ~256 KB** (64k tokens). Previously `read_file` shared the ~64 KB / 16k-token cap with `execute_bash` and `search_code`, which clipped mid-sized source files — generated code, JSON fixtures, long prompt templates — and forced the model into an extra range-reads round trip against the 200-iteration tool-loop budget. `execute_bash` stdout/stderr and `search_code` keep the 16k-token cap, since verbose test output and broad ripgrep patterns benefit from being forced to narrow rather than handing the model noise.

### Fixed

- Prompt glyph (> or :) now correctly reflects the normal/safe modes.

## [0.2.2] - 2026-04-21

### Security

- **Windows absolute paths bypassed the external-path detection** on every filesystem-touching dispatcher (`read_file`, `write_file`, `list_directory`, `glob_files`, `create_directory`, `edit_file`, `morph_edit_file` via the shared resolver), on `execute_bash`'s path scanner, on the image loader, and on the config parser that classifies `Bash(path)` entries. All these call sites used `path.starts_with('/') || path.starts_with('~')`, which only catches the Unix variant — `C:\Users\...` or `\\server\share\...` on Windows slipped through as "relative", got joined to the workspace, and then `Path::join`'s "replace on absolute" rule silently let the path escape. Centralised into two composable helpers in `tools::utils`: `is_absolute_path` (Unix `/foo` + Windows drive / UNC) and `is_absolute_or_tilde` (adds `~` / `~/foo`). Both combine `starts_with('/')` with `Path::is_absolute` rather than relying on either alone — `Path::is_absolute` returns `false` on Windows for a Unix-shaped `/etc/passwd`, which would have re-introduced the bug in reverse.
- **Tilde expansion (`~` / `~/foo`) now works cross-platform** and respects bash-style remainder semantics. Reads `HOME` on Unix and `USERPROFILE` on Windows (previously `HOME`-only, which left a Windows user typing `~/docs` with no expansion and a confusing "file not found" downstream). Composes via `PathBuf::push` so the separator between home and the remainder is platform-native. Leading separators in the remainder are trimmed before composition, so `~//foo` resolves to `~/foo` as bash would — rather than to the raw `/foo` fragment that `PathBuf::push`'s replace-on-absolute behaviour would otherwise produce.
- **`glob_files` could enumerate paths outside the workspace without a permission check.** `path=".."` landed on `workspace.join("..")`, which `read_dir` happily walked as the workspace parent; `path="/etc"` was worse — Rust's `Path::join` replaces with absolute paths, so the walk started at `/etc` directly. Neither went through any permission check. The glob path is now canonicalized and routed through the same `check_read_access` gate used by `list_directory` and `read_file`: relative escapes and unauthorised absolute paths are blocked, while explicitly-allowed external directories (matching a `Read(...)` rule or approved via the interactive prompt) still work for legitimate "review `/some/other/repo`" requests.
- **`glob_files` no longer follows symlinks by default**, matching ripgrep's `rg` behaviour (needs `-L` to follow). Prevents a workspace-internal symlink pointing outside the workspace from leaking filenames under the target directory via the glob walk. Set `follow_symlinks: true` to opt in to the prior behaviour.
- **MCP tool responses are now bounded.** The MCP server is a separate process sofos can't fully sandbox, but it CAN cap the response text before handing it to the model. Previously an oversized MCP reply could reproduce the same "string too long" HTTP 400 that internal tools used to trigger; now the `text` field is truncated at ~1 MB with a hint that the cap came from sofos, not the server.
- **MCP image attachments are now capped** at 10 images or ~20 MB base64 bytes per response, whichever hits first. Multimodal providers count images against a separate budget from text, so a chatty MCP server returning dozens of screenshots could blow past provider limits even when the text was short. The cap is greedy: images are walked in order and kept whenever they still fit under both caps, so a single oversized image in the middle of the response is skipped without blocking smaller images that come after it. A note is appended to the response text after text truncation (so it always survives) telling the model how many attachments were dropped.

### Fixed

- **Write-side path resolution now canonicalises through any number of missing intermediate directories.** When creating a new file or directory, `resolve_for_write` used to canonicalise only as far as the *immediate* parent — if the grandparent (or any further ancestor) was also missing, the resolved path stayed un-canonicalised. Whenever the canonical form of an ancestor differs from its literal form, permission rules written against the canonical prefix silently missed the write, and the operation was denied for paths that should have been allowed. Common places this happens: an intermediate symlink at any depth (platform-independent), macOS's built-in `/tmp` → `/private/tmp` redirection, Windows UNC-prefix normalisation (`C:\foo` → `\\?\C:\foo`), and case folding on case-insensitive filesystems. The resolver now walks up to the nearest existing ancestor, canonicalises it, and re-appends the missing tail components so the returned path always reflects every layer of filesystem indirection on the way down.
- **`edit_file` and `morph_edit_file` no longer corrupt files larger than ~64 KB.** Both tools read the original through the same code path as the `read_file` tool output, which was truncating to the model-facing output cap before the edit was applied. Any file past the cap was silently losing its tail — and gaining a literal `[TRUNCATED: ...]` footer — on every edit. The fix moves the output-cap truncation out of the filesystem layer and into the `read_file` dispatcher, so the edit tools now see the full file regardless of size. Added a regression test (`test_edit_file_preserves_content_past_truncation_cap`) that edits a ~200 KB file and asserts the tail sentinel survives.
- **Tool outputs can no longer crash the request with "string too long" (HTTP 400).** Every tool result that returns variable-size content is now bounded below OpenAI's 10 MB per-output ceiling:
  - `search_code` caps matched lines at 300 columns (with `--max-columns-preview`), skips files over 1 MB, excludes `target/`, `node_modules/`, `.git/`, `dist/`, and `build/` by default on top of `.gitignore`, and truncates total output to ~64 KB. Also adds `--` before the pattern so `pattern="-v"` or `pattern="--files"` is treated literally instead of flipping ripgrep's behaviour.
  - `glob_files` skips the same default excludes and truncates to ~1 MB — a broad pattern like `**/*` over a populated `target/` no longer returns tens of thousands of paths.
  - `list_directory` truncates to ~1 MB for pathological directories.
  - `write_file`, `edit_file`, and `morph_edit_file` diff reports truncate to ~1 MB (ANSI-highlighted diffs of large overwrites previously had no ceiling).

### Changed

- **`edit_file` / `morph_edit_file` now check Read AND Write** for external paths (previously only Write). A user who explicitly granted Write but denied or did not grant Read no longer has their file silently read to compute the diff — the scopes now hold independently. A Write-only grant that used to be sufficient for `edit_file` on an external file will now need a Read grant too.
- **`create_directory`, `move_file`, `copy_file` accept external paths** (absolute and `~/`) with the appropriate permission grants, matching what `write_file` / `edit_file` already supported. `create_directory` and the destination of `move_file` / `copy_file` require Write; the source of `copy_file` requires Read; the source of `move_file` requires Write (the move removes the source). Previously these tools hard-rejected any path outside the workspace.

### Added

- **`search_code` and `glob_files` `include_ignored` parameter** (default `false`). Set to `true` to bypass the built-in excludes (`target/`, `node_modules/`, `.git/`, `dist/`, `build/`) and, for `search_code`, `.gitignore` / `.ignore` filtering. Only set it when you specifically need to look inside build artefacts or vendored code.
- **`glob_files` `follow_symlinks` parameter** (default `false`). Set to `true` to walk through symlinks the way `rg -L` does.

## [0.2.1] - 2026-04-20

### Fixed

- **Windows release build** is no longer broken, so the `x86_64-pc-windows-msvc` binary is produced again.

## [0.2.0] - 2026-04-20

### Added

- **New terminal UI.** New layout with a rounded multi-line input box, a hint line (spinner, queue counter, `esc to interrupt`), and a status line showing the current model, mode, reasoning config, and running token totals. The UI owns a small region at the bottom of the terminal; everything else flows into the terminal emulator's native scrollback, so the scrollbar, mouse-wheel scrolling, and text selection / copy-paste all work exactly like a normal terminal session.
- **Keep typing during an AI turn.** Messages submitted while the model is working are queued in FIFO order. If the model is mid tool-loop, your message is delivered at the next tool-call boundary so the model can course-correct without being interrupted (steered messages echo with a `↑` glyph). Otherwise it runs as the next turn once the current one ends.
- **Soft-wrap in the input box.** Long lines reflow at the terminal width instead of horizontally scrolling; the input grows vertically up to six rows as the content expands.
- **Interactive permission prompts for paths outside the workspace.** Three independent scopes — read, write, and bash — each prompt separately the first time you reference an external path. Decisions can be remembered in config or kept session-scoped.
- **`write_file` `append` parameter** lets the model write files that don't fit in a single response. The first call creates or overwrites; subsequent calls with `append: true` concatenate without rereading the file.
- **ESC / Ctrl+C during an AI turn** aborts the in-flight request immediately instead of blocking until the server responds. Works during streaming, the initial request, the tool-loop response, and the 200-iteration recovery summary.
- **Session resume picker** as an in-TUI overlay (arrow keys / j-k / Enter to load, Esc / Ctrl+C to cancel).
- **Prebuilt release binaries** for macOS, Linux, and Windows via GitHub Actions.

### Fixed

- **Morph edits no longer silently truncate files.** A truncated response from the Morph service now hard-errors instead of being written, and every file write is atomic — a crash or interrupt partway through leaves the original file untouched. Symlinks, executable bits, and restrictive permissions are preserved across the edit.
- **`git checkout <branch>` / `git checkout HEAD~N` / `git checkout -- <path>`** now prompt for confirmation before running. `git checkout -f` and `git checkout -b` remain hard-denied; other forms show the full command and require an explicit Yes.
- **Bash `--flag=/path` bypassed the external-path prompt.** Commands like `grep --include=/etc/passwd …` slipped past the prompt because the whole flag token started with `-`. The path portion is now extracted and checked.
- **OpenAI "No tool call found for function call output" 400** after the conversation was trimmed mid tool-pair. The trim no longer leaves an orphaned tool result at the head of the history; if a message mixes a tool result with a steered user text, the text is preserved while the orphaned result is dropped.
- **OpenAI "roles must alternate" rejection** after ESC during a post-tool API call. The interrupt notice is now folded into the existing turn instead of appended as a second consecutive user message.
- **OpenAI "Missing 'path' parameter"** when the tool-call JSON was cut off by `max_tokens`. A repair ladder recovers truncated arguments; `write_file` accepts `file_path` / `file` / `filepath` / `filename` as aliases for `path` and `text` / `body` / `data` for `content`. The error message back to the model names the fields that WERE received and suggests `append: true` or `edit_file` when the payload was clearly truncated. Morph edits opt out of the repair to avoid silently merging corrupted code.
- **Shift+Enter now inserts a newline** on terminals that implement the kitty keyboard protocol (Ghostty, kitty, Alacritty, WezTerm, iTerm2 with the flag enabled). On Terminal.app and iTerm2 without the flag the emulator itself strips the modifier, so there's no code fix; Alt+Enter works as a universal alternative.
- **UTF-8 panic when reading files with multi-byte characters** (Cyrillic, CJK, emoji) near the truncation boundary.
- **Slash commands like `/exit` now match with trailing whitespace or a stray newline** — previously a Shift+Enter before sending would turn `/exit` into a plain message.
- **Startup no longer hangs** on terminals with slow cursor-position reporting (Ghostty).
- **Window resize no longer leaves ghost rows or prompt bleed-through.** The earlier resize-related deadlocks and stale rows are gone; drag-resize is also noticeably smoother on long sessions.
- **Conversation survives API errors.** A failed request no longer drops the user's in-progress turn.
- **Context summary survives failed compaction.** When compaction fails and falls back to simple trimming, the head summary message stays intact.

### Changed

- **Default `--max-tokens` bumped from 8192 to 32768.** The previous 8192 was too low for modern frontier models writing long documents in a single `write_file` call — responses were cut off mid tool-call, surfacing as the "Missing 'path' parameter" confusion. Smaller models still cap at their own server-side limit, and the value is always overridable via `--max-tokens`.
- **Permission dialog defaults to "Yes".** A bare Enter approves the prompt; Esc / Ctrl+C resolves to "No".
- **Hint row pinned directly above the input box.** Transient state (`processing…`, `esc to interrupt`, `awaiting confirmation`, queue count) sits fixed over the prompt instead of below it; busy time is formatted as `Nm Ns` once it crosses 60 seconds.
- **`cp`, `mv`, and `mkdir`** now prompt interactively instead of being blanket-denied, letting the model repair its own mistakes. `rm`, `rmdir`, `touch`, and `ln` remain hard-denied.
- **Parent-directory traversal check is now token-aware.** Legitimate git revision ranges like `HEAD~5..HEAD` and `git log HEAD~1..HEAD -- src/foo.rs` no longer trip the `..` guard; flag-embedded traversals like `--include=../secret.h` still do.
- **`search_code` tool display** shows a one-line summary (`Found N matches in M files for <pattern>`) instead of dumping full ripgrep output to the terminal; the model still receives the complete results.
- **Morph edits fall back to `edit_file` on timeout** instead of failing the turn.
- **`morph_edit_file` tool** now uses the official Morph field names (`target_filepath`, `instructions`, `code_edit`); older `path` / `instruction` / `file_path` / `file` names are still accepted as fallbacks.
- **`Read(/path/**)` glob** now matches the base directory itself, so `list_directory` on a granted external path works without needing a separate rule for the directory.
- **`write_file`, `edit_file`, and `morph_edit_file`** now support paths outside the workspace with a Write-scope grant.
- **Startup shows the reasoning config** (on/off, budget) alongside model and workspace.

## [0.1.22] - 2026-03-24

### Added
- Clipboard image paste with Ctrl+V: numbered markers (①②③), multi-image support, per-image deletion

## [0.1.21] - 2026-03-24

### Added
- `edit_file` tool for targeted string replacement edits (no external API required)
- `glob_files` tool for recursive file pattern matching (`**/*.rs`, `src/**/mod.rs`)
- `web_fetch` tool for fetching URL content as readable text
- Syntax highlighting inside diffs with line numbers
- Streaming infrastructure for Anthropic SSE API (disabled pending incremental markdown rendering)

### Changed
- Renamed project instructions file from `.sofosrc` to `AGENTS.md` per the [AGENTS.md](https://agents.md) convention for providing project context to AI agents
- Replaced yanked `pulldown-cmark-mdcat` with `pulldown-cmark` + custom ANSI markdown renderer
- Improved diff display: darker backgrounds (#5e0000 / #00005f), syntax-colored code, line numbers
- Tool output with ANSI formatting (diffs) no longer dimmed

## [0.1.20] - 2026-03-23

### Added
- Conversation compaction: replaces naive message trimming with intelligent context preservation
  - Two-phase approach: truncates large tool results first, then summarizes older messages via the LLM
  - Works with both Anthropic and OpenAI providers
  - Auto-triggers at 80% of token budget before sending the next request
  - `/compact` command for manual compaction
  - Shows "Compacting conversation..." animation during summarization
  - Falls back to trimming on failure or ESC interrupt

### Changed
- Increase `MAX_TOOL_OUTPUT_TOKENS`

### Fixed
- Gracefully handle invalid image URLs in conversations and resumed sessions

## [0.1.19] - 2026-01-08

### Changed
- Simplified cargo-release configuration to use built-in publishing

## [0.1.18] - 2026-01-08

### Added
- cargo-release automation for versioning and publishing
- Comprehensive release documentation

## [0.1.17] - 2026-01-05

### Changed
- Renamed `mcpServers` to `mcp-servers` in config
- Reduced `max_context_tokens` for cost optimization

### Fixed
- Image detection failing when messages contain apostrophes/contractions

## [0.1.16] - 2026-01-04

### Added
- MCP (Model Context Protocol) server integration for extending tool capabilities
- Cost optimizations: caching, token-efficient tools, and output truncation

### Changed
- Updated `reqwest` crate
- Display all loaded MCP servers on app startup
- Updated `is_blocked` error messages

### Fixed
- MCP image handling: use separate image blocks instead of embedded base64
- Web search tool result block filtering for Anthropic

## [0.1.15] - 2025-12-31

### Added
- Cyrillic character support in history

### Changed
- Reorganized files and logic into cleaner structure

## [0.1.14] - 2025-12-24

### Added
- Markdown formatting and code syntax highlighting

## [0.1.13] - 2025-12-23

### Added
- Cursor shape changes depending on edit mode
- Improved ripgrep detection and empty file type handling

### Changed
- Handle image paths with spaces
- Removed blinking cursor after command execution
- Updated readline library and prompt symbols
- Included image tool in safe mode

### Fixed
- Abort API calls on ESC for immediate REPL interruption
- Auto-continue OpenAI reasoning-only responses

## [0.1.12] - 2025-12-22

### Added
- Local and web image vision support
- Context token limiting
- Actionable hints to all error messages
- Enhanced confirmation dialogs with icons, colors, and safe defaults
- Security level distinction for error messages

### Changed
- Standardized message formatting with two-line hint structure
- Improved RawModeGuard usage for panic safety
- Network resilience improvements and crash point elimination
- Added thread join timeout to prevent UI hangs
- Retry jitter and Unix signal detection

### Fixed
- Session-scoped permissions

## [0.1.11] - 2025-12-21

### Added
- Global config support in `~/.sofos/config.toml`
- Read permission system with glob patterns and tilde expansion
- Homebrew install option

### Changed
- Block tilde paths in bash to enforce workspace sandboxing
- Type safety improvements and centralized config
- Updated project structure documentation

### Security
- Allowed `2>&1` for stderr/stdout combining in bash

## [0.1.10] - 2025-12-21

### Added
- Network resilience and crash point elimination

## [0.1.9] - 2025-12-22

### Added
- Prompt caching for Claude API
- Caching to read_file_tool
- System prompt support for OpenAI

### Changed
- Refactored prompt caching system

## [0.1.8] - 2025-12-18

### Added
- 3-tier permission system for bash execution (Allow/Deny/Ask)
- Config migration from JSON to TOML
- Refactored type safety and centralized config

## [0.1.7] - 2025-12-18

### Changed
- Migrated config from JSON to TOML format

## [0.1.6] - 2025-12-18

### Security
- Implemented 3-tier permission system for bashexec: Allow, Deny, Ask

## [0.1.5] - 2025-12-15

### Added
- Prompt caching for Claude
- System prompt handling for OpenAI

## [0.1.4] - 2025-12-14

### Added
- Installation from crates.io
- Documentation links
- Crates.io version badge
- Links and resources section to README

## [0.1.3] - 2025-12-14

### Changed
- Keywords and metadata updates

### Added
- Support for multiple installation methods (Homebrew, crates.io)

## [0.1.2] - 2025-12-10

### Added
- OpenAI API support
- OpenAI web search tool
- Support for all GPT-5 models with Responses API
- OpenAI reasoning model handling
- Safe mode for restricted capabilities
- Tab-based command selection (replaced rustyline with reedline)

### Changed
- Enabled Morph integration for OpenAI models
- Model pricing fixes

### Fixed
- Conversation workflow improvements

## [0.1.1] - 2025-12-09

### Added
- Morph Fast Apply integration
- Ripgrep code search integration
- Bash executor with safety checks
- File operations: delete, move, copy
- Programmatic confirmation for destructive operations
- Thinking animation
- Visual diff display
- Session save/restore functionality
- Claude web-search tool integration
- Syntax highlighting
- Team and personal instructions support
- Git operations restrictions (read-only)
- Reject reasons for restricted bash commands
- Bash output size limiting (50MB)
- Conversation history limiting
- File size checks (10MB limit)
- Extended thinking capability
- Iterative tool execution loop (max 200 iterations)
- Retry logic with session preservation for network failures
- Token usage and estimated cost display
- ESC key to interrupt API calls
- Separate REPL logic refactoring

### Changed
- Replaced recursion with iterative loop for tool execution
- Updated dependency versions
- Improved README documentation
- Updated default Claude model

### Fixed
- Symlink escape prevention
- File size limit enforcement
- Conversation workflow
- Warning fixes

## [0.1.0] - 2025-12-04

### Added
- Initial release
- Claude AI integration for coding assistance
- Interactive REPL with session persistence
- File system operations (read, write, list, delete, move, copy)
- Sandboxed bash command execution
- Code search via ripgrep
- Tool calling with iterative execution
- Conversation history management
- Custom instructions support (.sofosrc)
- API request building and response handling
- Error handling with user-friendly messages

---

**Versioning:** This project follows [Semantic Versioning](https://semver.org/).
