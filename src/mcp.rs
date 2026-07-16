//! MCP client integration via JSON-RPC 2.0 over stdio.
//!
//! When `mcp` feature is enabled, provides MCP server connection
//! and tool registration from MCP servers.

use std::collections::HashMap;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// Errors produced by MCP client and registry operations.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("spawn error: {0}")]
    Spawn(String),
}

/// Metadata describing a tool exposed by an MCP server.
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: Value,
}

/// Metadata describing a resource exposed by an MCP server.
#[derive(Debug, Clone, Deserialize)]
pub struct McpResourceInfo {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "mimeType", default)]
    pub mime_type: Option<String>,
}

#[derive(Serialize)]
struct Request<'a> {
    jsonrpc: &'a str,
    id: u64,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Serialize)]
struct Notification<'a> {
    jsonrpc: &'a str,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    #[allow(dead_code)]
    code: i64,
    message: String,
}

struct ClientInner {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
}

impl ClientInner {
    async fn send_request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        let line = serde_json::to_string(&req).map_err(|e| McpError::Transport(e.to_string()))?;
        self.write_line(&line).await?;

        loop {
            let mut buf = String::new();
            let n = self
                .reader
                .read_line(&mut buf)
                .await
                .map_err(|e| McpError::Transport(e.to_string()))?;
            if n == 0 {
                return Err(McpError::Transport("connection closed by peer".into()));
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }
            let resp: Response =
                serde_json::from_str(trimmed).map_err(|e| McpError::Protocol(e.to_string()))?;
            if resp.id == Some(id) {
                if let Some(err) = resp.error {
                    return Err(McpError::Protocol(err.message));
                }
                return Ok(resp.result.unwrap_or(Value::Null));
            }
        }
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), McpError> {
        let notif = Notification {
            jsonrpc: "2.0",
            method,
            params,
        };
        let line = serde_json::to_string(&notif).map_err(|e| McpError::Transport(e.to_string()))?;
        self.write_line(&line).await
    }

    async fn write_line(&mut self, line: &str) -> Result<(), McpError> {
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        Ok(())
    }
}

/// Client connected to a single MCP server over stdio.
pub struct McpClient {
    inner: Mutex<ClientInner>,
}

impl McpClient {
    /// Spawns a child process and establishes an MCP connection via stdin/stdout.
    pub async fn connect_stdio(command: &str, args: &[&str]) -> Result<Self, McpError> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| McpError::Spawn(format!("failed to spawn {command}: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Spawn("child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Spawn("child has no stdout".into()))?;

        let mut inner = ClientInner {
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
        };

        let init_params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "rx4", "version": "0.3.0" }
        });
        inner
            .send_request("initialize", Some(init_params))
            .await
            .map_err(|e| McpError::Protocol(format!("initialize failed: {e}")))?;
        inner
            .send_notification("notifications/initialized", None)
            .await?;

        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Calls `tools/list` and returns the tools exposed by the server.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let mut inner = self.inner.lock().await;
        let result = inner.send_request("tools/list", None).await?;
        let tools = result
            .get("tools")
            .ok_or_else(|| McpError::Protocol("missing 'tools' field in response".into()))?;
        serde_json::from_value(tools.clone())
            .map_err(|e| McpError::Protocol(format!("failed to parse tools: {e}")))
    }

    /// Calls `tools/call` with the given arguments and returns the raw result.
    pub async fn call_tool(&self, name: &str, arguments: &Value) -> Result<Value, McpError> {
        let mut inner = self.inner.lock().await;
        let params = json!({ "name": name, "arguments": arguments });
        inner.send_request("tools/call", Some(params)).await
    }

    /// Calls `resources/list` and returns the resources exposed by the server.
    pub async fn list_resources(&self) -> Result<Vec<McpResourceInfo>, McpError> {
        let mut inner = self.inner.lock().await;
        let result = inner.send_request("resources/list", None).await?;
        let resources = result
            .get("resources")
            .ok_or_else(|| McpError::Protocol("missing 'resources' field in response".into()))?;
        serde_json::from_value(resources.clone())
            .map_err(|e| McpError::Protocol(format!("failed to parse resources: {e}")))
    }

    /// Calls `resources/read` and returns the text content of the first entry.
    pub async fn read_resource(&self, uri: &str) -> Result<String, McpError> {
        let mut inner = self.inner.lock().await;
        let params = json!({ "uri": uri });
        let result = inner.send_request("resources/read", Some(params)).await?;
        let contents = result
            .get("contents")
            .ok_or_else(|| McpError::Protocol("missing 'contents' field in response".into()))?;
        if let Some(arr) = contents.as_array() {
            if let Some(first) = arr.first() {
                if let Some(text) = first.get("text").and_then(|t| t.as_str()) {
                    return Ok(text.to_string());
                }
            }
        }
        Ok(result.to_string())
    }

    /// Gracefully shuts down the connection by terminating the child process.
    pub async fn close(&mut self) -> Result<(), McpError> {
        let mut inner = self.inner.lock().await;
        let _ = inner.child.kill().await;
        let _ = inner.child.wait().await;
        Ok(())
    }
}

