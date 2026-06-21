use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(rename = "mcp-servers", default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

/// How an MCP server's tools relate to read-only mode. Default `Disabled`
/// means tools from this server are filtered out when read-only mode is on,
/// because Sofos cannot tell which MCP tools mutate state. Users opt
/// individual servers in once they have verified the server is safe.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReadOnlyAccess {
    /// Filter tools from this server out in read-only mode. Default.
    #[default]
    Disabled,
    /// Include the server's tools in read-only mode — used when the server is
    /// known to only expose read-only operations.
    ReadOnly,
    /// Include the server's tools in read-only mode — explicit opt-in even
    /// when the server may mutate state.
    Allow,
}

impl ReadOnlyAccess {
    pub fn is_available_in_readonly(self) -> bool {
        matches!(self, ReadOnlyAccess::ReadOnly | ReadOnlyAccess::Allow)
    }
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

    /// Whether the server's tools are exposed in read-only mode. Defaults to
    /// `Disabled` so configuring a new server does not silently grant it
    /// access to read-only sessions.
    #[serde(default, alias = "safe_mode")]
    pub readonly: ReadOnlyAccess,
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

/// Load MCP configuration from the global and local config files. The
/// local file overrides the global one when both define a server with the
/// same name. A file that cannot be read or parsed is reported and
/// skipped, so one broken file does not hide the servers in the other.
pub fn load_mcp_config(workspace: &Path) -> HashMap<String, McpServerConfig> {
    let mut servers = HashMap::new();

    if let Some(global_config_path) = crate::config::global_config_path() {
        match load_mcp_config_from_file(&global_config_path) {
            Ok(global_servers) => servers.extend(global_servers),
            Err(e) => {
                // The raw error can quote a config line holding a secret, so
                // log it only at DEBUG, not in the default WARN output.
                tracing::warn!(
                    path = %global_config_path.display(),
                    "failed to read global MCP config; skipping"
                );
                tracing::debug!(error = %e, "global MCP config read error");
            }
        }
    }

    let local_config_path = workspace.join(crate::config::LOCAL_CONFIG_FILE);
    match load_mcp_config_from_file(&local_config_path) {
        Ok(local_servers) => servers.extend(local_servers),
        Err(e) => {
            tracing::warn!(
                path = %local_config_path.display(),
                "failed to read local MCP config; skipping"
            );
            tracing::debug!(error = %e, "local MCP config read error");
        }
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

    // Drop invalid entries at load time. The previous version logged a
    // warning but still returned them, which made every later step
    // (connect, list_tools) re-fail with the generic "Invalid MCP
    // server configuration" error — useless without the original
    // reason. Filtering here keeps the warning informative while
    // removing the noise downstream.
    let mut servers = HashMap::new();
    for (name, server_config) in config.mcp_servers {
        match server_config.validate() {
            Ok(()) => {
                servers.insert(name, server_config);
            }
            Err(e) => {
                tracing::warn!(
                    server = %name,
                    error = %e,
                    "invalid MCP server config; skipping"
                );
            }
        }
    }

    Ok(servers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stdio_server() {
        let toml_content = r#"
[mcp-servers.test-server]
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
[mcp-servers.http-server]
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
    fn load_filters_out_invalid_server_entries() {
        // The loader used to log a warning and return invalid entries,
        // which then re-failed with the opaque "Invalid MCP server
        // configuration" error on every later request. Bad entries are
        // now dropped at parse time so only well-formed entries survive.
        let workspace = tempfile::tempdir().unwrap();
        let config_dir = workspace.path().join(".sofos");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.local.toml"),
            r#"
[mcp-servers.valid-stdio]
command = "/usr/bin/server"

[mcp-servers.invalid-empty]
# neither command nor url — should be dropped

[mcp-servers.invalid-both]
command = "/usr/bin/server"
url = "https://example.com/mcp"
"#,
        )
        .unwrap();

        let servers = load_mcp_config(workspace.path());
        assert!(
            servers.contains_key("valid-stdio"),
            "well-formed entry must survive: {:?}",
            servers.keys().collect::<Vec<_>>()
        );
        assert!(
            !servers.contains_key("invalid-empty"),
            "empty entry must be filtered: {:?}",
            servers.keys().collect::<Vec<_>>()
        );
        assert!(
            !servers.contains_key("invalid-both"),
            "ambiguous entry must be filtered: {:?}",
            servers.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_validation() {
        let valid_stdio = McpServerConfig {
            command: Some("/path/to/server".to_string()),
            args: None,
            env: None,
            url: None,
            headers: None,
            readonly: ReadOnlyAccess::default(),
        };
        assert!(valid_stdio.validate().is_ok());

        let valid_http = McpServerConfig {
            command: None,
            args: None,
            env: None,
            url: Some("https://example.com".to_string()),
            headers: None,
            readonly: ReadOnlyAccess::default(),
        };
        assert!(valid_http.validate().is_ok());

        let invalid_empty = McpServerConfig {
            command: None,
            args: None,
            env: None,
            url: None,
            headers: None,
            readonly: ReadOnlyAccess::default(),
        };
        assert!(invalid_empty.validate().is_err());

        let invalid_both = McpServerConfig {
            command: Some("/path".to_string()),
            args: None,
            env: None,
            url: Some("https://example.com".to_string()),
            headers: None,
            readonly: ReadOnlyAccess::default(),
        };
        assert!(invalid_both.validate().is_err());
    }

    #[test]
    fn readonly_defaults_to_disabled() {
        let toml_content = r#"
[mcp-servers.test-server]
command = "/path/to/server"
"#;
        let config: McpConfig = toml::from_str(toml_content).unwrap();
        let server = config.mcp_servers.get("test-server").unwrap();
        assert_eq!(server.readonly, ReadOnlyAccess::Disabled);
        assert!(!server.readonly.is_available_in_readonly());
    }

    #[test]
    fn readonly_parses_read_only_and_allow() {
        let toml_content = r#"
[mcp-servers.docs]
command = "/srv"
readonly = "read_only"

[mcp-servers.write]
command = "/srv"
readonly = "allow"
"#;
        let config: McpConfig = toml::from_str(toml_content).unwrap();
        assert_eq!(
            config.mcp_servers["docs"].readonly,
            ReadOnlyAccess::ReadOnly
        );
        assert_eq!(config.mcp_servers["write"].readonly, ReadOnlyAccess::Allow);
        assert!(
            config.mcp_servers["docs"]
                .readonly
                .is_available_in_readonly()
        );
        assert!(
            config.mcp_servers["write"]
                .readonly
                .is_available_in_readonly()
        );
    }

    /// Configs written before the rename used the `safe_mode` key; the
    /// serde alias keeps them working.
    #[test]
    fn legacy_safe_mode_key_is_still_accepted() {
        let toml_content = r#"
[mcp-servers.docs]
command = "/srv"
safe_mode = "read_only"
"#;
        let config: McpConfig = toml::from_str(toml_content).unwrap();
        assert_eq!(
            config.mcp_servers["docs"].readonly,
            ReadOnlyAccess::ReadOnly
        );
    }
}
