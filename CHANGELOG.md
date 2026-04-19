# Changelog

All notable changes to Sofos are documented in this file.

## [Unreleased]

### Added
- **Mid-turn message steering**: messages typed while the worker is running a tool loop are now drained between tool iterations and folded into the same user turn that carries the tool results, so the model can course-correct without being interrupted. Steered messages echo with a `↑` glyph and a "queued for delivery before the next tool call" subtitle; residual messages (turns that end without hitting a drain point) are flushed as a new `Job::Message` on `WorkerIdle`.
- **Ratatui-based TUI** replacing the reedline REPL loop. Claude Code-inspired layout with a rounded multi-line input box, a hint line with spinner / queue counter, and a live status line showing model / mode / reasoning config / running token totals.
  - **Inline viewport** via ratatui's `Viewport::Inline(N)` — the TUI only owns a small region at the bottom of the terminal, and all captured stdout/stderr flows above it through `Terminal::insert_before`. That means the terminal emulator provides the scrollback, native scrollbar, mouse-wheel scrolling, and text selection / copy-paste — no reimplementation of any of those inside the app.
  - **Message queueing while the AI is working**: keep typing during an AI turn; Enter pushes the message onto a FIFO queue that drains automatically when the worker becomes idle.
  - **Background worker thread** owns the `Repl` and processes jobs sequentially; UI and worker communicate via `mpsc` / tokio `UnboundedSender`. State changes (`/s`, `/n`, `/think on|off`, `/clear`, `/resume`) push a fresh `StatusSnapshot` to the UI so the status line updates instantly.
  - **Stdout/stderr capture**: fds 1 and 2 are redirected to `os_pipe` pipes at startup so every existing `println!`/`eprintln!`/`colored` call streams into the terminal scrollback via `insert_before`; rendering uses `/dev/tty` as a separate `CrosstermBackend` sink.
  - **ESC / Ctrl+C** sets a shared `Arc<AtomicBool>` watched by the API request loops, replacing the old per-call raw-mode animation threads.
  - **Resume picker** as a TUI overlay (arrow keys / j-k / Enter / Esc / Ctrl+C).
- Interactive permission prompts for external directory access with three separate scopes:
  - `Read(/path/**)` — read/list files outside workspace
  - `Write(/path/**)` — write/edit files outside workspace
  - `Bash(/path/**)` — bash commands referencing external paths
  - User is prompted to allow or deny, with option to remember the decision in config
  - Session-scoped caching for non-persisted decisions
- GitHub Actions release workflow for prebuilt binaries (macOS, Linux, Windows)
- **`write_file` `append` parameter** for writing files larger than a single `max_output_tokens` response can emit. First call (append omitted / false) creates or overwrites; subsequent calls with `append: true` concatenate. The tool description guides the model toward this pattern, and the append path uses `OpenOptions::append(true).create(true)` so chunks don't incur the quadratic cost of read-modify-write.
- **Soft-wrap in the TUI input box** — long lines now reflow at the terminal width instead of horizontally scrolling. The input box grows vertically (up to 6 content rows) as the wrapped content expands, using tui-textarea's own measurement so the growth tracks real wrapped rows rather than logical lines.

### Removed
- `reedline` dependency, `ReplPrompt`, `ClipboardEditMode`, and `UI::run_animation_with_interrupt` — all superseded by the ratatui TUI.

