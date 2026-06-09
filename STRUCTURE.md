# Sofos Code Structure

**Status:** Canonical structural reference  
**Scope:** `src/` runtime architecture, module ownership, provider boundaries, tool execution, terminal UI, session persistence, MCP integration, and security-sensitive sandboxing rules.

---

## Table of contents

1. [Architecture overview](#1-architecture-overview)
2. [Source layout](#2-source-layout)
3. [Top-level modules](#3-top-level-modules)
   - [3.1 `main.rs`](#31-mainrs)
   - [3.2 `cli.rs`](#32-clirs)
   - [3.3 `config.rs`](#33-configrs)
   - [3.4 `error.rs`](#34-errorrs)
   - [3.5 `clipboard.rs`](#35-clipboardrs)
4. [`api/`](#4-api)
   - [4.1 `api/mod.rs`](#41-apimodrs)
   - [4.2 `api/types.rs`](#42-apitypesrs)
   - [4.3 `api/anthropic/`](#43-apianthropic)
   - [4.4 `api/openai/`](#44-apiopenai)
   - [4.5 `api/morph.rs`](#45-apimorphrs)
   - [4.6 `api/model_info.rs`](#46-apimodel_infors)
   - [4.7 `api/truncate.rs`](#47-apitruncaters)
   - [4.8 `api/utils.rs`](#48-apiutilsrs)
5. [`repl/`](#5-repl)
   - [5.1 `repl/mod.rs`](#51-replmodrs)
   - [5.2 `repl/turn.rs`](#52-replturnrs)
   - [5.3 `repl/request_builder.rs`](#53-replrequest_builderrs)
   - [5.4 `repl/response_handler.rs`](#54-replresponse_handlerrs)
   - [5.5 `repl/compaction.rs`](#55-replcompactionrs)
   - [5.6 `repl/sessions.rs`](#56-replsessionsrs)
   - [5.7 `repl/conversation/`](#57-replconversation)
   - [5.8 `repl/tui/`](#58-repltui)
6. [`session/`](#6-session)
   - [6.1 `session/state.rs`](#61-sessionstaters)
   - [6.2 `session/selector.rs`](#62-sessionselectorrs)
   - [6.3 `session/history/`](#63-sessionhistory)
7. [`tools/`](#7-tools)
   - [7.1 `tools/mod.rs`](#71-toolsmodrs)
   - [7.2 `tools/executor.rs`](#72-toolsexecutorrs)
   - [7.3 `tools/resolve.rs`](#73-toolsresolvers)
   - [7.4 `tools/filesystem.rs`](#74-toolsfilesystemrs)
   - [7.5 `tools/bash/`](#75-toolsbash)
   - [7.6 `tools/permissions/`](#76-toolspermissions)
   - [7.7 `tools/codesearch.rs`](#77-toolscodesearchrs)
   - [7.8 `tools/image.rs`](#78-toolsimagers)
   - [7.9 `tools/morph_validate.rs`](#79-toolsmorph_validaters)
   - [7.10 `tools/plan.rs`](#710-toolsplanrs)
   - [7.11 `tools/types.rs`](#711-toolstypesrs)
   - [7.12 `tools/tool_name.rs`](#712-toolstool_namers)
   - [7.13 `tools/utils.rs`](#713-toolsutilsrs)
8. [`mcp/`](#8-mcp)
   - [8.1 `mcp/config.rs`](#81-mcpconfigrs)
   - [8.2 `mcp/protocol.rs`](#82-mcpprotocolrs)
   - [8.3 `mcp/client.rs`](#83-mcpclientrs)
   - [8.4 `mcp/manager.rs`](#84-mcpmanagerrs)
   - [8.5 `mcp/transport/`](#85-mcptransport)
9. [`ui/`](#9-ui)
   - [9.1 `ui/mod.rs`](#91-uimodrs)
   - [9.2 `ui/markdown.rs`](#92-uimarkdownrs)
   - [9.3 `ui/syntax.rs`](#93-uisyntaxrs)
   - [9.4 `ui/diff.rs`](#94-uidiffrs)
   - [9.5 `ui/cost.rs`](#95-uicostrs)
   - [9.6 `ui/session_display.rs`](#96-uisession_displayrs)
10. [`commands/`](#10-commands)
11. [Request and tool-call flow](#11-request-and-tool-call-flow)
12. [Security boundaries](#12-security-boundaries)
13. [Provider boundaries](#13-provider-boundaries)
14. [Session persistence model](#14-session-persistence-model)
15. [Configuration and permission files](#15-configuration-and-permission-files)
16. [Single sources of truth](#16-single-sources-of-truth)
17. [Dependency direction](#17-dependency-direction)
18. [Extension boundaries and non-goals](#18-extension-boundaries-and-non-goals)
19. [Architectural invariants](#19-architectural-invariants)

---

## 1. Architecture overview

Sofos is a terminal-based AI coding assistant. It connects a local terminal UI, an LLM provider, a tool dispatcher, workspace-scoped filesystem access, external-path permission grants, session persistence, and optional MCP servers into one controlled agent loop.

The runtime model is:

```text
CLI entry point
   ↓
REPL state + terminal UI
   ↓
request builder + provider client
   ↓
assistant response blocks
   ↓
tool executor + permission gates + MCP manager
   ↓
tool results returned as user-message blocks
   ↓
next provider request until no tool calls remain
```

The architecture has these primary layers:

```text
main.rs / cli.rs
   ↓
repl/ orchestration
   ├── api/ provider clients and wire types
   ├── tools/ local and external tool execution
   ├── mcp/ external tool servers
   ├── session/ persisted conversations
   └── ui/ terminal rendering and display helpers
```

The core structural rules are:

1. **`repl/` owns the turn lifecycle.**
   It builds requests, streams provider responses, executes tool loops, handles interruption, saves sessions, and coordinates safe mode.

2. **`api/` owns provider translation.**
   Anthropic, OpenAI, and Morph have separate clients and wire modules. Shared in-memory message and tool types live in `api/types.rs`.

3. **`tools/` owns all local tool execution.**
   File operations, bash, code search, image loading, web fetch, Morph validation, path resolution, and permission checks are centralized under `tools/`.

4. **`mcp/` owns external tool-server integration.**
   MCP server configuration, transports, protocol messages, connection management, tool listing, and tool execution are isolated from native tool implementation.

5. **`session/` owns durable session state.**
   Runtime counters and conversation history are separated from the on-disk session JSON and index format.

6. **`ui/` owns display, not orchestration.**
   Markdown rendering, syntax highlighting, diffs, cost summaries, banners, status formatting, and session replay belong here.

7. **Sandboxing is enforced before side effects.**
   Filesystem tools resolve paths and check Read / Write grants. Bash tools pass through command-tier checks, structural checks, read-deny checks, and external Bash path grants.

8. **Provider-specific details do not leak upward.**
   The REPL talks through `LlmClient`; Anthropic and OpenAI decide how to serialize equivalent concepts such as reasoning, cache markers, web search, and streaming events.

9. **Every assistant tool use receives a matching tool result.**
   The response handler maintains the provider protocol invariant even when a tool fails or the user cancels a deletion mid-batch.

---

## 2. Source layout

```text
src/
├── main.rs
│   # Binary entry point; parses CLI, builds clients, initializes the REPL, and starts one-shot or interactive mode.
├── cli.rs
│   # Command-line argument definitions, API-key lookup, and startup option parsing.
├── config.rs
│   # Runtime defaults, model configuration, safe-mode messages, and context-budget helpers.
├── error.rs
│   # Application error taxonomy, result alias, conversions, and blocked-operation classification.
├── clipboard.rs
│   # Clipboard image import, size checks, and numbered input markers.
│
├── api/
│   ├── mod.rs
│   │   # Provider facade exposing Anthropic, OpenAI, Morph, and shared LLM client dispatch.
│   ├── types.rs
│   │   # Provider-neutral message, content block, tool, reasoning, image, request, response, and usage types.
│   ├── model_info.rs
│   │   # Per-model capability registry, context limits, compaction thresholds, effort support, and pricing.
│   ├── morph.rs
│   │   # Morph Apply API client used by the optional fast edit tool.
│   ├── truncate.rs
│   │   # Request and conversation truncation helpers for provider context limits.
│   ├── utils.rs
│   │   # Shared provider-client utilities such as UTF-8-safe truncation and HTTP helpers.
│   ├── anthropic/
│   │   ├── mod.rs
│   │   │   # Anthropic module exports and helpers for thinking, adaptive effort, and compaction support.
│   │   ├── client.rs
│   │   │   # Anthropic HTTP client, request preparation, connectivity checks, retries, and streaming entry point.
│   │   ├── stream.rs
│   │   │   # Anthropic SSE parser that converts streaming events into shared response blocks.
│   │   └── wire.rs
│   │       # Anthropic-specific request and response wire-format structures.
│   └── openai/
│       ├── mod.rs
│       │   # OpenAI module exports and provider helper functions.
│       ├── client.rs
│       │   # OpenAI HTTP client, Responses API preparation, connectivity checks, and streaming entry point.
│       ├── stream.rs
│       │   # OpenAI SSE parser that converts streaming events into shared response blocks.
│       └── wire.rs
│           # OpenAI-specific Responses API wire-format structures.
│
├── repl/
│   ├── mod.rs
│   │   # Main REPL state, initialization, safe-mode handling, status snapshots, and command-facing state changes.
│   ├── turn.rs
│   │   # Per-user-message driver: image loading, first request, streaming response, errors, counters, and save handoff.
│   ├── request_builder.rs
│   │   # Converts conversation state into provider requests, including reasoning, tools, caching, and compaction settings.
│   ├── response_handler.rs
│   │   # Iterative assistant response and tool-call loop, tool-result pairing, steering, and max-iteration recovery.
│   ├── compaction.rs
│   │   # REPL-level explicit and automatic conversation compaction orchestration.
│   ├── sessions.rs
│   │   # REPL-level session save, load, restore, and provider-compatibility checks.
│   ├── conversation/
│   │   ├── mod.rs
│   │   │   # ConversationHistory facade and module exports.
│   │   ├── messages.rs
│   │   │   # In-memory message insertion, restoration, clearing, and accessors.
│   │   ├── lifecycle.rs
│   │   │   # System prompt construction, feature list wiring, and custom instruction attachment.
│   │   ├── compaction.rs
│   │   │   # Conversation-level prefix replacement and tool-result truncation.
│   │   └── tokens.rs
│   │       # Token-budget estimates, auto-compaction thresholds, and prompt-cache anchor maintenance.
│   └── tui/
│       ├── mod.rs
│       │   # Ratatui front-end entry point and wiring around the REPL worker.
│       ├── app.rs
│       │   # TUI application state: log, input box, queue, picker, status, and overlays.
│       ├── event.rs
│       │   # UI event and worker message types, including status snapshots and exit summaries.
│       ├── event_loop.rs
│       │   # Terminal event pump, key dispatch, resize handling, and worker event processing.
│       ├── worker.rs
│       │   # Background worker thread that owns the REPL and processes queued jobs.
│       ├── ui.rs
│       │   # Ratatui rendering for the inline viewport, input box, status line, and overlays.
│       ├── input.rs
│       │   # Multi-line input state, editing behaviour, clipboard marker handling, and submission extraction.
│       ├── keymap.rs
│       │   # Keyboard bindings for editing, submission, interrupts, and picker navigation.
│       ├── output.rs
│       │   # Stdout and stderr capture so REPL output appears inside the TUI log.
│       ├── inline_terminal.rs
│       │   # Resize-safe terminal adapter for the inline viewport.
│       ├── inline_tui.rs
│       │   # Frame driver that keeps Sofos anchored at the bottom of the terminal.
│       ├── scrollback.rs
│       │   # Scrollback integration using terminal scrolling-region behaviour.
│       ├── slash_popup.rs
│       │   # State for the inline slash-command suggestion list (filter, navigation, selection).
│       └── sgr.rs
│           # Small helpers for ANSI SGR style sequences.
│
├── session/
│   ├── mod.rs
│   │   # Session module facade and public exports.
│   ├── state.rs
│   │   # Runtime session id, conversation, token counters, cache counters, and reset helpers.
│   ├── selector.rs
│   │   # Interactive session picker used by resume flows.
│   └── history/
│       ├── mod.rs
│       │   # Session persistence facade, exports, and atomic write helper.
│       ├── manager.rs
│       │   # HistoryManager, session directory layout, save/load/list orchestration, ids, and save locking.
│       ├── model.rs
│       │   # Persisted session JSON shapes, display messages, metadata, and token counter structures.
│       ├── index.rs
│       │   # Session index loading, updating, and saving.
│       ├── preview.rs
│       │   # Short session preview extraction for resume lists.
│       └── instructions.rs
│           # Loading of project AGENTS.md and personal .sofos/instructions.md files.
│
├── tools/
│   ├── mod.rs
│   │   # Native tool module facade and ToolExecutor / ToolName exports.
│   ├── executor.rs
│   │   # Central native and MCP tool dispatcher, permission checks, tool routing, web fetch, and output caps.
│   ├── resolve.rs
│   │   # Path resolution, tilde handling, canonicalization, write-target resolution, and workspace classification.
│   ├── filesystem.rs
│   │   # Low-level file and directory operations, atomic writes, append, edit, move, copy, and delete helpers.
│   ├── codesearch.rs
│   │   # Ripgrep-backed code search with ignore policy, file-type filters, and output limits.
│   ├── image.rs
│   │   # Image loader used by the `view_image` tool: format detection, 20 MB size cap, automatic resize to 2048 pixels on the long side, base64 encoding, and Read-permission integration.
│   ├── morph_validate.rs
│   │   # Safety checks that reject suspicious or truncated Morph Apply output before writing files.
│   ├── plan.rs
│   │   # `update_plan` argument validation, model-facing acknowledgements, and terminal checklist rendering.
│   ├── tool_name.rs
│   │   # Type-safe native tool-name enum and string conversion logic.
│   ├── types.rs
│   │   # Provider-facing native tool schemas and safe-mode / Morph-enabled tool lists.
│   ├── utils.rs
│   │   # Tool confirmations, HTML-to-text conversion, path predicates, and truncation helpers.
│   ├── test_support.rs
│   │   # Shared test-only helpers for tool tests.
│   ├── tests.rs
│   │   # Tool integration and security tests.
│   ├── bash/
│   │   ├── mod.rs
│   │   │   # Bash tool module facade and exports.
│   │   ├── executor.rs
│   │   │   # Sandboxed bash command execution, permission integration, process spawning, and capture limits.
│   │   ├── validate.rs
│   │   │   # Bash structural validation, git restrictions, external path grants, read-deny checks, and rejection wording.
│   │   └── output.rs
│   │       # Bash output display formatting, line caps, and model-facing result preparation.
│   └── permissions/
│       ├── mod.rs
│       │   # Permission module facade and shared permission enums.
│       ├── manager.rs
│       │   # PermissionManager, built-in command tiers, config loading, prompts, glob sets, and tilde expansion.
│       ├── settings.rs
│       │   # TOML-backed permission and MCP configuration structures.
│       ├── pattern.rs
│       │   # Permission rule parsing, scope extraction, wildcard handling, and blanket Bash rules.
│       ├── scope.rs
│       │   # Read, Write, and Bash path scope matching helpers.
│       └── command_parse.rs
│           # Shell tokenisation and compound-command analysis used by bash permission checks.
│
├── mcp/
│   ├── mod.rs
│   │   # MCP module facade and McpManager export.
│   ├── config.rs
│   │   # MCP server configuration loading from local and global config files.
│   ├── protocol.rs
│   │   # MCP and JSON-RPC request, response, tool, content, and id wire types.
│   ├── client.rs
│   │   # MCP client handshake, initialized notification, tool listing, tool calls, timeouts, and response parsing.
│   ├── manager.rs
│   │   # MCP server set, startup orchestration, tool cache, prefixed tool names, and execution routing.
│   └── transport/
│       ├── mod.rs
│       │   # MCP transport module facade and shared transport exports.
│       ├── stdio.rs
│       │   # Child-process stdio MCP transport, lifecycle handling, synchronization, and stderr capture.
│       └── http.rs
│           # Streamable HTTP MCP transport with connect timeout and request timeout handling.
│
├── ui/
│   ├── mod.rs
│   │   # UI facade for banners, errors, warnings, tool output, assistant text, streaming, and cursor style.
│   ├── markdown.rs
│   │   # Markdown renderer for block and streaming assistant output.
│   ├── syntax.rs
│   │   # Syntax-highlighting asset loading and code rendering helpers.
│   ├── diff.rs
│   │   # Compact syntax-highlighted diff rendering with context and line numbers.
│   ├── cost.rs
│   │   # Token usage, cache accounting, pricing, tier detection, and session cost summaries.
│   └── session_display.rs
│       # Replay formatting for saved sessions in the terminal UI.
│
└── commands/
    ├── mod.rs
    │   # Slash-command module facade and exports.
    └── builtin.rs
        # Built-in slash-command parsing and dispatch hooks for REPL state changes.
```

Each directory is a responsibility boundary. File names reflect ownership: request construction is not mixed with provider transport, provider transport is not mixed with terminal UI, and permission checks are not duplicated inside individual tools beyond the dispatcher routes that select the correct scope.

---

## 3. Top-level modules

### 3.1 `main.rs`

`main.rs` is the binary entry point.

It owns:

- tracing initialization;
- CLI parsing;
- reasoning-effort validation against the selected model;
- LLM client construction;
- API connectivity checks;
- workspace discovery;
- startup banner assembly;
- Morph client initialization;
- REPL construction;
- optional session resume before entering interactive mode;
- one-shot prompt mode.

It does not own:

- provider wire serialization;
- tool execution;
- path permission policy;
- session JSON layout;
- terminal event handling;
- command implementations after the REPL starts.

Rules:

- Provider choice is derived from the model name at startup.
- Startup validation rejects unsupported reasoning-effort / model pairs before the first API request.
- Interactive mode hands the startup banner to the TUI so the inline viewport cannot overwrite it.
- One-shot prompt mode prints directly and exits after saving and summarising the session.

### 3.2 `cli.rs`

`cli.rs` owns command-line argument parsing.

It contains:

- the `Cli` shape consumed by `main.rs`;
- API-key options and environment-variable fallbacks;
- model and token options;
- reasoning-effort CLI input;
- safe-mode, resume, prompt, and connectivity flags;
- deprecated option compatibility where applicable.

It does not validate provider wire compatibility beyond what can be expressed as CLI shape. Model-specific policy is checked by `main.rs` and `api/model_info.rs`.

### 3.3 `config.rs`

`config.rs` owns runtime configuration defaults and small state types used by the REPL.

It contains:

- model configuration values passed into request building;
- safe-mode and normal-mode system messages;
- context and auto-compaction thresholds derived from model information;
- global defaults for the response-handler loop.

It does not load permission files. Permission configuration belongs to `tools/permissions/`.

### 3.4 `error.rs`

`error.rs` owns the application error taxonomy.

It contains:

- the `SofosError` enum;
- the crate-level `Result<T>` alias;
- conversions from I/O, HTTP, JSON, and related failures;
- user-facing error categories used by UI formatting.

Rules:

- Tool and API errors should preserve enough context to be actionable.
- Blocked operations should be distinguishable from ordinary failures so the UI can render them differently.
- User-facing errors should include a safe next step when possible.

### 3.5 `clipboard.rs`

`clipboard.rs` owns clipboard image import.

It contains:

- platform clipboard access;
- clipboard image extraction;
- image size enforcement;
- numbered marker handling used by the TUI input flow.

It does not own image loading from disk. Local and remote image loading for the `view_image` tool lives in `tools/image.rs`.

---

## 4. `api/`

`api/` owns provider clients, provider-neutral request / response types, model capability data, truncation helpers, and the optional Morph Apply client.

`api/` does not execute tools, read workspace files, or render terminal UI.

### 4.1 `api/mod.rs`

`api/mod.rs` is the provider façade.

It contains:

- module exports;
- public re-exports for clients and shared types;
- the `LlmClient` enum over `AnthropicClient` and `OpenAIClient`;
- provider-neutral methods for non-streaming requests, streaming requests, connectivity checks, and provider labels.

Rules:

- REPL code should call `LlmClient`, not provider-specific clients directly, except where it must ask provider-specific capability helpers.
- Provider-specific request preparation stays inside the relevant provider module.

### 4.2 `api/types.rs`

`api/types.rs` owns the provider-neutral in-memory representation of messages, content blocks, tools, cache controls, reasoning controls, usage, and responses.

It contains:

- `CreateMessageRequest` and `CreateMessageResponse`;
- `Message` and message-content variants;
- assistant content blocks, including text, tool use, thinking, reasoning, compaction, server tool use, and web-search result blocks;
- tool definitions;
- image source types;
- token-usage accounting types;
- reasoning-effort and provider configuration shapes.

Rules:

- This module is the common contract between `repl/`, `api/anthropic/`, `api/openai/`, `tools/`, and `session/`.
- Provider modules convert from these types to wire format and back.
- Session persistence stores these shapes or display projections, so changes here can affect backwards compatibility.

### 4.3 `api/anthropic/`

`api/anthropic/` owns Anthropic Messages API integration.

It contains:

- `client.rs` — HTTP request execution, retries where applicable, request preparation, beta-header selection, connectivity checks, and streaming entry points;
- `wire.rs` — Anthropic-specific request and response wire structures;
- `stream.rs` — Anthropic SSE parsing and conversion into shared response content;
- `mod.rs` — module exports and provider helper functions such as thinking-budget and adaptive-thinking support.

Rules:

- Anthropic-only concepts such as prompt-cache block markers, `thinking`, adaptive thinking, server-side compaction, Anthropic web search, and Anthropic beta headers are handled here or in `repl/request_builder.rs` immediately before request construction.
- Anthropic streaming must produce the same final shared `CreateMessageResponse` shape as non-streaming.

### 4.4 `api/openai/`

`api/openai/` owns OpenAI Responses API integration.

It contains:

- `client.rs` — HTTP request execution, OpenAI request preparation, prompt-cache key usage, connectivity checks, and streaming entry points;
- `wire.rs` — OpenAI-specific request and response wire structures;
- `stream.rs` — OpenAI SSE parsing and conversion into shared response content;
- `mod.rs` — module exports and provider helpers.

Rules:

- OpenAI-only concepts such as Responses API items, reasoning summaries, encrypted reasoning content, `prompt_cache_key`, OpenAI web search, and OpenAI streaming event shapes are handled here.
- OpenAI reasoning blocks must not be sent to Anthropic after a provider switch or resume boundary.

### 4.5 `api/morph.rs`

`api/morph.rs` owns the optional Morph Apply client.

It contains:

- Morph client construction;
- Morph Apply request / response handling;
- Morph model selection;
- Morph transport error mapping.

Rules:

- Morph is not a general LLM provider for conversations. It is used only by the `morph_edit_file` tool path.
- Morph failures should not corrupt files. The tool dispatcher validates Morph output before writing it.

### 4.6 `api/model_info.rs`

`api/model_info.rs` owns per-model capability and pricing metadata.

It contains:

- the `SUPPORTED_MODELS` whitelist — every model id accepted by `--model` and shown in the `/model` picker, with its description and provider;
- version-free model-id constants (`CLAUDE_OPUS`, `GPT_FLAGSHIP`, and so on) that every model id in the codebase refers to, so renaming a model on the wire is a one-line change to the constant's value;
- helpers `canonical_model`, `model_support_error`, and `supported_models_label` that share one source of truth with the CLI rejection message and the picker rows;
- model registry entries;
- context-window sizes;
- auto-compaction thresholds;
- supported reasoning-effort levels;
- adaptive-thinking support;
- server-side compaction support;
- token pricing and tiered-pricing rules.

Rules:

- Model capability checks should use this registry instead of hard-coded scattered checks.
- Adding a supported model is one struct literal in `SUPPORTED_MODELS` — the `Model` struct carries the user-facing description and provider alongside the context window, effort matrix, and pricing, so there is no separate `lookup` table to keep in sync.
- Removing a model is one deletion in `SUPPORTED_MODELS`. The CLI and the picker share that array as their source of truth, so nothing else has to be touched.

### 4.7 `api/truncate.rs`

`api/truncate.rs` owns provider-facing truncation and compaction support that applies to conversation content before requests are sent.

It contains helpers for keeping request payloads below provider limits without changing tool execution semantics.

It does not own tool-output caps at the point where tools return data. Tool-output caps live in `tools/utils.rs` and `tools/executor.rs`.

### 4.8 `api/utils.rs`

`api/utils.rs` owns provider-client utility functions.

It contains:

- UTF-8-safe truncation helpers;
- shared HTTP / API utility behaviour;
- small helpers used by provider clients and request handling.

It should stay provider-neutral. Provider-specific interpretation belongs in `api/anthropic/` or `api/openai/`.

---

## 5. `repl/`

`repl/` owns the conversation runtime. It is the central coordinator between the provider client, conversation history, tool executor, session manager, image loader, and terminal UI.

`repl/` does not implement provider wire protocols, local filesystem operations, permission matching, or syntax highlighting internals.

### 5.1 `repl/mod.rs`

`repl/mod.rs` owns the `Repl` state object and high-level state transitions.

It contains:

- `ReplConfig`;
- `Repl` and its owned runtime dependencies;
- REPL construction;
- MCP manager initialization;
- tool executor initialization;
- custom instruction loading;
- safe-mode setup;
- available-tool refresh;
- one-shot prompt execution;
- status-line snapshots;
- `/effort`, `/safe`, `/normal`, and `/clear` state handlers;
- shared interrupt and mid-turn steering buffers.

Rules:

- `Repl` owns one Tokio runtime for its lifetime.
- Available tools are refreshed when safe mode changes.
- Safe mode changes update both the tool executor and the conversation context.
- The TUI is the interactive front end; `Repl::run` delegates to `repl/tui`.

### 5.2 `repl/turn.rs`

`repl/turn.rs` owns per-message processing.

It coordinates:

- adding the user turn to conversation history;
- image detection and loading;
- building the initial provider request;
- streaming the first response;
- invoking the response handler for tool-loop continuation;
- error recovery paths that keep the conversation protocol valid;
- token counter updates and session-state updates.

Rules:

- A failed API call must not erase the user turn.
- Image-loading retries should preserve the user's text and remove only failing image blocks.
- Interruptions should be represented in conversation history without creating provider-invalid role sequences.

### 5.3 `repl/request_builder.rs`

`repl/request_builder.rs` owns conversion from current REPL state to a provider-neutral request.

It contains:

- `RequestBuilder`;
- reasoning configuration selection;
- Anthropic adaptive-thinking and legacy-thinking setup;
- OpenAI reasoning setup;
- Anthropic server-side compaction request setup;
- system-prompt attachment;
- tool list attachment;
- OpenAI `prompt_cache_key` assignment;
- Anthropic prompt-cache breakpoint stamping.

Rules:

- Request construction reads model capabilities from `api/model_info.rs`.
- Anthropic cache markers are stamped only for Anthropic requests.
- OpenAI reasoning config is included only for OpenAI requests.
- Cache anchor and rolling cache-breakpoint logic must remain consistent with `repl/conversation/` token management.

### 5.4 `repl/response_handler.rs`

`repl/response_handler.rs` owns assistant response processing and iterative tool execution.

It contains:

- `ResponseHandler`;
- assistant content-block classification;
- assistant message insertion into conversation history;
- tool-use extraction;
- native and MCP tool execution through `ToolExecutor`;
- tool-result block construction;
- MCP image result forwarding;
- user-cancelled deletion handling;
- mid-turn steering message delivery;
- follow-up request generation;
- max-tool-iteration protection;
- OpenAI reasoning-only continuation;
- max-token truncation stop handling.

Rules:

- Tool loops are iterative, not recursive.
- The maximum tool-iteration limit prevents infinite loops.
- Every tool-use block must be followed by a matching tool-result block before the next provider request.
- If a deletion is cancelled mid-batch, skipped tools still receive synthetic tool results.
- A response cut off by `max_tokens` must not feed half-formed tool calls back into execution.

### 5.5 `repl/compaction.rs`

`repl/compaction.rs` owns explicit and automatic conversation compaction orchestration at the REPL level.

It coordinates:

- local truncation of large tool results;
- summary generation when needed;
- interruption handling during compaction;
- conversation replacement with a preserved summary plus recent context.

Provider-specific server-side compaction support is configured by `repl/request_builder.rs`; durable conversation edits are applied by `repl/conversation/`.

### 5.6 `repl/sessions.rs`

`repl/sessions.rs` owns REPL-level session save and load behaviour.

It coordinates:

- saving the current session through `HistoryManager`;
- loading a selected session by id;
- restoring message history;
- restoring model and safe-mode state where valid;
- rejecting incompatible provider resumes.

Rules:

- A resumed session must not silently cross provider boundaries if its saved message shapes are provider-specific.
- Safe-mode state should resume with the session so the tool grant matches the saved context.

### 5.7 `repl/conversation/`

`repl/conversation/` owns in-memory conversation history.

It contains:

- `mod.rs` — module façade and `ConversationHistory` export;
- `messages.rs` — adding, restoring, clearing, and exposing messages;
- `lifecycle.rs` — system-prompt construction, feature wiring, and custom-instruction attachment;
- `compaction.rs` — local conversation replacement and tool-result truncation;
- `tokens.rs` — token-budget tracking and cache-anchor maintenance.

Rules:

- The system prompt is assembled here, not inside provider clients.
- Conversation trimming must preserve provider protocol validity.
- Cache-anchor state belongs with the conversation because it depends on message history shape.
- Tool-result truncation changes model-visible context, not session display history.

### 5.8 `repl/tui/`

`repl/tui/` owns the interactive terminal front end.

It contains:

- `mod.rs` — TUI entry point and wiring;
- `app.rs` — UI state such as log, input, queue, picker, status, and overlays;
- `event.rs` — worker-to-UI event types and status snapshots;
- `event_loop.rs` — terminal event pump;
- `worker.rs` — background thread that owns the `Repl`;
- `ui.rs` — Ratatui rendering;
- `input.rs` — input box state and editing operations;
- `keymap.rs` — keyboard mappings;
- `output.rs` — stdout and stderr capture;
- `inline_terminal.rs` — resize-safe custom terminal integration;
- `inline_tui.rs` — inline viewport frame driver;
- `scrollback.rs` — terminal scrollback integration;
- `slash_popup.rs` — state for the inline slash-command suggestion list;
- `sgr.rs` — SGR escape helpers.

The TUI also carries two modal pickers as fields on `app::App` and corresponding job/event variants:

- the resume picker (`Picker` + `UiEvent::ShowResumePicker` + `Job::ResumeSelected`) drives `/resume`;
- the model picker (`ModelPicker` + `UiEvent::ShowModelPicker` + `Job::ModelSelected`) drives `/model`. Rows on the other provider are flagged unavailable on the `ModelPickerEntry`, the renderer greys them out, and the navigation helper in `app.rs` skips past them so the cursor only lands on a model the running session can switch to.

Rules:

- The TUI owns terminal interaction and event routing; it does not decide provider request structure.
- The worker owns the `Repl`; the UI communicates via events, queues, interrupt flags, and steering buffers.
- Terminal scrollback should remain usable outside the inline viewport.

---

## 6. `session/`

`session/` owns runtime session state and durable session persistence.

`session/` does not execute tools or build provider requests.

### 6.1 `session/state.rs`

`session/state.rs` owns runtime counters and current conversation state.

It contains:

- current session id;
- current `ConversationHistory`;
- token counters;
- cache-read and cache-creation counters;
- peak single-turn input counter used by tiered pricing;
- state-reset helpers.

Rules:

- Runtime counters must survive save / load when persisted.
- Session state should be updated by REPL orchestration, not provider clients.

### 6.2 `session/selector.rs`

`session/selector.rs` owns the session selection UI used by resume flows.

It contains the terminal picker for saved sessions and returns the selected session id to the REPL.

It does not load or parse session JSON. That belongs to `session/history/`.

### 6.3 `session/history/`

`session/history/` owns the on-disk session format.

It contains:

- `mod.rs` — module documentation, exports, and atomic write helper;
- `manager.rs` — `HistoryManager`, directory layout, save / load / list orchestration, session id generation, save-lock handling;
- `model.rs` — persisted session shapes, display messages, metadata, and token counters;
- `index.rs` — session index load / update / save;
- `preview.rs` — session preview generation;
- `instructions.rs` — project and personal instruction discovery.

On-disk locations:

```text
.sofos/sessions/<session_id>.json
.sofos/sessions/index.json
.sofos/sessions/.save.lock
```

Rules:

- Session ids must not contain path separators or traversal names.
- Concurrent saves must serialize index updates.
- Older session files should load with safe defaults when fields are missing.
- Display history and provider API history are separate persisted concepts.
- Instruction loading reads `AGENTS.md` and `.sofos/instructions.md`.

---

## 7. `tools/`

`tools/` owns native tool definitions and execution. It is the main security boundary for local side effects.

`tools/` does not build provider requests or render the main TUI. It returns structured text and image results to the response handler.

### 7.1 `tools/mod.rs`

`tools/mod.rs` is the module façade.

It exports:

- native tool modules;
- `ToolExecutor`;
- `ToolName`;
- test-only support modules.

It does not contain dispatch logic. Dispatch lives in `tools/executor.rs`.

### 7.2 `tools/executor.rs`

`tools/executor.rs` owns native tool dispatch and MCP tool dispatch integration.

It contains:

- `ToolExecutor`;
- `ToolExecutionResult`;
- available-tool list selection;
- safe-mode tool filtering;
- MCP tool detection and execution;
- Read and Write external-path permission checks;
- session-scoped path grants and denials;
- file operation routing;
- bash tool invocation;
- code search invocation;
- web fetch implementation;
- Morph edit execution and fallback messages;
- MCP output and image caps;
- model-facing tool-result truncation.

Rules:

- Every filesystem-touching tool resolves paths through `tools/resolve.rs`.
- External files require the correct scope: Read, Write, or both.
- `edit_file` and `morph_edit_file` require both Read and Write for external paths.
- `copy_file` requires Read on external source and Write on external destination.
- `move_file` requires Write on any external source or destination.
- Delete operations require explicit confirmation even after permission checks.
- Web fetch accepts only `http://` and `https://`, caps raw body size, strips HTML, and truncates model-visible text.

### 7.3 `tools/resolve.rs`

`tools/resolve.rs` owns path resolution and workspace classification for native tools.

It contains:

- `ResolvedPath`;
- existing-path resolution for reads;
- write-target resolution for paths that may not exist yet;
- tilde and absolute path handling;
- lexical normalization fallback when no existing ancestor can be canonicalized;
- inside-workspace classification.

Rules:

- Existing paths are canonicalized through the filesystem.
- Write targets canonicalize the nearest existing ancestor and re-append the missing tail.
- A path must not be classified as inside the workspace purely because the raw string looked relative.
- Parent traversal fallback classification must remain conservative.

### 7.4 `tools/filesystem.rs`

`tools/filesystem.rs` owns low-level filesystem operations.

It contains:

- file read and write primitives;
- append support;
- directory listing;
- directory creation;
- targeted edit support;
- copy and move helpers;
- delete helpers;
- atomic write behaviour;
- file size limits;
- workspace-root storage.

Rules:

- The dispatcher decides whether a path is inside or outside the workspace and whether permission grants are satisfied.
- Low-level operations should not silently truncate content used for editing.
- Writes should be atomic where possible.
- Destructive operations remain explicit and auditable.

### 7.5 `tools/bash/`

`tools/bash/` owns sandboxed bash execution.

It contains:

- `mod.rs` — module façade and exports;
- `executor.rs` — command execution, permission-manager integration, session-scoped Bash path grants, process spawning, and capture limits;
- `validate.rs` — structural command checks, external Bash path checks, read-deny enforcement, git-operation restrictions, and rejection messages;
- `output.rs` — output formatting, display caps, and model-facing output preparation.

Rules:

- Bash commands pass through the 3-tier permission system: Allowed, Denied, or Ask.
- Structural checks still run even when a command is otherwise allowed.
- Parent-directory traversal as a path component is blocked.
- Output redirection to files is blocked; `2>&1` is allowed.
- Here-documents are blocked.
- Shell command substitution and process substitution (`$(...)`, backticks, `<(...)`, `>(...)`) are blocked because they hide subcommands from the permission system. Single-quoted literals and arithmetic expansion `$((expr))` remain allowed.
- Dangerous git operations are blocked or prompted according to policy.
- Commands referencing external paths require Bash-path grants. Workspace-relative path arguments are also canonicalised, so a symlink inside the workspace cannot route a read through an external file without the same prompt.
- Read deny rules apply to command path arguments.
- Each command runs under a supervisor that streams output, enforces per-stream byte caps while reading, applies a wall-clock timeout, and terminates the whole process group on user interrupt (ESC / Ctrl+C).

### 7.6 `tools/permissions/`

`tools/permissions/` owns the permission system.

It contains:

- `mod.rs` — permission module exports and shared enums;
- `manager.rs` — `PermissionManager`, built-in command tiers, config loading, permission prompts, glob compilation, and tilde expansion;
- `settings.rs` — TOML settings shapes;
- `pattern.rs` — permission rule parsing, including blanket Bash rules;
- `scope.rs` — Read / Write / Bash path scope matching;
- `command_parse.rs` — command tokenization and compound command analysis.

Permission files:

```text
.sofos/config.local.toml
~/.sofos/config.toml
```

Rules:

- Deny rules win over allow rules.
- Read, Write, and Bash path scopes are independent.
- `*` must not cross directory separators; recursive matches use `**`.
- `Read(path/**)` and equivalent scope rules should also cover the base directory.
- Command allow / deny rules can be exact or wildcard by base command.
- Unknown bash commands prompt the user when interactive.

### 7.7 `tools/codesearch.rs`

`tools/codesearch.rs` owns ripgrep-based code search.

It contains:

- ripgrep availability detection;
- search command construction;
- optional file-type filtering;
- ignored-directory policy;
- result and file-size caps;
- formatted result output.

Rules:

- Search skips heavy build / vendor directories by default.
- `include_ignored` is an explicit opt-in to broader search.
- Search output is capped before it enters model context.

### 7.8 `tools/image.rs`

`tools/image.rs` owns the image loader behind the `view_image` tool.

It contains:

- decode, applying any orientation the photo's metadata records, plus optional resize (long side fits within 2048 pixels) before the bytes reach the model;
- byte-level format detection: PNG, JPEG, GIF, and WebP pass through unchanged when small enough; other decodable formats (e.g. BMP) are re-encoded as PNG;
- base64 encoding and media-type assignment;
- the 20 MB per-file size cap on the raw bytes;
- canonical workspace resolution so inside/outside classification compares the same path shape on both sides;
- integration with the shared Read-permission grant set, so a single "Allow Read access to /foo?" decision answered for `read_file` also covers `view_image`;
- a URL passthrough that hands `http(s)://` inputs to the model provider unchanged.

Rules:

- Local files outside the workspace go through the same interactive Read prompt as `read_file`.
- Files that fail to decode or exceed the size cap produce errors that name the cause.
- The loader never fetches remote URLs itself; the model provider does that on its side.

### 7.9 `tools/morph_validate.rs`

`tools/morph_validate.rs` owns safety checks for Morph Apply output.

It contains validation that rejects suspicious or truncated merged code before any file write occurs.

Rules:

- Morph output must be validated against the original file.
- Rejected Morph output leaves the original file untouched and asks the model to use `edit_file`.

### 7.10 `tools/plan.rs`

`tools/plan.rs` owns the `update_plan` payload parsing and the on-screen checklist rendering.

It contains:

- the `PlanStepStatus`, `PlanStep`, and `PlanUpdate` types;
- payload validation, including the at-most-one `in_progress` step rule;
- the compact model-facing acknowledgement string;
- the styled terminal checklist with status markers and counts.

Rules:

- The model receives only a short acknowledgement, never the full plan body, so conversation history stays bounded.
- The renderer must reuse `ui::ACCENT_RGB` so the plan checklist matches the rest of sofos' colour scheme.
- The module performs no file or network access and is safe to expose in read-only mode.

### 7.11 `tools/types.rs`

`tools/types.rs` owns native tool definitions exposed to providers.

It contains:

- tool schemas;
- read-only tool lists;
- full tool lists;
- Morph-enabled tool lists;
- code-search tool insertion;
- provider-facing descriptions for local tools.

Rules:

- Safe mode must expose only read-only native tools.
- Tool definitions should match dispatcher parameter names and accepted aliases where possible.
- Provider-specific server-side tools should be represented in shared `api/types.rs` but selected here where appropriate.

### 7.12 `tools/tool_name.rs`

`tools/tool_name.rs` owns the type-safe native tool-name enum.

It contains:

- native tool variants;
- string conversions;
- parsing from provider-supplied tool names.

Rules:

- Native dispatch should match on `ToolName`, not raw strings.
- Adding a tool requires updating this enum, schemas, dispatch, tests, and user documentation where applicable.

### 7.13 `tools/utils.rs`

`tools/utils.rs` owns shared tool utilities.

It contains:

- confirmation prompts;
- HTML-to-text conversion;
- truncation constants and helpers;
- path helper predicates;
- output caps for file reads, path listings, diffs, MCP text, and MCP images.

Rules:

- Model-facing tool output caps should be centralized here.
- Human confirmation helpers should be reused instead of implemented ad hoc.

---

## 8. `mcp/`

`mcp/` owns Model Context Protocol integration. It discovers external MCP servers, connects to them, caches their tools, and routes MCP tool calls.

`mcp/` does not implement native Sofos tools or enforce native safe-mode filtering. MCP server trust is configured by the user.

### 8.1 `mcp/config.rs`

`mcp/config.rs` owns MCP server configuration loading.

It contains:

- local and global config discovery;
- stdio server configuration;
- HTTP server configuration;
- environment variable mapping;
- invalid-entry filtering and diagnostics.

Rules:

- Local config overrides or augments global config according to the config model.
- Invalid server entries should be dropped early with actionable diagnostics.

### 8.2 `mcp/protocol.rs`

`mcp/protocol.rs` owns JSON-RPC and MCP protocol types.

It contains:

- request and response shapes;
- initialize and initialized notification shapes;
- tool-listing shapes;
- tool-call shapes;
- content payload types;
- numeric and string id compatibility where needed.

Rules:

- Protocol types should mirror MCP wire semantics and avoid leaking transport concerns.

### 8.3 `mcp/client.rs`

`mcp/client.rs` owns MCP client implementations over the available transports.

It coordinates:

- initialization handshake;
- initialized notification;
- tool listing;
- tool execution;
- request timeouts;
- response parsing;
- server stderr handling where applicable.

### 8.4 `mcp/manager.rs`

`mcp/manager.rs` owns the set of configured MCP servers.

It contains:

- server startup and connection orchestration;
- startup status lines;
- tool-name prefixing;
- tool cache construction;
- MCP tool lookup;
- MCP tool execution routing;
- image attachment conversion for the tool executor.

Rules:

- MCP tools are prefixed with their server name using a triple underscore separator. Server and tool names that contain the separator are rejected at registration, so the prefixed name unambiguously identifies the originating server.
- Tool registrations whose prefixed name collides with an earlier registration are skipped with a warning instead of overwriting.
- Each MCP server has a safe-mode policy (`disabled`, `read_only`, or `allow`). When safe mode is on, only tools from servers whose policy is `read_only` or `allow` are exposed; everything else is filtered out so a configured MCP server cannot quietly mutate state in a safe-mode session.
- Tool listings are cached for the session.
- Calls to different servers should not serialize unnecessarily.

### 8.5 `mcp/transport/`

`mcp/transport/` owns wire transports.

It contains:

- `stdio.rs` — child-process stdio transport, process lifecycle, request / response synchronization, and stderr capture;
- `http.rs` — streamable HTTP transport, connect timeout, request timeout, and HTTP-specific message exchange;
- `mod.rs` — shared transport exports.

Rules:

- Transport modules should know how bytes move, not what tools mean.
- Client and manager code own protocol-level sequencing and tool semantics.

---

## 9. `ui/`

`ui/` owns terminal-facing rendering helpers and display formatting outside the Ratatui event loop.

`ui/` does not build LLM requests or execute tools.

### 9.1 `ui/mod.rs`

`ui/mod.rs` owns the `UI` façade and shared display functions.

It contains:

- banner rendering;
- error, warning, and blocked-operation formatting;
- assistant text printing;
- tool header and tool-output formatting;
- stream-printer integration;
- cursor-style helpers.

Rules:

- User-facing output should go through this layer where practical so wording and colour semantics stay consistent.

### 9.2 `ui/markdown.rs`

`ui/markdown.rs` owns Markdown rendering.

It contains:

- block and streaming Markdown formatting;
- heading, emphasis, code, blockquote, list, and link handling;
- terminal hyperlink support where available;
- style restoration across nested spans.

Rules:

- Streaming and final rendering should agree visually.
- Markdown rendering must remain terminal-safe and avoid corrupting input state.

### 9.3 `ui/syntax.rs`

`ui/syntax.rs` owns syntax-highlighting support.

It contains:

- syntax-set and theme loading;
- language selection;
- highlighted code rendering helpers.

Rules:

- Highlighting assets should be reused rather than reloaded for every diff or code block.

### 9.4 `ui/diff.rs`

`ui/diff.rs` owns visual diff generation.

It contains:

- compact contextual diffs;
- line-number rendering;
- added / removed / unchanged line styling;
- syntax-coloured diff content.

Rules:

- File-editing tools return diffs generated here.
- Diff output is display-oriented and capped before model insertion.

### 9.5 `ui/cost.rs`

`ui/cost.rs` owns token usage and cost calculation display.

It contains:

- provider pricing application;
- cache-read and cache-write accounting;
- tiered-pricing detection display;
- session summary rendering.

Rules:

- Cost display reads model pricing from `api/model_info.rs`.
- Pricing calculations must account for provider cache discounts and premiums.

### 9.6 `ui/session_display.rs`

`ui/session_display.rs` owns replay formatting for saved sessions.

It contains display logic for persisted `DisplayMessage` values so resumed sessions can show previous user, assistant, and tool activity consistently.

---

## 10. `commands/`

`commands/` owns slash-command parsing and command routing.

It contains:

- `mod.rs` — `Command` enum, slash-command parsing, and dispatch into per-command hooks;
- `builtin.rs` — built-in command execution hooks that update REPL state.

Built-in commands include:

- `/resume`;
- `/clear`;
- `/compact`;
- `/effort`;
- `/model`;
- `/safe`;
- `/normal`;
- `/exit`, `/quit`, `/q`.

Rules:

- Commands should update REPL state through `Repl` methods rather than mutating low-level fields directly.
- Commands that affect tool availability must refresh the advertised tool list.

---

## 11. Request and tool-call flow

A normal interactive turn follows this sequence:

```text
user input
   ↓
repl/tui sends work to worker
   ↓
Repl::process_message
   ↓
image detection and message insertion
   ↓
RequestBuilder::build
   ↓
LlmClient::create_message_streaming
   ↓
provider stream parser produces shared content blocks
   ↓
ResponseHandler::handle_response
   ├── assistant text displayed and stored
   ├── assistant tool_use blocks extracted
   ├── ToolExecutor executes native or MCP tools
   ├── tool_result blocks inserted as a user message
   └── next request sent if tool calls were present
   ↓
loop exits when the assistant response contains no local tool calls
   ↓
session saved
```

Protocol rules:

1. Assistant responses are stored with their text and tool-use blocks.
2. Tool results are sent back as user-message blocks.
3. Tool-result ids must match the originating tool-use ids.
4. Multiple tool results from one assistant response are grouped into one user turn.
5. Mid-turn user messages are folded into the next tool-result turn as steering text.
6. A cancellation or tool failure still produces a provider-valid tool result.
7. The tool loop stops at the configured maximum iteration count and asks the model for a recovery summary.

---

## 12. Security boundaries

Security-sensitive concerns have explicit owners.

### Filesystem access

- Path resolution and workspace classification: `tools/resolve.rs`.
- Low-level file operations: `tools/filesystem.rs`.
- Read / Write permission checks: `tools/executor.rs` and `tools/permissions/`.
- External path grants: `.sofos/config.local.toml`, `~/.sofos/config.toml`, or session-scoped prompt approval.

Rules:

- Workspace paths are allowed by default unless denied by configuration.
- External paths require explicit Read or Write grants.
- Read and Write grants are independent.
- Symlink-resolved canonical paths are used for external permission checks.
- Edit tools that read and write external paths require both scopes.

### Bash execution

- Permission tiers: `tools/permissions/manager.rs`.
- Structural command checks: `tools/bash/validate.rs`.
- Process execution and output capture: `tools/bash/executor.rs`.
- Output formatting: `tools/bash/output.rs`.

Rules:

- Allowed commands auto-run only after structural checks pass.
- Forbidden commands are blocked.
- Unknown commands prompt when interactive.
- Parent traversal, file redirection, here-documents, dangerous git operations, and denied read paths are rejected.
- External absolute or tilde paths require Bash-path grants.

### Tool output and provider limits

- Tool output caps: `tools/utils.rs` and `tools/executor.rs`.
- Provider request truncation: `api/truncate.rs` and `repl/conversation/`.
- Web fetch raw body cap: `tools/executor.rs`.
- MCP text and image caps: `tools/executor.rs`.

Rules:

- Large tool outputs should be truncated before they can exceed provider limits.
- Editing tools must read full file content internally; truncation is model-facing only.

---

## 13. Provider boundaries

Sofos supports Anthropic and OpenAI as conversation providers, plus Morph as an edit-application service.

| Concern | Owner |
|---|---|
| Provider-neutral message model | `api/types.rs` |
| Provider selection | `main.rs` |
| Shared provider façade | `api/mod.rs` |
| Anthropic HTTP and SSE | `api/anthropic/` |
| OpenAI HTTP and SSE | `api/openai/` |
| Morph Apply API | `api/morph.rs` |
| Model capabilities and pricing | `api/model_info.rs` |
| Request-level provider feature selection | `repl/request_builder.rs` |

Rules:

- Provider clients convert between provider wire formats and shared `api/types.rs` types.
- REPL orchestration should not depend on provider wire JSON shapes.
- Provider-specific blocks must be sanitized or skipped when they do not apply to the active provider.
- Reasoning, thinking, compaction, cache, and web-search settings are selected by provider and model capability.

---

## 14. Session persistence model

Sofos persists both API-continuation data and user-display data.

```text
Session
├── api_messages       provider-facing conversation continuation
├── display_messages   UI-friendly replay records
├── system_prompt      saved prompt context
├── token counters     persisted usage totals
├── model              saved model where available
├── safe_mode          saved tool-grant mode where available
├── created_at
└── updated_at
```

Rules:

- API messages are for continuing conversations.
- Display messages are for replaying previous sessions to the user.
- The saved system prompt prevents resumed sessions from silently changing context.
- The saved model prevents provider-incompatible resumes.
- Token counters must survive resume so cost and tiered-pricing state remain honest.
- The index file is a summary cache, not the source of full conversation truth.

---

## 15. Configuration and permission files

Sofos uses separate configuration surfaces:

```text
AGENTS.md                    project instructions, version controlled
.sofos/instructions.md       personal instructions, gitignored
.sofos/config.local.toml     workspace permission and MCP configuration
~/.sofos/config.toml         global permission and MCP configuration
.sofos/sessions/             saved sessions, gitignored
```

Rules:

- Project instructions are team-visible.
- Personal instructions and session data are local.
- Permission grants are explicit and scope-specific.
- Local configuration can override or extend global configuration.
- `.sofos/` should not be committed.

---

## 16. Single sources of truth

| Concern | Owner |
|---|---|
| CLI shape | `cli.rs` |
| Startup orchestration | `main.rs` |
| Runtime model and context defaults | `config.rs` |
| Error taxonomy | `error.rs` |
| Provider-neutral message and tool types | `api/types.rs` |
| Anthropic wire protocol | `api/anthropic/` |
| OpenAI wire protocol | `api/openai/` |
| Morph API calls | `api/morph.rs` |
| Model capabilities and pricing | `api/model_info.rs` |
| Request construction | `repl/request_builder.rs` |
| Response and tool-loop handling | `repl/response_handler.rs` |
| In-memory conversation history | `repl/conversation/` |
| Interactive TUI runtime | `repl/tui/` |
| Runtime session counters | `session/state.rs` |
| On-disk session format | `session/history/model.rs` |
| Session save / load orchestration | `session/history/manager.rs` |
| Session index | `session/history/index.rs` |
| Custom instruction loading | `session/history/instructions.rs` |
| Native tool dispatch | `tools/executor.rs` |
| Native tool schemas | `tools/types.rs` |
| Native tool-name parsing | `tools/tool_name.rs` |
| Path resolution and workspace classification | `tools/resolve.rs` |
| Low-level filesystem operations | `tools/filesystem.rs` |
| Bash execution | `tools/bash/executor.rs` |
| Bash structural validation | `tools/bash/validate.rs` |
| Permission settings and prompts | `tools/permissions/manager.rs` |
| Permission rule parsing | `tools/permissions/pattern.rs` |
| Code search | `tools/codesearch.rs` |
| `view_image` tool image loading | `tools/image.rs` |
| Morph output validation | `tools/morph_validate.rs` |
| MCP configuration | `mcp/config.rs` |
| MCP protocol shapes | `mcp/protocol.rs` |
| MCP server set and tool cache | `mcp/manager.rs` |
| MCP transports | `mcp/transport/` |
| Markdown rendering | `ui/markdown.rs` |
| Syntax highlighting | `ui/syntax.rs` |
| Diff rendering | `ui/diff.rs` |
| Cost display | `ui/cost.rs` |
| Slash commands | `commands/builtin.rs` |

No second implementation of these concerns should be added.

---

## 17. Dependency direction

The intended dependency graph is:

```text
main.rs
  ├── cli.rs
  ├── api/
  ├── repl/
  │   ├── api/
  │   ├── tools/
  │   ├── mcp/
  │   ├── session/
  │   └── ui/
  └── session/

repl/
  ├── api/       request / response types and provider façade
  ├── tools/     tool execution
  ├── mcp/       external tool manager through ToolExecutor
  ├── session/   persistence and runtime state
  ├── commands/  slash-command routing
  └── ui/        display helpers

tools/
  ├── api/       tool result block and image types where required
  ├── mcp/       MCP result routing through ToolExecutor
  ├── ui/        confirmations and diffs
  └── error.rs   error reporting
```

Dependency rules:

- `api/` must not depend on `repl/` or `tools/`, except for tool-name constants used to specialise argument parsing.
- Provider clients must not execute tools.
- `tools/` must not build provider requests.
- `mcp/` must not implement native tool semantics.
- `ui/` must not own core business logic.
- `session/history/` must not execute provider requests or tools.
- `repl/` is allowed to coordinate all layers but should not duplicate their internal logic.

---

## 18. Extension boundaries and non-goals

### Adding a native tool

A new native tool must update:

1. `tools/tool_name.rs`;
2. `tools/types.rs`;
3. `tools/executor.rs`;
4. security checks in `tools/resolve.rs`, `tools/permissions/`, or `tools/bash/` if the tool touches paths or commands;
5. tests;
6. README documentation if user-visible.

### Adding an MCP capability

MCP changes should stay inside `mcp/` unless they affect tool-result display, image caps, or provider-facing tool definitions.

### Adding a provider

A new conversation provider needs:

1. provider client and wire modules under `api/`;
2. an `LlmClient` variant;
3. request-builder support for provider-specific reasoning, caching, and tool shapes;
4. streaming and non-streaming conversion into shared `api/types.rs`;
5. session compatibility checks for provider-specific blocks.

### Non-goals

Sofos does not expose:

- unrestricted filesystem access without user grants;
- unrestricted shell execution;
- hidden tool execution;
- provider-specific wire objects throughout the REPL;
- MCP safe-mode filtering guarantees for third-party servers;
- a library API separate from the terminal binary.

---

## 19. Architectural invariants

The following invariants define the long-term structure of Sofos:

- The REPL owns turn orchestration.
- Provider clients own provider wire formats.
- Tool execution is visible and routed through `ToolExecutor`.
- Filesystem paths are resolved and permission-checked before side effects.
- Read, Write, and Bash permissions are independent scopes.
- Bash commands pass both command-tier and structural checks.
- Destructive filesystem operations require explicit confirmation.
- Editing tools must not operate on model-truncated file content.
- Every assistant tool use must receive a matching tool result.
- Tool loops are bounded.
- Interruptions must keep conversation history provider-valid.
- Session resume must preserve provider compatibility, safe mode, system prompt, and token counters.
- Provider-specific reasoning and cache features are selected from model capabilities.
- MCP tools are external extensions and are isolated behind the MCP manager.
- User-facing display concerns stay in `ui/` and `repl/tui/`.
- Each security-sensitive concern has one owner and no duplicate implementation.
