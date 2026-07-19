//! MCP client integration via JSON-RPC 2.0 over stdio / HTTP / SSE.
//!
//! When `mcp` feature is enabled, provides MCP server connection
//! and tool registration from MCP servers.

use std::collections::HashMap;
use std::process::Stdio;

use async_trait::async_trait;
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

#[async_trait]
trait McpTransport: Send {
    async fn send_request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, McpError>;
    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), McpError>;
    async fn close(&mut self) -> Result<(), McpError>;
}

struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
}

impl StdioTransport {
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

#[async_trait]
impl McpTransport for StdioTransport {
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

    async fn close(&mut self) -> Result<(), McpError> {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        Ok(())
    }
}

struct HttpTransport {
    client: reqwest::Client,
    url: String,
    headers: HashMap<String, String>,
    next_id: u64,
    session_id: Option<String>,
    prefer_sse: bool,
}

impl HttpTransport {
    fn new(
        url: String,
        headers: HashMap<String, String>,
        prefer_sse: bool,
    ) -> Result<Self, McpError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("rx4/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| McpError::Transport(e.to_string()))?;
        Ok(Self {
            client,
            url,
            headers,
            next_id: 1,
            session_id: None,
            prefer_sse,
        })
    }

    fn apply_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let accept = if self.prefer_sse {
            "text/event-stream, application/json"
        } else {
            "application/json, text/event-stream"
        };
        let mut req = req
            .header("Content-Type", "application/json")
            .header("Accept", accept);
        if let Some(sid) = &self.session_id {
            req = req.header("Mcp-Session-Id", sid);
        }
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        req
    }

    async fn post_json(&mut self, body: &Value) -> Result<(String, String), McpError> {
        let req = self.apply_headers(self.client.post(&self.url).json(body));
        let resp = req
            .send()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(sid.to_string());
        }
        let text = resp
            .text()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(McpError::Transport(format!(
                "HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            )));
        }
        Ok((content_type, text))
    }
}

fn result_from_response(resp: Response) -> Result<Value, McpError> {
    if let Some(err) = resp.error {
        return Err(McpError::Protocol(err.message));
    }
    Ok(resp.result.unwrap_or(Value::Null))
}

fn parse_json_rpc_body(body: &str, expected_id: u64) -> Result<Value, McpError> {
    let resp: Response =
        serde_json::from_str(body.trim()).map_err(|e| McpError::Protocol(e.to_string()))?;
    if resp.id.is_some() && resp.id != Some(expected_id) {
        return Err(McpError::Protocol(format!(
            "response id mismatch: expected {expected_id}, got {:?}",
            resp.id
        )));
    }
    result_from_response(resp)
}

fn parse_sse_body(body: &str, expected_id: u64) -> Result<Value, McpError> {
    let mut last_err: Option<McpError> = None;
    for block in body.split("\n\n") {
        let mut data_lines = Vec::new();
        for line in block.lines() {
            if let Some(rest) = line.trim_end().strip_prefix("data:") {
                data_lines.push(rest.trim_start());
            }
        }
        if data_lines.is_empty() {
            continue;
        }
        let data = data_lines.join("\n");
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        match serde_json::from_str::<Response>(&data) {
            Ok(resp) if resp.id == Some(expected_id) => return result_from_response(resp),
            Ok(_) => {}
            Err(e) => last_err = Some(McpError::Protocol(e.to_string())),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        McpError::Protocol(format!("no SSE response for id {expected_id} in stream"))
    }))
}

#[async_trait]
impl McpTransport for HttpTransport {
    async fn send_request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let body = serde_json::to_value(Request {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })
        .map_err(|e| McpError::Transport(e.to_string()))?;

        let (content_type, text) = self.post_json(&body).await?;
        if content_type
            .to_ascii_lowercase()
            .contains("text/event-stream")
        {
            parse_sse_body(&text, id)
        } else {
            parse_json_rpc_body(&text, id)
        }
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), McpError> {
        let body = serde_json::to_value(Notification {
            jsonrpc: "2.0",
            method,
            params,
        })
        .map_err(|e| McpError::Transport(e.to_string()))?;
        let _ = self.post_json(&body).await?;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), McpError> {
        Ok(())
    }
}

type SsePendingMap = std::sync::Arc<
    tokio::sync::Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Result<Value, McpError>>>>,
>;

