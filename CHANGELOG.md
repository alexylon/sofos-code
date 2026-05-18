# Changelog

All notable changes to Sofos are documented in this file.

## [Unreleased]

## [0.3.0] - 2026-05-18

### Added

- **`/think` was renamed to `/effort` and now opens an inline picker.** The picker lists only the reasoning levels the active model supports. Up and Down highlight a level, Enter confirms, Esc cancels. `/effort <level>` still works directly. **Breaking change**: `/think` is no longer recognised.
- **New `/model` command opens an inline picker for switching the active model.** Up and Down highlight a row, Enter confirms, Esc cancels. Each row shows the model id, a short description, and a `(current)` tag on the active one. Models on the other provider are greyed out and labelled `(re-launch session to activate)`, because the API client is built once at startup and cannot swap providers mid-session — the cursor skips those rows so you can't land on a model the session cannot reach. `/model <name>` switches directly without the picker; same-provider switches happen in place, cross-provider attempts are refused with a "re-launch with `--model <name>`" message.
- **`--model` lists the supported models when it rejects a value.** Passing `--model gpt-9.9` (or any unsupported slug) exits with `[supported models: claude-opus-4-7, claude-sonnet-4-6, claude-haiku-4-5, gpt-5.5, gpt-5.4, gpt-5.4-mini, gpt-5.3-codex]`, matching how `--reasoning-effort` already behaves.
- **An inline suggestion list appears as soon as you type `/`.** A small panel below the input shows every slash command with a short description, filtered as you type. Up and Down highlight a row, Enter runs it, Tab inserts the name so you can finish typing arguments, Esc closes the panel. The previous silent single-match Tab completion still works — it now goes through the same list.
- **New `view_image` tool lets the model open an image on demand.** Pass a local file path or `http(s)://` URL and the image is attached to the conversation. Supports JPEG, PNG, GIF, and WebP up to 20 MB per local file; URLs are forwarded to the provider, which fetches them on its side. External paths use the same Read prompt as `read_file`, so granting a directory once covers both tools. Local images larger than 2048 px on the long side are downscaled before sending, so a 4K screenshot doesn't burn through the per-image token budget. For a folder of images the model is told to call `list_directory` first, then `view_image` per file.

### Changed

- **Image paths typed inline in a prompt are no longer auto-attached.** A path or URL ending in an image extension used to be stripped from the message and attached as an image block. That misread unrelated text that happened to end in `.png`, and did nothing for vague asks like "look at the image in `assets/`". The model now uses the `view_image` tool instead. Clipboard paste (Ctrl-V) still attaches images directly.
- **Bash commands run under a supervisor with a time limit, live output caps, and interrupt support.** Output is streamed instead of buffered, the 10 MB-per-stream cap fires while reading rather than after exit, a 300-second wall-clock limit kills commands that never finish (a stuck `tail -f`, a runaway test), and ESC or Ctrl+C terminates the whole process group instead of waiting for the command to clean up on its own. The error message names the reason — cap, timeout, or interrupt — so the model can recover.
- **MCP tool names are joined to the server name with a triple underscore.** The old single underscore could collide — server `a_b` tool `c` produced the same id as server `a` tool `b_c` — letting a tool call land on the wrong server. Triple underscores can't appear in either name (registrations with them are rejected with a warning), so collisions are no longer possible. Existing configs are unaffected.

### Security

- **Shell command and process substitution are blocked in bash commands.** `$(rm bad)`, backticks, and process substitution `<(cmd)` / `>(cmd)` used to slip past the permission system because only the outer command was checked. They are now refused with a message that names the marker. Single-quoted literals and arithmetic expansion `$((expr))` still work.
- **Workspace symlinks can no longer route bash reads outside the workspace.** When a workspace-relative path resolves through a symlink to a file outside the workspace, it now goes through the same external-path prompt that absolute and `~/` paths already use.
- **MCP tools are filtered out in safe mode by default.** Safe mode used to restrict only Sofos's native tools, leaving every MCP-exposed tool fully available. Each MCP server entry now has a `safe_mode` setting — `disabled` (default, filtered), `read_only`, or `allow` — and only opted-in servers expose their tools while safe mode is on. The startup banner and `/safe` confirmation list which servers were filtered and which were opted in.

### Fixed

