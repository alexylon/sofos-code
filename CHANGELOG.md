# Changelog

All notable changes to Sofos are documented in this file.

## [Unreleased]

### Fixed

- **Ctrl+U in the TUI input now deletes from the cursor to the start of the line** (readline / Claude Code convention), instead of undoing the last edit. Ctrl+K (delete to end of line) and Ctrl+W (delete previous word) already worked.
- **Session summary "Estimated cost" now accounts for the cache discount.** A high cache-hit rate previously overstated the bill by ~3× (every input token billed at full rate). Cache reads are now priced at 10% of the base input rate on both providers; Anthropic 5-min cache writes at 125%.

### Added

- **Cache-hit indicator in the session summary.** Adds `cache read: N (M% hit)` and (Anthropic only) `cache write: N` rows when there's any cache activity. The Input row now shows the total tokens the model saw (cached + uncached) on both providers — previously Anthropic's row understated by the cached portion.
- **"Finished in Xs" turn-completion marker.** A dimmed `Finished in 1m 34s` prints after the assistant fully completes a turn, so the prompt-ready signal is unambiguous. Steer messages typed mid-turn don't reset the timer; skipped on interrupt or error.
- **Bare `"Bash"` entry in `permissions.allow` / `permissions.deny` acts as a blanket rule.** `allow` auto-passes every bash command (except the built-in forbidden set: `rm`, `chmod`, `sudo`, …); `deny` auto-rejects every bash command. Blanket entries beat more-specific rules; deny wins when both are set. Structural safety (`>` redirection, `git push`, parent traversal, external paths) still applies.

### Changed

- **Permission check walks every sub-command of a compound shell.** `for f in *.rs; do echo $f; sed -n '1,320p' $f | nl -ba; done` is now auto-allowed when every step is on the read-only allow-list — previously it hit `Ask` because only the leading `for` keyword was checked. The walk splits on `;`, newlines, `|`, `||`, `&&` (quote-aware; `2>&1` preserved). Volatile-args detection (`sed -n '1,N'p`, `head -n N`, `grep -A N`, `awk 'NR==N'`) follows the same path, so the Yes/No-only prompt fires for volatile sub-commands buried in a `for` loop or `&&` chain.

### Security

- **`cat foo && rm bar` no longer slips past as Allowed.** Compound shells now check every sub-command against the forbidden set (`rm`, `chmod`, `sudo`, …) — previously only the first base was looked up. Commands smuggled inside `$(...)` or backticks remain invisible to the permission system, same as before this change.

## [0.2.6] - 2026-05-01

### Fixed

- **Anthropic prefix cache survives wide multi-tool iterations.** A single iteration adding more than ~20 blocks (parallel tool calls returning many blocks at once) used to cold-miss the rolling cache lookup and re-bill the entire prefix at full price. A stateful secondary anchor breakpoint now keeps the prefix cached across wide turns. Intermediate blocks between the anchor and the rolling tail still re-bill on wide turns — a fundamental limit of Anthropic's 20-block lookback window.
- **Cost no longer grows exponentially across iterations.** Two cache-invalidation bugs introduced with the 800k context budget caused each agent-loop iteration to re-bill the entire prefix at full price (~$1–2 → ~$15 → ~$50). Fixed by pinning a stable `prompt_cache_key` to the session id on every OpenAI request, and removing the mid-loop tool-result truncation (the "phase-1 compaction" added in 0.2.5) that evicted the cached suffix on every iteration. `/compact` still handles structural summarization.
- **Anthropic gets a rolling cache breakpoint** on the last block of each request, so the cached prefix grows with the conversation instead of restarting on each turn.

### Added

- **Cache-hit observability.** Usage now carries `cache_read_input_tokens` (both providers) and `cache_creation_input_tokens` (Anthropic), so the cost line can show a hit indicator.

## [0.2.5] - 2026-04-29

### Added

- **Per-model auto-trim budget.** The conversation auto-trim threshold now picks 800k tokens for flagship Claude / GPT-5.5 models and 300k for Codex variants (which have a 400k API window), instead of a single 165k default that didn't match any modern model.
- **In-loop phase-1 compaction.** The agent loop now truncates large tool-result payloads in older messages between iterations, so a long tool chain (file dumps, verbose bash) doesn't push usage past the trigger ratio with no relief until manual `/compact`. Purely local and history-preserving — every message stays in place, only big tool-result bodies shrink. Phase 2 (LLM summarization) is still gated behind explicit `/compact`.