/// Parses a fully-qualified tool name of the form `mcp__{server}__{tool}`
/// into its server and tool components.
#[cfg(test)]
fn parse_tool_name(full_name: &str) -> Option<(&str, &str)> {
    let rest = full_name.strip_prefix("mcp__")?;
    rest.split_once("__")
}

/// Registry managing multiple MCP servers and routing tool calls.
pub struct McpRegistry {
    servers: HashMap<String, McpClient>,
    tool_index: HashMap<String, (String, String)>,
}

impl McpRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self {
            servers: HashMap::new(),
            tool_index: HashMap::new(),
        }
    }

    /// Registers a connected server, indexing its tools for routing.
    pub async fn register(&mut self, name: &str, client: McpClient) -> Result<(), McpError> {
        let tools = client.list_tools().await?;
        for tool in tools {
            let full = format!("mcp__{name}__{}", tool.name);
            self.tool_index.insert(full, (name.to_string(), tool.name));
        }
        self.servers.insert(name.to_string(), client);
        Ok(())
    }

    /// Returns all tool names from all registered servers, prefixed with
    /// `mcp__{server}__{tool}`.
    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tool_index.keys().cloned().collect();
        names.sort();
        names
    }

    /// Routes a fully-qualified tool call to the appropriate server.
    pub async fn call(&self, full_name: &str, arguments: &Value) -> Result<Value, McpError> {
        let (server, tool) = self
            .tool_index
            .get(full_name)
            .ok_or_else(|| McpError::NotFound(format!("tool not found: {full_name}")))?;
        let client = self
            .servers
            .get(server)
            .ok_or_else(|| McpError::NotFound(format!("server not found: {server}")))?;
        client.call_tool(tool, arguments).await
    }

    /// Shuts down all registered servers.
    pub async fn close_all(&mut self) -> Result<(), McpError> {
        let mut last_err = None;
        for client in self.servers.values_mut() {
            if let Err(e) = client.close().await {
                last_err = Some(e);
            }
        }
        self.servers.clear();
        self.tool_index.clear();
        if let Some(e) = last_err {
            return Err(e);
        }
        Ok(())
    }
}

impl Default for McpRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl McpRegistry {
        fn register_tools_for_test(&mut self, name: &str, tools: &[&str]) {
            for tool in tools {
                let full = format!("mcp__{name}__{tool}");
                self.tool_index
                    .insert(full, (name.to_string(), tool.to_string()));
            }
        }
    }

    #[test]
    fn test_parse_tool_name_format() {
        assert_eq!(
            parse_tool_name("mcp__fs__read_file"),
            Some(("fs", "read_file"))
        );
        assert_eq!(parse_tool_name("mcp__git__status"), Some(("git", "status")));
        assert_eq!(parse_tool_name("read_file"), None);
        assert_eq!(parse_tool_name("mcp__fs"), None);
        assert_eq!(parse_tool_name("mcp__"), None);
    }

    #[test]
    fn test_registry_tool_name_prefixing() {
        let mut registry = McpRegistry::new();
        registry.register_tools_for_test("fs", &["read_file", "write_file"]);
        registry.register_tools_for_test("git", &["status"]);

        let names = registry.tool_names();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"mcp__fs__read_file".to_string()));
        assert!(names.contains(&"mcp__fs__write_file".to_string()));
        assert!(names.contains(&"mcp__git__status".to_string()));
    }

    #[tokio::test]
    async fn test_error_unknown_server() {
        let mut registry = McpRegistry::new();
        registry.register_tools_for_test("fs", &["read_file"]);

        let result = registry.call("mcp__unknown__tool", &Value::Null).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            McpError::NotFound(msg) => assert!(msg.contains("mcp__unknown__tool")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_error_unknown_tool() {
        let mut registry = McpRegistry::new();
        registry.register_tools_for_test("fs", &["read_file"]);

        let result = registry.call("mcp__fs__nonexistent", &Value::Null).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            McpError::NotFound(msg) => assert!(msg.contains("mcp__fs__nonexistent")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn test_registry_default() {
        let registry = McpRegistry::default();
        assert!(registry.tool_names().is_empty());
    }
}