struct SseGetTransport {
    client: reqwest::Client,
    post_url: String,
    headers: HashMap<String, String>,
    next_id: u64,
    session_id: Option<String>,
    pending: SsePendingMap,
    _reader: tokio::task::JoinHandle<()>,
}

impl SseGetTransport {
    async fn connect(
        sse_url: String,
        post_url: String,
        headers: HashMap<String, String>,
    ) -> Result<Self, McpError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("rx4/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| McpError::Transport(e.to_string()))?;
        let mut req = client.get(&sse_url).header("Accept", "text/event-stream");
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(McpError::Transport(format!(
                "SSE GET HTTP {}",
                resp.status()
            )));
        }
        let mut session_id = None;
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            session_id = Some(sid.to_string());
        }
        let pending: SsePendingMap = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let pending_r = pending.clone();
        let reader = tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = resp.bytes_stream();
            let mut buf = String::new();
            while let Some(chunk) = stream.next().await {
                let Ok(bytes) = chunk else {
                    break;
                };
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(pos) = buf.find("\n\n") {
                    let block = buf[..pos].to_string();
                    buf = buf[pos + 2..].to_string();
                    let mut data_lines = Vec::new();
                    for line in block.lines() {
                        if let Some(rest) = line.trim_end().strip_prefix("data:") {
                            data_lines.push(rest.trim_start());
                        }
                    }
                    if data_lines.is_empty() {
                        continue;
                    }
                    let data = data_lines.join("\n");
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    if let Ok(resp) = serde_json::from_str::<Response>(&data) {
                        if let Some(id) = resp.id {
                            let mut map = pending_r.lock().await;
                            if let Some(tx) = map.remove(&id) {
                                let _ = tx.send(result_from_response(resp));
                            }
                        }
                    }
                }
            }
        });
        Ok(Self {
            client,
            post_url,
            headers,
            next_id: 1,
            session_id,
            pending,
            _reader: reader,
        })
    }
}

#[async_trait]
impl McpTransport for SseGetTransport {
    async fn send_request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let body = serde_json::to_value(Request {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })
        .map_err(|e| McpError::Transport(e.to_string()))?;
        let mut req = self
            .client
            .post(&self.post_url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&body);
        if let Some(sid) = &self.session_id {
            req = req.header("Mcp-Session-Id", sid);
        }
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(sid.to_string());
        }
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let text = resp
            .text()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        if !status.is_success() {
            self.pending.lock().await.remove(&id);
            return Err(McpError::Transport(format!(
                "HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            )));
        }
        // Some servers answer on POST body; others only on SSE channel.
        if content_type.contains("application/json") && !text.trim().is_empty() {
            self.pending.lock().await.remove(&id);
            return parse_json_rpc_body(&text, id);
        }
        if content_type.contains("text/event-stream") && !text.trim().is_empty() {
            self.pending.lock().await.remove(&id);
            return parse_sse_body(&text, id);
        }
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => Err(McpError::Transport("SSE response channel closed".into())),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(McpError::Transport(
                    "timed out waiting for SSE response".into(),
                ))
            }
        }
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), McpError> {
        let body = serde_json::to_value(Notification {
            jsonrpc: "2.0",
            method,
            params,
        })
        .map_err(|e| McpError::Transport(e.to_string()))?;
        let mut req = self
            .client
            .post(&self.post_url)
            .header("Content-Type", "application/json")
            .json(&body);
        if let Some(sid) = &self.session_id {
            req = req.header("Mcp-Session-Id", sid);
        }
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let _ = req
            .send()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), McpError> {
        self._reader.abort();
        Ok(())
    }
}

async fn handshake(transport: &mut dyn McpTransport) -> Result<(), McpError> {
    let init_params = json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": { "name": "rx4", "version": env!("CARGO_PKG_VERSION") }
    });
    transport
        .send_request("initialize", Some(init_params))
        .await
        .map_err(|e| McpError::Protocol(format!("initialize failed: {e}")))?;
    transport
        .send_notification("notifications/initialized", None)
        .await?;
    Ok(())
}

/// Client connected to a single MCP server over stdio, HTTP, or SSE.
pub struct McpClient {
    inner: Mutex<Box<dyn McpTransport>>,
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

        let mut transport = StdioTransport {
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
        };
        handshake(&mut transport).await?;