### Fixed

- **"Approaching token limit" warning no longer spams once stuck at the floor.** A long agent loop used to print one warning per tool round-trip while at the 10-message floor and over budget. Now fires once on entry and clears once back under budget. Rephrased to `Auto-trim hit the 10-message floor at ~N tokens (budget M). Run /compact or /clear if responses start degrading.`

## [0.2.4] - 2026-04-27

### Changed

- **Permission prompt drops the "and remember" options for bash commands whose args won't repeat.** Persisting `sed -n '1270,1320p'`, `head -n 50`, `tail -n 100`, `grep -A 5 pat`, `awk 'NR==5'` as exact strings was useless because the numeric arg changes every call. Sofos now detects these shapes per pipe segment and falls back to a plain Yes/No prompt. To allowlist a whole invocation family, add a `Bash(cmd:*)` entry to `.sofos/config.local.toml`.
- Updated AI model pricing in the cost summary.

### Added

- **`nl` added to the built-in auto-allow list** alongside `cat`, `head`, `tail`, `less`, `more`. Pipelines like `nl -ba file.rs | sed -n '1270,1320p'` no longer prompt.

## [0.2.3] - 2026-04-23

### Added

- **Claude Opus 4.7 adaptive-thinking support.** Opus 4.7 rejects the older `{thinking: {type: "enabled", budget_tokens: N}}` shape; sofos now sends `{thinking: {type: "adaptive"}, output_config: {effort}}` instead. `/think on` maps to `effort: high`, `/think off` to `effort: low`. Status line and startup banner show `Adaptive thinking effort: high|low` instead of a fake token budget.
- **Confirmation modal now fits short terminals.** The 4-choice permission prompt grows the viewport when it can; when it can't, drops the separators / hint row and scrolls the choice list around the cursor with `▴` / `▾` cues.
- **Visible feedback on `/s` and `/n`** (safe-mode toggles). Now prints a one-line status (`Safe mode: enabled / read-only tools only; no writes or bash`, `Safe mode: disabled / all tools available`, or a dimmed `already enabled/disabled`).

### Security

- **`FOO=bar rm -rf /` no longer bypassed the forbidden-command check.** Leading `KEY=value` env assignments are now skipped before the base-command lookup, so the real command (`rm`) is checked.
- **Forbidden-git detection now covers shell substitution boundaries.** `` echo hi; `git push` ``, `echo $(git push)`, `(git push)`, and `{ git push; }` no longer slip past — backticks, `$(…)`, `(…)` subshells, and `{…;}` groups are all recognised as command boundaries.
- **Clipboard paste is now bounded at 20 MB**, matching the Anthropic Messages API ceiling on base64-encoded image bodies. Oversized screenshots are dropped before they enter the conversation.

### Fixed

- **MCP requests are now bounded at 120 seconds.** A frozen stdio MCP server used to wedge every subsequent MCP call. HTTP MCP clients gain an explicit timeout for the same reason — previously they inherited reqwest's unlimited default.
- **MCP child processes are now reaped on drop** instead of lingering as zombies until sofos exits.
- **Concurrent sofos processes no longer lose session index entries.** Two instances in the same directory used to race each other's index updates and silently clobber session metadata. Saves and deletes now serialise via an OS-level exclusive lock on `.sofos/sessions/.save.lock`.
- **"Goodbye!" no longer prints on the same line as the status row on exit** when the session summary is skipped (zero usage).
- **Safe-mode tool list now matches reality.** The model used to be told it had only `list_directory, read_file, web_search`, but actually got `list_directory, read_file, glob_files, web_fetch, web_search` (+ `search_code` when ripgrep is present).
- **Corrupted session file on save no longer wipes the in-memory conversation.** Saving used to re-read the prior file to preserve `created_at`; if unparseable, it would abort and drop the current turn's messages. Now falls back to `now` instead.
- **Empty-signature thinking blocks no longer round-trip.** A streamed thinking block that ended without a signature used to be persisted with an empty signature, which the server then rejected on the next turn with a 400.
- Streaming no longer prints a bare `Thinking:` label when the first delta is empty.
- Prompt glyph (`>` / `:`) now correctly reflects normal vs safe modes.

### Changed

