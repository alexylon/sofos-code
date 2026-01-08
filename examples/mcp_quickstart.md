# MCP Quick Start Guide

## What is MCP?

MCP (Model Context Protocol) allows Sofos to connect to external tools and services: databases, file systems, APIs, and more.

## Quick Setup

### 1. Install Node.js (if needed)

```bash
# macOS
brew install node

# Or visit: https://nodejs.org/
```

### 2. Configure a Server

Create `~/.sofos/config.toml`:

```toml
[mcp-servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/yourusername/Documents"]
```

### 3. Start Sofos

```bash
sofos
```

You should see: `MCP servers initialized`

### 4. Use MCP Tools

```
λ> List files in my Documents folder

λ> Read test.txt from Documents
```

The AI will use tools like `filesystem_list_directory` and `filesystem_read_file`.

## Popular Servers

### GitHub

```toml
[mcp-servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { "GITHUB_TOKEN" = "ghp_YOUR_TOKEN" }
```

Get token: https://github.com/settings/tokens

### PostgreSQL

```toml
[mcp-servers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://localhost/mydb"]
```

### SQLite

```toml
[mcp-servers.sqlite]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-sqlite", "/path/to/database.db"]
```

## Config Locations

- **Global:** `~/.sofos/config.toml` - Shared across all projects
- **Local:** `.sofos/config.local.toml` - Project-specific, overrides global

## More Servers

See all official servers: https://github.com/modelcontextprotocol/servers

## Troubleshooting

**Server won't start?**
```bash
# Test directly
npx -y @modelcontextprotocol/server-filesystem /tmp
```

**Tools not working?**
- Check paths exist
- Verify API tokens are valid
- Ensure databases are running

## Resources

- Documentation: https://modelcontextprotocol.io/
- Create your own: https://modelcontextprotocol.io/docs/building-servers
