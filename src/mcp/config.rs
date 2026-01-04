use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(rename = "mcpServers", default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
}

impl McpServerConfig {
    pub fn is_stdio(&self) -> bool {
        self.command.is_some()
    }

    pub fn is_http(&self) -> bool {
        self.url.is_some()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.command.is_none() && self.url.is_none() {
            return Err("MCP server must have either 'command' or 'url'".to_string());
        }

        if self.command.is_some() && self.url.is_some() {
            return Err("MCP server cannot have both 'command' and 'url'".to_string());
        }

        Ok(())
    }
}

/// Load MCP configuration from both global and local config files
pub fn load_mcp_config(workspace: &Path) -> HashMap<String, McpServerConfig> {
    let mut servers = HashMap::new();

    // Try to load global config from ~/.sofos/config.toml
    if let Some(home) = std::env::var_os("HOME") {
        let global_config_path = PathBuf::from(home).join(".sofos/config.toml");
        if let Ok(global_servers) = load_mcp_config_from_file(&global_config_path) {
            servers.extend(global_servers);
        }
    }

    // Try to load local config from .sofos/config.local.toml (overrides global)
    let local_config_path = workspace.join(".sofos/config.local.toml");
    if let Ok(local_servers) = load_mcp_config_from_file(&local_config_path) {
        servers.extend(local_servers);
    }

    servers
}

fn load_mcp_config_from_file(
    path: &PathBuf,
) -> Result<HashMap<String, McpServerConfig>, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let content = std::fs::read_to_string(path)?;
    let config: McpConfig = toml::from_str(&content)?;

    // Validate all server configs
    for (name, server_config) in &config.mcp_servers {
        if let Err(e) = server_config.validate() {
            eprintln!("Warning: Invalid MCP server config for '{}': {}", name, e);
        }
    }

    Ok(config.mcp_servers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stdio_server() {
        let toml_content = r#"
[mcpServers.test-server]
command = "/path/to/server"
args = ["--port", "8080"]
env = { "API_KEY" = "secret" }
"#;

        let config: McpConfig = toml::from_str(toml_content).unwrap();
        let server = config.mcp_servers.get("test-server").unwrap();

        assert_eq!(server.command, Some("/path/to/server".to_string()));
        assert_eq!(
            server.args,
            Some(vec!["--port".to_string(), "8080".to_string()])
        );
        assert!(server.is_stdio());
        assert!(!server.is_http());
    }

    #[test]
    fn test_parse_http_server() {
        let toml_content = r#"
[mcpServers.http-server]
url = "https://example.com/mcp"
headers = { "Authorization" = "Bearer token" }
"#;

        let config: McpConfig = toml::from_str(toml_content).unwrap();
        let server = config.mcp_servers.get("http-server").unwrap();

        assert_eq!(server.url, Some("https://example.com/mcp".to_string()));
        assert!(!server.is_stdio());
        assert!(server.is_http());
    }

    #[test]
    fn test_validation() {
        let valid_stdio = McpServerConfig {
            command: Some("/path/to/server".to_string()),
            args: None,
            env: None,
            url: None,
            headers: None,
        };
        assert!(valid_stdio.validate().is_ok());

        let valid_http = McpServerConfig {
            command: None,
            args: None,
            env: None,
            url: Some("https://example.com".to_string()),
            headers: None,
        };
        assert!(valid_http.validate().is_ok());

        let invalid_empty = McpServerConfig {
            command: None,
            args: None,
            env: None,
            url: None,
            headers: None,
        };
        assert!(invalid_empty.validate().is_err());

        let invalid_both = McpServerConfig {
            command: Some("/path".to_string()),
            args: None,
            env: None,
            url: Some("https://example.com".to_string()),
            headers: None,
        };
        assert!(invalid_both.validate().is_err());
    }
}
