# MCP Server Examples

Common MCP server configurations for Sofos.

## Filesystem

```toml
[mcp-servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/directory"]
```

## Databases

### PostgreSQL

```toml
[mcp-servers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://localhost/mydb"]
```

### PostgreSQL with credentials

```toml
[mcp-servers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://user@localhost/mydb"]
env = { "PGPASSWORD" = "secret123" }
```

### SQLite

```toml
[mcp-servers.sqlite]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-sqlite", "/path/to/database.db"]
```

## APIs

### GitHub

```toml
[mcp-servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { "GITHUB_TOKEN" = "ghp_YOUR_TOKEN" }
```

Get token: https://github.com/settings/tokens

### GitLab

```toml
[mcp-servers.gitlab]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-gitlab"]
env = { "GITLAB_TOKEN" = "glpat_YOUR_TOKEN" }
```

### Slack

```toml
[mcp-servers.slack]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-slack"]
env = { "SLACK_BOT_TOKEN" = "xoxb-YOUR-TOKEN", "SLACK_TEAM_ID" = "T1234567890" }
```

## Cloud Storage

### Google Drive

```toml
[mcp-servers.gdrive]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-google-drive"]
env = { "GOOGLE_APPLICATION_CREDENTIALS" = "/path/to/credentials.json" }
```

## Custom Servers

### HTTP Server

```toml
[mcp-servers.custom]
url = "https://api.example.com/mcp"
headers = { "Authorization" = "Bearer YOUR_TOKEN" }
```

### Python Server

```toml
[mcp-servers.python]
command = "python"
args = ["-m", "my_mcp_server"]
env = { "API_KEY" = "YOUR_KEY" }
```

### Custom Binary with Environment

```toml
[mcp-servers.company-internal]
command = "/usr/local/bin/company-mcp-server"
args = ["--config", "/etc/company/mcp-config.json"]
env = { "COMPANY_API_URL" = "https://internal.company.com", "LOG_LEVEL" = "debug" }
```

## Multiple Servers

```toml
[mcp-servers.docs]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/username/Documents"]

[mcp-servers.projects]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/username/Projects"]

[mcp-servers.github]
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
