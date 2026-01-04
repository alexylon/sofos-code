# MCP Server Examples

Common MCP server configurations for Sofos.

## Filesystem

```toml
[mcpServers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/directory"]
```

## Databases

### PostgreSQL

```toml
[mcpServers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://localhost/mydb"]
```

### PostgreSQL with credentials

```toml
[mcpServers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://user@localhost/mydb"]
env = { "PGPASSWORD" = "secret123" }
```

### SQLite

```toml
[mcpServers.sqlite]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-sqlite", "/path/to/database.db"]
```

## APIs

### GitHub

```toml
[mcpServers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { "GITHUB_TOKEN" = "ghp_YOUR_TOKEN" }
```

Get token: https://github.com/settings/tokens

### GitLab

```toml
[mcpServers.gitlab]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-gitlab"]
env = { "GITLAB_TOKEN" = "glpat_YOUR_TOKEN" }
```

### Slack

```toml
[mcpServers.slack]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-slack"]
env = { "SLACK_BOT_TOKEN" = "xoxb-YOUR-TOKEN", "SLACK_TEAM_ID" = "T1234567890" }
```

## Cloud Storage

### Google Drive

```toml
[mcpServers.gdrive]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-google-drive"]
env = { "GOOGLE_APPLICATION_CREDENTIALS" = "/path/to/credentials.json" }
```

## Custom Servers

### HTTP Server

```toml
[mcpServers.custom]
url = "https://api.example.com/mcp"
headers = { "Authorization" = "Bearer YOUR_TOKEN" }
```

### Python Server

```toml
[mcpServers.python]
command = "python"
args = ["-m", "my_mcp_server"]
env = { "API_KEY" = "YOUR_KEY" }
```

### Custom Binary with Environment

```toml
[mcpServers.company-internal]
command = "/usr/local/bin/company-mcp-server"
args = ["--config", "/etc/company/mcp-config.json"]
env = { "COMPANY_API_URL" = "https://internal.company.com", "LOG_LEVEL" = "debug" }
```

## Multiple Servers

```toml
[mcpServers.docs]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/username/Documents"]

[mcpServers.projects]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/username/Projects"]

[mcpServers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { "GITHUB_TOKEN" = "ghp_YOUR_TOKEN" }
```

## Config Locations

- **Global:** `~/.sofos/config.toml` - Shared across projects
- **Local:** `.sofos/config.local.toml` - Project-specific (overrides global)

## Tool Naming

MCP tools are prefixed with their server name:
- Server: `github`
- Tool: `create_issue`
- Available as: `github_create_issue`

## Resources

- All servers: https://github.com/modelcontextprotocol/servers
- Documentation: https://modelcontextprotocol.io/
