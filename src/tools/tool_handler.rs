use crate::error::Result;
use async_trait::async_trait;
use serde_json::Value;

/// Trait for tool implementations
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Get the tool's name
    fn name(&self) -> &'static str;
    
    /// Execute the tool with given input
    async fn execute(&self, input: &Value) -> Result<String>;
    
    /// Check if this tool requires special capabilities
    fn requires_capability(&self) -> Option<ToolCapability> {
        None
    }
}

/// Capabilities that tools might require
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCapability {
    /// Requires Morph API client
    Morph,
    /// Requires ripgrep (code search)
    CodeSearch,
}