        Ok(Self {
            inner: Mutex::new(Box::new(transport)),
        })
    }

    /// Connects to a remote MCP server via JSON-RPC over HTTP POST.
    ///
    /// Responses may be plain JSON or `text/event-stream` (streamable HTTP).
    pub async fn connect_http(
        url: &str,
        headers: Option<HashMap<String, String>>,
    ) -> Result<Self, McpError> {
        let mut transport =
            HttpTransport::new(url.to_string(), headers.unwrap_or_default(), false)?;
        handshake(&mut transport).await?;
        Ok(Self {
            inner: Mutex::new(Box::new(transport)),
        })
    }

    /// Connects to a remote MCP server preferring SSE-style responses.
    ///
    /// Uses the same streamable HTTP POST path as [`Self::connect_http`], with
    /// `Accept` set to prefer `text/event-stream` when the server offers it.
    pub async fn connect_sse(
        url: &str,
        headers: Option<HashMap<String, String>>,
    ) -> Result<Self, McpError> {
        let mut transport = HttpTransport::new(url.to_string(), headers.unwrap_or_default(), true)?;
        handshake(&mut transport).await?;
        Ok(Self {
            inner: Mutex::new(Box::new(transport)),
        })
    }

    /// Connect with a long-lived SSE GET channel for server messages plus HTTP POST for requests.
    ///
    /// `sse_url` is opened with `GET` + `Accept: text/event-stream`. Requests POST to
    /// `post_url` (defaults to `sse_url`). Falls back to streamable HTTP POST when GET fails.
    pub async fn connect_sse_get(
        sse_url: &str,
        post_url: Option<&str>,
        headers: Option<HashMap<String, String>>,
    ) -> Result<Self, McpError> {
        let headers = headers.unwrap_or_default();
        let post = post_url.unwrap_or(sse_url).to_string();
        match SseGetTransport::connect(sse_url.to_string(), post, headers.clone()).await {
            Ok(mut transport) => {
                handshake(&mut transport).await?;
                Ok(Self {
                    inner: Mutex::new(Box::new(transport)),
                })
            }
            Err(e) => {
                tracing::warn!(
                    "sse GET connect failed ({e}); falling back to POST streamable HTTP"
                );
                Self::connect_sse(sse_url, Some(headers)).await
            }
        }
    }

    /// Connect from marketplace/host config (`stdio` | `http` | `sse`).
    pub async fn connect_config(
        cfg: &crate::marketplace::McpServerConfig,
    ) -> Result<Self, McpError> {
        use crate::marketplace::McpTransportKind;
        match cfg.transport {
            McpTransportKind::Stdio => {
                if cfg.command.is_empty() {
                    return Err(McpError::Spawn("stdio MCP config missing command".into()));
                }
                let args: Vec<&str> = cfg.args.iter().map(String::as_str).collect();
                Self::connect_stdio(&cfg.command, &args).await
            }
            McpTransportKind::Http => {
                let url = cfg
                    .url
                    .as_deref()
                    .ok_or_else(|| McpError::Transport("http MCP config missing url".into()))?;
                let headers = if cfg.headers.is_empty() {
                    None
                } else {
                    Some(cfg.headers.clone())
                };
                Self::connect_http(url, headers).await
            }
            McpTransportKind::Sse => {
                let url = cfg
                    .url
                    .as_deref()
                    .ok_or_else(|| McpError::Transport("sse MCP config missing url".into()))?;
                let headers = if cfg.headers.is_empty() {
                    None
                } else {
                    Some(cfg.headers.clone())
                };
                Self::connect_sse(url, headers).await
            }
        }
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
        inner.close().await
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

    #[test]
    fn parse_json_rpc_success() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
        let v = parse_json_rpc_body(body, 1).unwrap();
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn parse_json_rpc_error() {
        let body = r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32600,"message":"bad"}}"#;
        let err = parse_json_rpc_body(body, 2).unwrap_err();
        assert!(matches!(err, McpError::Protocol(m) if m == "bad"));
    }

    #[test]
    fn parse_sse_json_data_events() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"tools\":[]}}\n\n";
        let v = parse_sse_body(body, 3).unwrap();
        assert!(v.get("tools").is_some());
    }

    #[test]
    fn parse_sse_skips_unrelated_ids() {
        let body = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"a\":1}}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"b\":2}}\n\n",
        );
        let v = parse_sse_body(body, 9).unwrap();
        assert_eq!(v["b"], 2);
    }
}