- **A first response cut off by the token limit no longer runs partial tool calls.** Follow-up responses inside the tool loop already handled this, but the first response was processed without checking its stop reason — a mid-tool-call cut-off left the arguments half-formed, and the call failed with a confusing parameter-missing error. The token-limit warning and early exit now fire on the initial response too.
- **Internal errors are no longer recorded as assistant output.** When a system error happened after a tool result, Sofos used to insert it as a fake assistant turn so the next request would be accepted — the model then read its own error as something it had said, which could poison the next reply. The error now lives on the trailing user turn instead, so the transcript never impersonates the model.
- **External image paths use the interactive Read prompt instead of a hard error.** Pasting an image path outside the workspace used to fail unless a matching `Read(...)` rule was already in the config; the rest of the file tools have prompted interactively for years. Image loading now shares the same session-scoped allow and deny lists as `read_file`, so granting Read access to a directory once covers both.
- **Tool descriptions match what the executor actually enforces.** `edit_file` and `morph_edit_file` now mention that editing an external file prompts for both Read and Write (the executor needs Read to load the file before applying the edit). `delete_file` and `delete_directory` now mention that absolute and `~/` paths work with a Write grant — the previous text claimed they were workspace-only, while the executor has supported external deletions for several releases.
- **`web_fetch` no longer hangs on large HTML pages.** A page of a few megabytes used to make the tool freeze or run out of memory; it now returns quickly and stops accumulating output once the response budget is filled. The maximum body size is also tightened from 64 MB to 8 MB, enough for almost every real page.
- **Session ids carry a random suffix, so two Sofos processes started in the same millisecond can no longer overwrite each other's saved history.** A helper picks an id that doesn't match any existing session file in the workspace, regenerating on the rare chance of a suffix collision.
- **Unknown slash commands no longer reach the model as plain prompts.** A typo like `/resuem` or an unsupported variant like `/effort turbo` used to be sent verbatim to the assistant, which would politely explain it didn't understand while charging input tokens. Sofos now catches any line that starts with `/` but fails to parse, prints a short error, and lists the valid commands.
- **The `read_file` transcript summary counts file content lines, not wrapper lines.** The line range used to include the two-line `File content of '...':` prefix, so a one-line file looked like three lines and a 100-line file looked like a 1–102 range. The summary now reports `Read N lines from <path>` based on the actual content; the obsolete `offset` field has been removed because `read_file` doesn't accept a range argument.
- **Pasting more than twenty images into one message no longer drops them silently.** The marker pool tops out at twenty circled-number characters; past that, the previous code inserted a plain `*` that the submission parser couldn't recover. The extra paste is now rejected with a clear "limit reached" warning and the existing images stay intact so the user can send them in batches.

### Removed

- **The README no longer claims first-class Windows support.** The terminal UI and the bash executor both depend on Unix-only behavior, so running Sofos on Windows was always best-effort. The platform line now reads "Tested on macOS, supported on Linux, experimental on Windows", and starting the TUI on a non-Unix system shows a clear configuration error instead of an opaque file-not-found.

## [0.2.12] - 2026-05-16

### Added

- **Visible task plans for multi-step work.** The assistant can now call `update_plan` to show the current plan with `pending`, `in_progress`, and `completed` statuses. Plan updates are available in safe mode too because they do not read or modify files, and the terminal renders them as a compact styled checklist while the model receives only a short acknowledgement.

### Changed

- **File-edit tool results are now fixed-size summaries.** `edit_file`, `write_file` (when it overwrites an existing file), and `morph_edit_file` previously returned the full syntax-highlighted diff to the model as the tool result. The colored diff carried truecolor ANSI escape sequences that roughly multiplied the byte count per line, and the tool result stayed in conversation history for every subsequent turn — so a session with many edits paid that bloated cost again on each later turn, and a single large rewrite could push the response into the hundreds of thousands of tokens. The model now sees a fixed two-line summary (`Success. Updated the following files:` followed by `M <path>`) regardless of edit size, while the terminal still renders the full colored diff exactly as before. If the model needs to verify the post-edit state it can re-read a range of the file.
- **Reasoning output renders markdown and is separated from what follows.** The dim `Thinking:` section that streams before the assistant's reply or before a tool call used to print as raw dim text and ran straight into the next `Using tool:` or `Assistant:` header. Prose that contained inline code, bold, or list markers showed the source characters instead of rendering them. The body now flows through the same markdown stream renderer the assistant text uses, with the rendered output wrapped in a faint terminal style so the muted look is preserved, and a blank line separates the thinking section from whatever follows it.

## [0.2.11] - 2026-05-16

### Added

