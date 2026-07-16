//! MCP client integration via rmcp.
//!
//! When `mcp` feature is enabled, provides MCP server connection
//! and tool registration from MCP servers.

#[cfg(feature = "mcp")]
pub struct McpClient;

#[cfg(feature = "mcp")]
impl McpClient {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "mcp")]
impl Default for McpClient {
    fn default() -> Self {
        Self::new()
    }
}