- **LLM request timeout raised to 30 minutes; retries removed for Anthropic and OpenAI.** The previous 5-minute client timeout didn't fit Opus 4.7 adaptive thinking at high effort, and the retry replayed the same long thinking twice more before failing. Timeouts, connect errors, and 5xx now surface immediately rather than quietly re-running an expensive call. Morph keeps its retry policy (5xx only, up to 2 retries with jittered backoff) and now falls back to `edit_file` on any failure. Morph's own ceiling is raised from 30 s to 10 minutes.
- **"User declined" prompt phrasing nudges the model to pivot instead of retrying.** Replaced `Command blocked by user: 'X'` with `User declined 'X'. Propose a different approach or ask the user to clarify rather than retrying the same command.`
- **`/think` wording aligned.** Banner, `/think on`, and `/think off` all read `Extended thinking: enabled` / `Extended thinking: disabled`.
- **`read_file` output cap raised to ~256 KB** (64k tokens) — previously it shared the ~64 KB / 16k-token cap with `execute_bash` and `search_code`. Bash and search keep the smaller cap so verbose output is forced to narrow.

## [0.2.2] - 2026-04-21

### Security

- **Windows absolute paths bypassed the external-path detection** on every filesystem-touching tool. The `starts_with('/')` checks only catch the Unix variant — `C:\Users\…` and `\\server\share\…` slipped through as "relative", got joined to the workspace, and then escaped via `Path::join`'s replace-on-absolute rule.
- **Tilde expansion (`~` / `~/foo`) now works cross-platform** — reads `HOME` on Unix and `USERPROFILE` on Windows (previously `HOME`-only). `~//foo` resolves to `~/foo` like bash.
- **`glob_files` could enumerate paths outside the workspace without a permission check.** `path=".."` walked the workspace parent; `path="/etc"` walked `/etc` directly. The path is now canonicalized and routed through the same permission gate as `list_directory` / `read_file`.
- **`glob_files` no longer follows symlinks by default**, matching `rg`. Prevents a workspace-internal symlink pointing outside the workspace from leaking filenames via the glob walk. Set `follow_symlinks: true` to opt in.
- **MCP tool responses are now bounded.** The `text` field is truncated at ~1 MB so an oversized MCP reply can't trigger a provider "string too long" 400.
- **MCP image attachments are now capped** at 10 images or ~20 MB base64 per response, whichever hits first. The cap is greedy: smaller images after a single oversized one are still kept.

### Fixed

- **Write-side path resolution now canonicalises through any number of missing intermediate directories.** Previously only the immediate parent was canonicalised, so an intermediate symlink, macOS's `/tmp` → `/private/tmp` redirection, Windows UNC normalisation, or case folding would silently break write permissions for paths that should have been allowed.
- **`edit_file` and `morph_edit_file` no longer corrupt files larger than ~64 KB.** Both tools read the original through `read_file`'s output cap, which silently lost the tail and added a `[TRUNCATED: …]` footer on every edit. The cap is now applied only at the model-facing dispatcher, not in the filesystem layer.
- **Tool outputs can no longer crash the request with "string too long" (HTTP 400).** Every variable-size tool result is now bounded below OpenAI's 10 MB ceiling:
  - `search_code`: ~64 KB cap, 300-column lines, files over 1 MB skipped, default excludes for `target/`, `node_modules/`, `.git/`, `dist/`, `build/` on top of `.gitignore`. `--` is now passed before the pattern so `-v` / `--files` are treated literally.
  - `glob_files` and `list_directory`: ~1 MB cap.
  - Write/edit diff reports: ~1 MB cap.

### Changed

- **`edit_file` / `morph_edit_file` now check Read AND Write** for external paths (previously only Write). A Write-only grant that used to be enough now needs a Read grant too — the scopes hold independently.
- **`create_directory`, `move_file`, `copy_file` accept external paths** (absolute, `~/`) with the appropriate permission grants, matching `write_file` / `edit_file`. Previously these tools hard-rejected anything outside the workspace.

### Added

- **`search_code` and `glob_files` `include_ignored` parameter** (default `false`). Set `true` to bypass the built-in excludes (`target/`, `node_modules/`, `.git/`, `dist/`, `build/`) and `.gitignore` filtering.
- **`glob_files` `follow_symlinks` parameter** (default `false`). Set `true` to walk symlinks like `rg -L`.

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