- **Two extra reasoning-effort levels: `xhigh` and `max`.** They join the existing `off` / `low` / `medium` / `high` scale and are reachable from both `--reasoning-effort` and `/think`. Support is per-model: `xhigh` is accepted by Claude Opus 4.7 and every OpenAI gpt-5 reasoning model (including the codex variants); `max` is accepted by Claude Opus 4.7, Opus 4.6, and Sonnet 4.6. Sofos validates the level against the active model both at startup and at `/think`, so a mismatch (for example `/think max` on a gpt-5 model, or `/think xhigh` on Sonnet 4.6) prints a clear "not supported on this model" message instead of letting the request reach the server and come back as a 400. The status line, startup banner, and CLI help all list the six levels.

### Fixed

- **Rate-limited responses (HTTP 429) are now retried once.** Previously a 429 fell into the same "client error, fail fast" bucket as 4xx codes, so a transient burst limit aborted the call straight away. The retry now uses the server's `Retry-After` delay when present (capped so a misbehaving server can't ask for an hour-long pause), or the usual exponential backoff otherwise, and surrenders after a single retry rather than burning every retry slot on an ongoing limit.
- **Truncated tool arguments with an internal trailing comma are now recovered.** A payload that needed both the `[1,2,]` → `[1,2]` cleanup and the "close the missing `}`" repair used to fall back to the raw, un-repaired arguments because the two repairs weren't combined on the same attempt; they now apply together.
- **Streaming responses from OpenAI no longer corrupt multi-byte characters that arrive split across HTTP chunks.** Same chunk-boundary corruption as Anthropic, fixed by buffering raw bytes and decoding only at SSE line boundaries.
- **OpenAI streams that emit nested error envelopes now surface the server's message.** Previously the parser only inspected the flat `{message: "..."}` shape, so the more common `{error: {message: "..."}}` envelope landed as "Unknown streaming error" and the user lost the real reason (rate limit, context overflow, and so on). Both envelopes are now tolerated.
- **Pressing ESC during a long OpenAI streamed response now stops on the very next line instead of finishing the burst.** Sofos used to check for the interrupt only between HTTP chunks, so a final chunk that arrived with many lines of text packed in ran through to the end before noticing the keypress; the parser now re-checks between lines as well.
- **An OpenAI normal stop now carries an explicit `stop_reason`, matching Anthropic.** Earlier the field was absent on OpenAI completions, so anything downstream that read it saw "no stop reason" and treated the same outcome differently than it would have on Anthropic.
- **Duplicated tool calls from transitional or Azure-style OpenAI backends are now deduplicated before being executed.** Some backends emit the same call in both the legacy `message.tool_calls` shape and the current top-level `function_call` shape; the assistant turn used to carry both copies, and the tool would run twice on the next round-trip.
- **Failures reading or decoding the Morph response now surface a clear Morph-tagged error.** The underlying transport error used to bubble out untagged and missed the standard error-formatting that the other provider clients already used.
- **Streaming responses from Anthropic no longer corrupt multi-byte characters that arrive split across HTTP chunks.** Accents, emoji, and CJK characters used to show up as the `?` replacement glyph in the live stream when the codepoint's bytes happened to land on either side of a chunk boundary, while the aggregated response held the correct text. Streaming now buffers raw bytes and decodes at line boundaries, so the streamed view matches the final response.
- **Long Anthropic streams parse faster.** The line-by-line buffer used to copy all the leftover bytes after every event it processed, which scaled poorly on long responses; the buffer now drains in place, so a long stream finishes quicker.
- **A pathologically large token count from the Anthropic API now saturates at the 32-bit ceiling instead of wrapping to a small number.** The 32-bit ceiling sits well above any realistic single-turn count, so this only matters as a defence against a misreported wire value, but if such a value ever arrives the reported count stays believable.
- **The `anthropic-beta` header now agrees with the request body about which models support server-side compaction.** The header decision and the body decision used to consult two different lists of supported models; when the two disagreed (for example `claude-opus-4-5`), a request would carry the compaction beta header without the matching `context_management` block, which can 400 on stricter validation. Both decisions now read from the same per-model flag.
- **Ctrl+Enter now inserts a newline in the TUI input box, matching Shift+Enter and Alt+Enter.** Earlier the keystroke was silently swallowed by the textarea router, even though the placeholder text and the dispatch comments already documented it as a fallback newline binding for terminals that do not deliver Shift+Enter distinctly.
- **MCP server "initialized" lines no longer get scrolled off-screen by the TUI viewport at startup.** They used to print straight to stdout while sofos was still connecting servers, before the inline viewport had anchored, so on tight windows the viewport scrolled them out of view as it made room for itself. The lines now print through the same channel as the workspace/model header and always land above the viewport. The lines are also grouped under one `MCP servers:` heading with indented bullets, matching the visual style of the other startup labels.
- **MCP server stderr no longer floods the default log, and the lines read as plain text instead of escaped ANSI.** Many MCP servers reserve stdout for JSON-RPC and emit their own INFO/DEBUG output (often pre-coloured) to stderr; sofos used to surface every line at warning level, so a clean startup printed a wall of warnings with `\x1b[…]` escape sequences baked in. The relay is now at debug level and the ANSI styling is stripped first — real connect or list-tools failures still surface as warnings, and the raw server lines are still available via `RUST_LOG=debug`.
- **Setting `compaction_preserve_recent` to `0` no longer crashes the next compaction.** The split-point lookup used to walk past the end of the message list and panic; it now stops at the last message, so a zero-preserve configuration just compacts everything older than the very last message.
- **The summary call's token usage is now counted toward the session total.** When auto-compaction got a usable response from the model but the summary was too short to apply (fell back to plain trimming), sofos used to discard the response without billing it; the spend now lands on the session counters whether or not the summary was actually applied.
- **The auto-compaction summary call no longer fights the prompt cache.** The one-shot summary used to share the OpenAI prompt-cache shard with the regular session, so the two distinct prefixes evicted each other on every compaction. The summary call now uses a `"<session>-summary"` shard of its own.
- **Cancelling a deletion mid-batch no longer breaks the next request.** When the assistant queued several tools in one turn and the user declined one of the deletions, sofos used to skip every later tool silently and leave their `tool_use` blocks orphaned on the assistant side. Anthropic then rejected the very next request with a 400 ("tool_use without matching tool_result"), forcing a `/clear` or a fresh session. Each un-executed tool now gets a short "skipped — earlier deletion cancelled" result block so the next request stays valid.
- **Responses cut off by `max_tokens` no longer feed half-formed tool calls back through the loop.** A truncation could land mid-`tool_use`, leaving the call's JSON incomplete. The tool loop now stops on `max_tokens` and prints the existing warning instead of attempting to parse or dispatch the truncated content.
- **Server-side compaction summaries no longer trigger an "empty response" warning.** When Anthropic returns just the compaction block (no text, no tool call), the live stream still renders the summary, and the tool loop now exits quietly instead of printing "Assistant returned an empty response".
- **Image-loading retry now preserves the user's prompt text.** Previously, when the API returned a 400 because an image URL could not be downloaded, sofos discarded the user's whole message (text and all) and sent a `[SYSTEM ERROR]` placeholder instead — so the retry asked the model to respond with no idea what the user had originally said. The retry now strips only the image blocks, keeps the surrounding text intact, and tags the surviving user turn with a system note explaining what was removed.
- **Image-loading retry now responds to ESC.** The retry used a non-streaming, non-interruptible code path; pressing ESC during the retry did nothing. The retry now goes through the same streaming + interrupt machinery as the initial request, so the user can abort it the same way.
- **API and system errors no longer fabricate fake assistant messages.** When a request failed mid-turn, sofos used to append an `[API error: ...]` note as if the *assistant* itself had written it. The next request then showed the model what looked like its own prior admission of failure, sometimes triggering confabulated follow-ups. The error context is now folded into the user turn that actually triggered the error, so the model sees a system note attached to the user's own message instead.
- **`--resume` now restores the model the session was saved under** *when both belong to the same provider*. Previously the resumed session ran under whatever `--model` value was on the command line, which could mix Anthropic-only content blocks (extended thinking, server-side compaction) into an OpenAI request and produce wire-format errors. If the saved model uses a different provider than the CLI value, sofos refuses to resume and asks the user to re-launch with the matching `--model` (the underlying HTTP client is provider-bound at startup, so silently swapping the model name across providers would just create a different wire mismatch).
- **`--resume` now restores safe mode** from the saved session, so the resumed tool grant matches what the user had configured at save time.
- **`--resume` now restores the saved system prompt.** Earlier resumes silently rebuilt the prompt from the current workspace, which could disagree with what the assistant had been answering against (different tool availability, different `AGENTS.md`).
- **`-p`/`--prompt` invocations now save the session even when the turn errors out.** Previously a failed non-interactive turn left no on-disk session, so `--resume` couldn't bring the user back to whatever state was reached.
- **Session token counters saturate at their 32-bit ceiling instead of wrapping.** Very long sessions that crossed 4.29 billion tokens used to wrap to a tiny number in release builds and panic in debug. The displayed totals now stay at the ceiling instead, which keeps the cost summary honest as a lower bound.