### Fixed
- **Morph-edited files silently truncated**. Four compounding issues: `MorphRequest` now sends an explicit `max_tokens: 64000` (Morph's server default was smaller for some revisions, causing silent truncation); `finish_reason` is parsed and `"length"` / `"max_tokens"` hard-errors instead of returning truncated content; a new `validate_morph_output` sanity check rejects empty responses, near-empty stubs on files over 500 bytes, and trailing-newline parity mismatches before anything touches disk; `write_file` / `write_file_with_outside_access` now write atomically via a sibling `.sofos.tmp` + `rename`, so a crash partway through a write leaves the original file intact. The atomic writer `canonicalize`s up front so symlinks are preserved (the rename replaces the real target, not the link itself), and on Unix copies the original file's permission bits onto the tmp before the swap so an executable script stays executable and a private config (`0600`) stays private.
- **OpenAI "No tool call found for function call output with call_id …" 400**: `trim_if_needed` removes messages from the front one at a time and could leave a user `ToolResult` stranded after its matching assistant `ToolUse` was dropped, producing an orphaned `function_call_output` in the serialized request. A new `drop_leading_orphaned_tool_results` pass runs after both trim loops and strips any leading message that still carries a `ToolResult` block.
- **Thinking block lost its dim+italic styling after the first line** in the TUI. The pipe reader delivers the log one line at a time and `ansi-to-tui` parses each line in isolation, so multi-line SGR wrappers dropped on lines 2+. `UI::print_thinking` now wraps each line individually.
- **Cursor-position query timing out on startup** (`Io(Custom { ... error: "The cursor position could not be read within a normal duration" })`). `crossterm::cursor::position()` writes its DSR to `io::stdout()`, not to the ratatui backend writer — so after `OutputCapture` redirected fd 1 to a pipe, the query was swallowed. The terminal is now constructed before `OutputCapture` is installed, and `OutputCapture::pause` / `resume` briefly swap fd 1 / fd 2 back to the real tty around resize-driven draws so `autoresize`'s cursor re-query can succeed.
- **Maximizing the window no longer crashes the TUI** with the same "cursor position could not be read within a normal duration" error. Root cause is separate from the startup variant: on resize, ratatui's `autoresize` → `compute_inline_size` → `crossterm::cursor::position()` tries to acquire crossterm's global `INTERNAL_EVENT_READER` mutex, which the input-reader thread holds while blocked inside `event::read()`. A new `SafeBackend` wrapper flips to a synthesized cursor position (top of the inline viewport) after startup, so autoresize reflows the viewport without issuing the DSR that would deadlock against the reader lock. The synthesized position is chosen so `compute_inline_size` emits exactly `viewport_height − 1` newlines — enough to move the cursor to the bottom row without scrolling any rows into scrollback on every resize.
- **Shift+Enter now reliably inserts a newline** in the TUI input box. The earlier poll-based fix for the resize deadlock inadvertently changed key event delivery for modified Enter; reverted that change in favour of the `SafeBackend` approach above, which targets the deadlock without touching `event::read`.
- **UTF-8 panic when reading files containing multi-byte characters**: `truncate_for_context` in `src/tools/filesystem.rs` sliced at a raw byte index that could land inside a Cyrillic / CJK / emoji scalar (`byte index 64000 is not a char boundary; it is inside 'ъ'`). Now snaps to the nearest char boundary via the shared `truncate_at_char_boundary` helper already used by `bashexec` and `web_fetch`.
- **Slash commands ignored when the input had trailing whitespace or a stray newline**. `Command::from_str` compared the full input to an exact literal, so `"/exit "`, `"/exit\n"`, or `"  /exit  "` (e.g. a stray Shift+Enter before send) fell through to being sent as a plain message. Commands now match after trimming surrounding whitespace; the trim rule lives in a single place and covers both the `is_command` branch and the dispatch branch.
- **`write_file` "Missing 'path' parameter" from OpenAI** when a large multi-line / multi-byte `content` payload caused the tool-call JSON to be cut off at `max_output_tokens` before `"path"` could be emitted. Fixes land at four levels:
  - **JSON repair ladder** in `src/api/utils.rs::parse_tool_arguments` — trim, drop trailing commas, escape raw `\n`/`\r`/`\t` inside string literals (tracking `"` nesting with backslash awareness so `\"` doesn't flip the state), close an unterminated string and tack on the missing closing brace, unwrap one level of double-encoding. Recovers the partial object so the dispatcher can surface a targeted "missing parameter" error including the keys that WERE provided. Anthropic now uses the same ladder — streaming `input_json_delta` assembly can hit the exact same truncation.
  - **`status: "incomplete"` / `incomplete_details.reason` parsing** on OpenAI's Responses API, mapped to the shared `stop_reason: "max_tokens"` so the existing "Response was cut off due to token limit" warning fires for OpenAI too.
  - **Parameter-name aliases** on `write_file`: `path` accepts `file_path` / `file` / `filepath` / `filename`, and `content` accepts `text` / `body` / `data`. When nothing matches, the error message lists the keys that WERE supplied so the model can self-correct.
  - **Truncation-aware error message** — when the only key present is `content`, the error sent back to the model explicitly tells it the previous response was likely cut off mid-call and suggests splitting the write via `append: true` or `edit_file`.
- **Stale-buffer artifacts on TUI resize** (previous prompt text bleeding through after maximize). Ratatui's `Terminal::clear` only resets the back buffer on resize, leaving the front buffer's flat-`Vec<Cell>` indices mapped to different `(x, y)` under the new width — stale cells showed through wherever the components don't explicitly write. Addressed by the `SafeBackend`-driven resize flow (no off-screen scrolling on each resize) combined with the already-present per-frame rendering of the input box.

### Changed
- **Default `--max-tokens` bumped from 8192 to 32768.** The previous 8192 cap was too low for modern frontier models writing long documents in a single `write_file` call — responses were cut off mid-tool-call, surfacing as the "Missing 'path' parameter" confusion. Claude Sonnet 4 and GPT-4.1 both support 32k+ output; smaller models cap at their own server-side limit, so the bump is safe as a default and is always overridable via `--max-tokens`.
- **Provider-common API logic consolidated into `src/api/utils.rs`.** Both `anthropic.rs` and `openai.rs` now delegate the shared bits — HTTP-client construction (`build_http_client`, which merges caller auth headers with the default `Content-Type: application/json` and the shared `REQUEST_TIMEOUT`), final response assembly (`build_message_response`, which centralises the `_response_type` / `_role` protocol constants and the `Usage` wrapping), and the tool-argument JSON repair ladder (`parse_tool_arguments`, used to be OpenAI-only) — leaving each client file with only the provider-specific code: SSE event parsing and `sanitize_messages_for_anthropic` on the Anthropic side, `build_response_input` / `OpenAIResponse` item types / `status → stop_reason` mapping on the OpenAI side. `morph.rs` picked up `build_http_client` too.
- **Hint row pinned above the input box** so transient state (`processing…`, `esc to interrupt`, `awaiting confirmation`, queue count) sits fixed directly over the prompt instead of below it. Busy elapsed time is formatted as `Nm Ns` once it crosses 60 seconds.
- **Permission dialog defaults to "Yes"**: the initial cursor sits on the first choice (`Yes`) so a bare Enter approves. The Esc / Ctrl+C fallback still resolves to `default_index` (`No`), so cancelling stays safe.
- **Bash path-traversal check is now token-aware**. The old substring check on `..` blocked legitimate git revision ranges like `HEAD~5..HEAD` and `git log HEAD~1..HEAD -- src/foo.rs`. The new `has_path_traversal` helper only flags `..` when it's a path component (`..`, `../foo`, `foo/..`, `foo/../bar`).
- **Bash file-recovery commands unblocked**. `git restore <path>` and `git checkout -- <path>` are removed from `dangerous_git_ops` so the model can roll back a corrupted file without going through the write tools. `git checkout -f`, `git checkout -b`, and the destructive variants stay blocked.
- **`cp` / `mv` / `mkdir` downgraded from `Denied` to `Ask`**. The user is prompted interactively instead of being blanket-denied, which lets the model repair its own mistakes. `rm` / `rmdir` / `touch` / `ln` stay on the hard-deny list.
- **Switched from fullscreen + alt screen to `Viewport::Inline`**. The TUI now owns a 7-row region at the bottom of the user's normal terminal (input + hint + status). All captured stdout/stderr flows above the viewport via `Terminal::insert_before`, so the terminal emulator provides native scrollback, scrollbar, mouse wheel, text selection, and copy-paste for sofos' output — no custom log buffer, no custom scrollbar widget.
- `search_code` tool display now shows a one-line summary (`Found N matches in M files for <pattern>`) instead of dumping full ripgrep output to the terminal; the LLM still receives the full results
- Morph edit falls back to `edit_file` on timeout instead of failing; added truncation marker guards for `edit_file` and `morph_edit_file`
- `Read(/path/**)` glob now also matches the base directory itself (for `list_directory`)
- `morph_edit_file` tool schema now matches the official Morph Fast Apply schema (`target_filepath`, `instructions`, `code_edit`); legacy `path`/`instruction`/`file_path`/`file` names are still accepted as fallbacks
- Bash commands with absolute or tilde paths now trigger interactive path grant instead of hard block
- Show reasoning config at startup
- `write_file`, `edit_file`, and `morph_edit_file` now support external paths (with Write scope permission)

- `morph_edit_file` `Missing 'path' parameter` errors on OpenAI: root cause was field-name divergence from the official Morph Fast Apply schema (we used `path`/`instruction`; the canonical schema is `target_filepath`/`instructions`), which models trained on Morph's schema would emit, leading to mismatched parameters. The tool schema is now aligned with Morph's docs. OpenAI tool-call arguments are parsed with a small repair ladder (trim → drop trailing commas → close missing brace, falling back to `{"raw_arguments": args}` so the model can self-correct), except `morph_edit_file` which uses strict `serde_json::from_str` to avoid silently merging truncated `code_edit` payloads into files.
- UTF-8 panics in string truncation when cutting multi-byte characters
- OpenAI tool call argument parsing for malformed/incremental JSON
- OpenAI tool call encoding issues
- Conversation preservation on API errors
- Preserve context summary when fallback trim drops messages during failed compaction

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
