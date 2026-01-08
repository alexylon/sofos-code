# Changelog

All notable changes to Sofos are documented in this file.

## [Unreleased]

## [0.1.17] - 2026-01-08

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