### Security

- **Session ids passed to `--resume` are validated.** Ids containing path separators (`/`, `\`), or that are exactly `.` or `..`, are rejected with a clear error instead of being interpolated into a filesystem path. The interactive picker never produces such ids; this protects callers that pass an external string.
- **Atomic file writes now stage through a temp file with an unpredictable name.** The earlier fixed `<file>.sofos.tmp` suffix let any process that could write to the same directory plant a file (potentially a symlink to elsewhere) at that exact path between the moment sofos started a write and the moment it created the temp file, redirecting the write to an attacker-chosen target. The staged name now includes 64 bits of randomness and is created exclusively, so a pre-existing file at that path produces a hard error instead of being clobbered through.
- **Workspace path validation no longer rejects filenames that just *contain* `..` as a substring.** Names like `my..file.txt` or `cache..old/note.md` were refused by an over-eager substring check, even though they have no `..` traversal component. The check now walks real path components, so it still blocks `..` traversal but accepts legitimate names with embedded double dots.
- **The workspace-membership check no longer trusts a raw path string in fallback cases.** When sofos couldn't find any real ancestor directory to canonicalise a write path against, an earlier version compared the caller-supplied path string directly to the workspace prefix — so a workspace-relative `../../etc/passwd` was mis-classified as inside the workspace. The check now collapses `.` / `..` components first and compares the cleaned-up path, so traversal segments are caught in this fallback case too.
- **`web_fetch` now refuses to download more than 64 MB of body.** Earlier the whole response was buffered into RAM with only a 30-second timeout to bound it, so a URL serving gigabytes (or a server that misreports its size) could OOM the process before the post-fetch truncation ever ran. The fetch now checks `Content-Length` up front and aborts mid-stream if the running total crosses the cap. The 64 KB cap on the text returned to the model is unchanged.
- **HTTP MCP servers now receive the post-handshake `notifications/initialized` message** that the MCP specification mandates. Strict servers rejected every subsequent request from sofos because this confirmation never arrived; the stdio transport already sent it, only the HTTP one was missing it.
- **MCP request and response ids now accept either the numeric or the string JSON-RPC shape.** Sofos still sends numeric ids, but a server replying with a string id (which the specification permits) used to fail to deserialise and the transport dropped with a confusing parse error. Either shape is now tolerated on the wire.
- **The HTTP MCP transport now fails fast when the server is unreachable.** Earlier a TCP/TLS connect would wait the full 120-second request timeout before giving up. The new 10-second connect timeout lets the user see "server unreachable" promptly while leaving the longer ceiling in place for legitimate slow responses.
- **The global MCP config at `~/.sofos/config.toml` now loads on Windows too.** The loader read `HOME` directly, which Unix sets but Windows does not, so the file was silently skipped for every Windows user. The loader now uses `USERPROFILE` on Windows and `HOME` elsewhere.
- **Invalid MCP server entries are dropped at config load time.** Earlier they were retained and surfaced later as a generic "Invalid MCP server configuration" error on every request, with the original reason only visible in the load-time warning. Bad entries now never make it into the live server set.
- **stderr from stdio MCP servers is captured** instead of being routed to `/dev/null`. Server-side stack traces and start-up errors used to vanish, leaving an opaque "parse error" or "connection closed" as the only downstream signal. (See the matching entry under Fixed for the level the captured lines log at.)
- **Concurrent MCP stdio requests can no longer pick up each other's responses.** The transport used to lock stdin for the write and stdout for the read separately, so two overlapping requests could read out of order. The full request/response cycle now runs under a single per-client lock.
- **MCP tool listings are now served from a cache built at start-up.** Earlier `get_all_tools` re-fetched every server's tool list on every call, which meant each TUI refresh fired a network round-trip per HTTP MCP server. Tool lists are stable for the session, so the cache returns immediately.
- **MCP tool calls no longer serialise across servers.** The manager used to hold its outer mutex across the awaited tool call, so a slow call to server A blocked every concurrent call to server B. The mutex is now dropped before the await; calls to different servers run in parallel.

### Changed

- **`delete_file` and `delete_directory` now accept external paths once the user grants Write access**, matching how `write_file` and `edit_file` already behaved. Earlier they rejected every path outside the workspace, which was confusing for users who had explicitly authorised the broader directory for writes.
- **Path globs no longer let `*` cross directory boundaries.** Both the `glob_files` tool and the permission-rule compiler now compile patterns with `literal_separator(true)`, so a pattern like `*.rs` matches Rust files at the current depth only and a `Read(./logs/*)` permission rule applies only to direct children of `./logs/`. Recursive matches still work through `**` (e.g. `**/*.rs`, `Read(./logs/**)`). **Breaking change** for permission rules that relied on `*` walking subdirectories — replace `*` with `**` to keep the old reach.
- **Syntax-highlighted diffs load their assets once per process.** Each `edit_file` / `write_file` / `morph_edit_file` call used to reload the bundled syntax and theme definitions (several megabytes) before rendering the diff. The assets now live behind a one-time initialiser, so subsequent diffs reuse them.
- **`--check-connection` paired with `--prompt` now warns instead of silently dropping the prompt.** The connectivity check exits before any prompt processing runs, so combining the two flags never produced the prompt response the user expected. The warning makes the unused flag visible without changing the exit semantics.
- **Claude Opus 4.6 and Claude Sonnet 4.6 now use adaptive thinking, matching Opus 4.7.** Anthropic's docs recommend `thinking: adaptive` with `output_config.effort` for all three 1M-context models — Opus 4.7 already required it, and 4.6 still accepts the legacy `{type: "enabled", budget_tokens: N}` shape but Anthropic flags it as deprecated. Sofos now sends the same adaptive shape across all three, so the startup banner shows `Adaptive thinking effort: <level>` (the model picks its own budget) instead of the static `Extended thinking: enabled (budget: 5120 tokens)` line on the 4.6 models. `/think low|medium|high` maps to the same effort levels on every adaptive model.

### Removed

- **`--verbose` (`-v`) CLI flag.** It parsed but never had any effect on any code path. Passing it now reports the usual unknown-argument error from clap. **Breaking change** for scripts that supplied `-v` decoratively.

## [0.2.10] - 2026-05-13

### Added

- **Live streaming with formatted markdown on both providers.** Assistant responses now stream token-by-token on Anthropic and OpenAI, with headings, emphasis, lists, blockquotes, links, and fenced code blocks rendered in their final terminal styling as the text arrives — previously the reply appeared all at once on completion (Anthropic streaming was implemented but switched off; OpenAI streaming was a stub that called the non-streaming endpoint and delivered the response in a single chunk). The streamed output matches the one-shot render once the response completes. OpenAI safety refusals also stream live through the same channel as regular text.

## [0.2.9] - 2026-05-08

### Changed

- **Bash tool output is capped at 30 lines on screen** with a `... (N more lines hidden)` footer when longer. Long file dumps from `cat`, `head`, `tail`, `nl | sed -n '1,Np'`, and verbose builds no longer flood the terminal. The model still receives the full output (subject to the existing tool-output token cap), so context and follow-up reasoning are unaffected — only the on-screen view and the saved session transcript are shortened. Note: line-counting is `\n`-based, so a single multi-megabyte line (e.g. `cat binary | base64`) is still printed in full.

### Fixed

- **Session resume now shows the bash command above its output again.** The replay path was extracting the command from the wrong field (the saved output text instead of the saved input JSON), so the `Executing: <command>` header silently fell through to nothing on every replayed bash entry — users saw the output of a bash call but not which command produced it. Live execution was unaffected; only the `--resume` rendering was broken.

## [0.2.8] - 2026-05-06

### Added

- **Anthropic server-side compaction** is now enabled on Claude Opus 4.7, Opus 4.6, and Sonnet 4.6. Sofos sends the `compact-2026-01-12` beta header and a `context_management.edits[type=compact_20260112]` block on every request to those models; when the request crosses the per-model auto-compact threshold (~250K tokens), the API itself summarises older turns and returns a `compaction` content block, dropping the pre-compaction messages server-side on subsequent requests. No extra round-trip — the compaction summary arrives in the same response as the user's reply.
- **OpenAI encrypted-reasoning round-trip.** Requests that enable reasoning now include `include: ["reasoning.encrypted_content"]`. Sofos captures the opaque encrypted-CoT blob alongside the visible reasoning summary and round-trips both on the next call, so the model resumes its hidden chain-of-thought across tool calls instead of regenerating it. Cuts hidden-reasoning output tokens on multi-call agentic turns.
- **Per-model registry** consolidating context-window, auto-compact threshold, adaptive-thinking flag, server-compaction flag, and pricing (including tiered-pricing rules) into a single entry per model. Adding a new model takes one registry entry.
- **Tiered-pricing detection for GPT-5.4 and GPT-5.5.** Sofos tracks the largest single-turn input observed across the session. If any single prompt crosses the documented 272K threshold, the cost calculator switches to premium rates (2× input, 1.5× output) for the rest of the session — matching what OpenAI actually bills.
- **1-hour cache TTL on stable prefixes.** System prompt, the last-listed tool definition, and the sticky message anchor now use Anthropic's `ttl: "1h"` ephemeral cache. The rolling breakpoint stays at 5 min because it moves every turn; paying the 2× write premium for a one-turn slot would burn cache writes for nothing.
- **Middle truncation for tool outputs.** Large bash / search / file-read / diff / MCP outputs preserve both the head and the tail (separated by a `…N tokens truncated…` marker) instead of the head-only cut sofos previously applied. The diagnostic tail (last error line, ripgrep totals, exit messages) now survives truncation.
- **`compaction` content block type** added to the on-the-wire schema and the saved-session schema, so Anthropic's server-side summaries persist across save / load.
- **Honest server-side cost line.** The session summary now correctly accounts for the 1-hour cache write premium (200% of base input) on top of the existing 5-minute cache write premium (125%).

### Changed

- **CLI `-t` / `--enable-thinking` is replaced with `-e` / `--reasoning-effort <off|low|medium|high>`** (default `medium`). The previous binary on/off knob is gone — `medium` is now the default-on state because `high` materially raises hidden-reasoning token cost on routine coding work, and `off` is the absolute-cheapest path. **Breaking change**: scripts using `-t` need updating.
- **`/think on` / `/think off` are replaced with `/think <off|low|medium|high>`.** `/think` (no argument) still shows status. `on` and `off` no longer parse as commands. **Breaking change**.
- **Auto-compact threshold lowered.** Conversations now compact at ~250K tokens on 1M-window models (Opus 4.7 / 4.6, Sonnet 4.6, GPT-5.4 / 5.5), ~170K on Haiku 4.5, ~250K on the GPT-5.3-Codex 400K window. Previously sat at 800K (non-codex) / 300K (codex), which left meaningful cost on the table re-sending huge prefixes on every tool round-trip.
- **Default reasoning effort is now `medium`** (was `high`). Verified roughly 3–5× cheaper hidden-reasoning bill on routine coding turns. Use `-e high` or `/think high` for hard tasks.
- **Reasoning summaries are suppressed on the OpenAI thinking-off path.** When `effort: off`, sofos sends `reasoning.effort = "minimal"` with no `summary` field, so the model returns no summary blocks at all (they bill as output tokens).
- **Model context windows corrected.** Claude Opus 4.7 / 4.6 and Sonnet 4.6 are 1,000,000 tokens (were 200K in the table); GPT-5.4 and GPT-5.5 are 1,050,000 tokens (were 400K). The drop-trim safety floor is now per-model API-aware (95% of the real window) instead of a flat 250K.
- **Anthropic beta header is now picked per-request based on whether the target model supports server-side compaction.** `token-efficient-tools-2025-02-19` ships on every Anthropic request; `compact-2026-01-12` only ships against models that actually support it (Opus 4.7, Opus 4.6, Sonnet 4.6). Removes the implicit dependency on Anthropic's "ignore unknown beta tokens" policy — if validation ever tightens, only the right requests carry the token.
- **`/think low|medium|high` on legacy non-adaptive Anthropic models now maps to distinct `budget_tokens` values** (`Low=1024`, `Medium=5120`, `High=16384`) instead of all three collapsing to one fixed value. Affects Sonnet 4.5, Opus 4.5/4.6, Haiku 4.5; adaptive models (Opus 4.7+) and OpenAI are unchanged.
- **Startup validation now requires `--max-tokens > 16384` whenever reasoning effort is enabled**, regardless of the current model. Catches a configuration that would have silently 400'd the next request after a runtime `/model` swap to a non-adaptive Anthropic model. Default `--max-tokens 32768` already satisfies the new check.
- **Server-side compaction trigger clamped to Anthropic's documented 50K floor.** Defends against a hypothetical future small-window model entry whose `auto_compact_at` would otherwise drop below 50K and 400 the request.

### Deprecated

- **`--thinking-budget` CLI flag.** The flag has had no effect on any provider path since `/think` started picking budgets per-effort tier — legacy Anthropic uses a fixed per-tier budget, adaptive Anthropic (Opus 4.7+) uses `output_config.effort`, and OpenAI uses `reasoning.effort`. It's now hidden from `--help` and prints a one-line deprecation warning at startup when a non-default value is supplied. The flag still parses as a no-op so existing scripts don't break at parse time. Use `--reasoning-effort <off|low|medium|high>` to control thinking depth. Will be removed in a future release.

### Fixed

- **Session token counters now persist across `--resume`.** Previously every counter (`total_input_tokens`, `total_output_tokens`, `total_cache_read_tokens`, `total_cache_creation_tokens`, `peak_single_turn_input_tokens`) reset to 0 on session reload — the cost line started from zero on resume and the gpt-5.4/5.5 cliff detector forgot whether the 272K threshold had already been crossed. All five counters are now saved as part of the session JSON and restored on load. Older session files (written before this release) default every counter to 0 on load (matching prior behaviour). **Forward-compat note:** if a session file written by this release is later opened by an older sofos, the older binary silently drops the new fields on save; mixing versions against the same session file will lose the persisted counters until you settle on one version.
- **Empty OpenAI reasoning shells are dropped instead of round-tripped.** When a reasoning output item arrives with `id` but no visible summary AND no `encrypted_content`, the wire shape `{type: "reasoning", id, summary: []}` carries no signal and may be rejected by some OpenAI models. Sofos now skips the block in that exact configuration; reasoning items with either a summary or encrypted CoT are preserved unchanged.
- **Streaming Anthropic responses now round-trip server-side `compaction` content blocks.** The streaming path used to silently drop them, so on a streaming-enabled Anthropic session the next turn would re-send the pre-compaction history and Anthropic would re-compact (extra cost). The non-streaming path was already correct; this brings streaming into parity.
- **OpenAI reasoning items round-trip in the right order relative to their assistant message.** Reasoning items were being emitted in the input array *after* the message they preceded, breaking encrypted_content round-trip continuity on the server side. Now correctly placed before.
- **Tool-cache breakpoint actually lands on Anthropic when OpenAI's web-search tool is registered.** The stamper used to no-op when `OpenAIWebSearch` was the last entry in the tool list, leaving Anthropic with no tool-defs cache breakpoint at all. Now finds the last *Anthropic-compatible* tool to stamp.
- **OpenAI `Reasoning` blocks no longer leak to Anthropic on provider switch.** A session that started on OpenAI accumulates `Reasoning` content blocks; switching to Anthropic mid-session would have sent those blocks to the Messages API, which doesn't recognise the type. The Anthropic sanitiser now drops them.
- **`peak_single_turn_input_tokens` is updated for every iteration of multi-tool turns**, not just the first. Long tool chains crossing the GPT-5.5 272K cliff inside the loop now correctly switch the cost line to premium rates.
- **Stale duplicate cache breakpoint on `read_file_tool` removed.** The tool definition carried an inline `cache_control` that, combined with the request-builder's last-tool stamp, could push the request to a 5th breakpoint (Anthropic limits to 4).
- **Image files with a missing or non-UTF-8 extension** now produce a clear error that names the file path and lists the supported formats. Previously this case collapsed into a confusing `Unsupported image format: .` (empty extension between the colon and the period).
- **MCP child processes are reaped** if pipe acquisition fails partway through startup. Practically unreachable in healthy operation, but the failure path now matches the success path's cleanup guarantee — no risk of a stray child lingering after a startup error.
- **A corrupted prior-session file** now logs a warning at save time instead of silently resetting `created_at` to "now". The save itself still succeeds (so the current turn is never lost); only the `created_at` field falls back. The benign read-failure case (file doesn't exist on first save, transient permission hiccup) stays silent.
- **Failures parsing an Anthropic streaming event** now log a debug entry with a short, UTF-8-safe preview of the offending JSON. Previously these were silently dropped, so a malformed chunk left no trace.
- **Markdown link destinations now render as terminal hyperlinks** (OSC 8) on supporting terminals, and the surrounding bold / italic / heading style is correctly restored after strong, code, and link spans. Previously a `[label](url)` rendered the label without the URL, and bold inside a heading lost its weight after a `**emphasized**` span.
- **Blockquote dim styling survives strong, code, and link spans inside the quote.** Previously a `> some **bold** text` line dropped back to normal-weight after the bold span instead of staying dim until the next paragraph.

## [0.2.7] - 2026-05-04

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
- **Visible feedback on `/safe` and `/normal`** (safe-mode toggles). Now prints a one-line status (`Safe mode: enabled / read-only tools only; no writes or bash`, `Safe mode: disabled / all tools available`, or a dimmed `already enabled/disabled`).

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
