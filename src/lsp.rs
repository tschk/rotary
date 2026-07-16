//! LSP client manager — JSON-RPC 2.0 over stdio with Content-Length framing.
//!
//! Manages one or more language server processes, sends initialize/didOpen/
//! didChange/didClose, collects published diagnostics, and forwards
//! references/definition requests. No external LSP crate; the protocol is
//! implemented manually over the child process stdin/stdout.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::warn;

/// Errors produced by LSP client operations.
#[derive(Debug, Error)]
pub enum LspError {
    #[error("failed to spawn LSP server: {0}")]
    Spawn(String),
    #[error("LSP protocol error: {0}")]
    Protocol(String),
    #[error("LSP server for language {0} not started")]
    NotStarted(String),
    #[error("unknown language: {0}")]
    UnknownLanguage(String),
}

/// Simplified view of a server's `ServerCapabilities`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LspCapabilities {
    pub has_diagnostics: bool,
    pub has_references: bool,
    pub has_definition: bool,
}

/// Severity of a diagnostic, matching the LSP integer encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum DiagnosticSeverity {
    Error = 1,
    Warning = 2,
    Information = 3,
    Hint = 4,
}

impl Serialize for DiagnosticSeverity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for DiagnosticSeverity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = i32::deserialize(deserializer)?;
        Ok(match value {
            1 => Self::Error,
            2 => Self::Warning,
            3 => Self::Information,
            _ => Self::Hint,
        })
    }
}

/// A single diagnostic reported by the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub line: u32,
    pub character: u32,
    pub message: String,
    pub severity: DiagnosticSeverity,
}

/// A location within a text document, derived from an LSP `Location.range.start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub uri: String,
    pub line: u32,
    pub character: u32,
}

/// A half-open range within a text document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRange {
    pub start_line: u32,
    pub start_char: u32,
    pub end_line: u32,
    pub end_char: u32,
}

/// A single content change for `textDocument/didChange`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextDocumentChange {
    pub range: Option<TextRange>,
    pub text: String,
}

/// Manages a single LSP server process over stdio.
pub struct LspServer {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: Arc<AtomicU64>,
    pending: Arc<DashMap<u64, oneshot::Sender<Value>>>,
    diagnostics: Arc<DashMap<String, Vec<Diagnostic>>>,
    capabilities: LspCapabilities,
}

impl LspServer {
    /// Spawn an LSP server process and send the `initialize` request.
    pub async fn spawn(
        command: &str,
        args: &[&str],
        workspace_root: &Path,
    ) -> Result<Self, LspError> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| LspError::Spawn(format!("{command}: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspError::Spawn("missing stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspError::Spawn("missing stdout".into()))?;

        let next_id = Arc::new(AtomicU64::new(0));
        let pending: Arc<DashMap<u64, oneshot::Sender<Value>>> = Arc::new(DashMap::new());
        let diagnostics: Arc<DashMap<String, Vec<Diagnostic>>> = Arc::new(DashMap::new());

        let reader_pending = pending.clone();
        let reader_diagnostics = diagnostics.clone();
        tokio::spawn(async move {
            if let Err(e) = read_loop(stdout, reader_pending, reader_diagnostics).await {
                warn!("LSP reader exited: {e}");
            }
        });

        let stdin = Arc::new(Mutex::new(stdin));
        let mut server = Self {
            child,
            stdin,
            next_id,
            pending,
            diagnostics,
            capabilities: LspCapabilities::default(),
        };
        server.initialize(workspace_root).await?;
        Ok(server)
    }

    /// Send the `initialize` request and record returned capabilities.
    pub async fn initialize(&mut self, workspace_root: &Path) -> Result<LspCapabilities, LspError> {
        let root_uri = path_to_uri(workspace_root);
        let params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "publishDiagnostics": {"relatedInformation": false}
                }
            }
        });
        let result = self.send_request("initialize", params).await?;
        let caps = result.get("capabilities").cloned().unwrap_or(Value::Null);
        let has_diagnostics = caps.get("textDocumentSync").is_some()
            || provider_enabled(caps.get("diagnosticProvider").unwrap_or(&Value::Null));
        let has_references = caps
            .get("referencesProvider")
            .map(provider_enabled)
            .unwrap_or(false);
        let has_definition = caps
            .get("definitionProvider")
            .map(provider_enabled)
            .unwrap_or(false);
        self.capabilities = LspCapabilities {
            has_diagnostics,
            has_references,
            has_definition,
        };
        self.send_notification("initialized", json!({})).await?;
        Ok(self.capabilities.clone())
    }

    /// Send a `textDocument/didOpen` notification.
    pub async fn text_document_did_open(
        &self,
        uri: &str,
        language_id: &str,
        text: &str,
    ) -> Result<(), LspError> {
        let params = json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": 1,
                "text": text,
            }
        });
        self.send_notification("textDocument/didOpen", params).await
    }

    /// Send a `textDocument/didChange` notification.
    pub async fn text_document_did_change(
        &self,
        uri: &str,
        changes: &[TextDocumentChange],
    ) -> Result<(), LspError> {
        let changes_json: Vec<Value> = changes
            .iter()
            .map(|c| {
                let range = c.range.as_ref().map(|r| {
                    json!({
                        "start": {"line": r.start_line, "character": r.start_char},
                        "end": {"line": r.end_line, "character": r.end_char},
                    })
                });
                json!({"range": range, "text": c.text})
            })
            .collect();
        let params = json!({
            "textDocument": {"uri": uri, "version": 2},
            "contentChanges": changes_json,
        });
        self.send_notification("textDocument/didChange", params)
            .await
    }

    /// Send a `textDocument/didClose` notification.
    pub async fn text_document_did_close(&self, uri: &str) -> Result<(), LspError> {
        let params = json!({"textDocument": {"uri": uri}});
        self.send_notification("textDocument/didClose", params)
            .await
    }

    /// Return the most recently published diagnostics for `uri`.
    pub async fn diagnostics(&self, uri: &str) -> Result<Vec<Diagnostic>, LspError> {
        Ok(self
            .diagnostics
            .get(uri)
            .map(|entry| entry.clone())
            .unwrap_or_default())
    }

    /// Call `textDocument/references` at the given position.
    pub async fn references(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>, LspError> {
        let params = json!({
            "textDocument": {"uri": uri},
            "position": {"line": line, "character": character},
            "context": {"includeDeclaration": true},
        });
        let result = self.send_request("textDocument/references", params).await?;
        Ok(parse_locations(&result))
    }

    /// Call `textDocument/definition` at the given position.
    pub async fn definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>, LspError> {
        let params = json!({
            "textDocument": {"uri": uri},
            "position": {"line": line, "character": character},
        });
        let result = self.send_request("textDocument/definition", params).await?;
        Ok(parse_locations(&result))
    }

    /// Send the `shutdown` request.
    pub async fn shutdown(&mut self) -> Result<(), LspError> {
        self.send_request("shutdown", Value::Null).await?;
        Ok(())
    }

    /// Send the `exit` notification and terminate the server process.
    pub async fn exit(&mut self) {
        let _ = self.send_notification("exit", Value::Null).await;
        let _ = self.child.kill().await;
    }

    async fn send_request(&self, method: &str, params: Value) -> Result<Value, LspError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&body).await?;
        let response = rx
            .await
            .map_err(|_| LspError::Protocol(format!("no response for {method} (id {id})")))?;
        if let Some(err) = response.get("error") {
            let message = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(LspError::Protocol(format!("{method}: {message}")));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), LspError> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&body).await
    }

    async fn write_message(&self, body: &Value) -> Result<(), LspError> {
        let serialized =
            serde_json::to_string(body).map_err(|e| LspError::Protocol(format!("encode: {e}")))?;
        let frame = format!("Content-Length: {}\r\n\r\n{}", serialized.len(), serialized);
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(frame.as_bytes())
            .await
            .map_err(|e| LspError::Protocol(format!("write: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| LspError::Protocol(format!("flush: {e}")))?;
        Ok(())
    }
}

/// Manages multiple LSP servers keyed by language.
pub struct LspManager {
    registered: Vec<(String, String, Vec<String>)>,
    servers: DashMap<String, Arc<Mutex<LspServer>>>,
}

impl LspManager {
    pub fn new() -> Self {
        Self {
            registered: Vec::new(),
            servers: DashMap::new(),
        }
    }

    /// Register an LSP server for a language without starting it.
    pub fn register(
        &mut self,
        language: &str,
        command: &str,
        args: &[&str],
    ) -> Result<(), LspError> {
        if self.is_registered(language) {
            return Err(LspError::Protocol(format!(
                "language already registered: {language}"
            )));
        }
        self.registered.push((
            language.to_string(),
            command.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        ));
        Ok(())
    }

    /// Returns whether a server has been registered for `language`.
    pub fn is_registered(&self, language: &str) -> bool {
        self.registered.iter().any(|(l, _, _)| l == language) || self.servers.contains_key(language)
    }

    /// Start all registered servers against `workspace_root`.
    pub async fn start(&mut self, workspace_root: &Path) -> Result<(), LspError> {
        let registered = std::mem::take(&mut self.registered);
        for (language, command, args) in registered {
            if self.servers.contains_key(&language) {
                continue;
            }
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let server = LspServer::spawn(&command, &arg_refs, workspace_root).await?;
            self.servers.insert(language, Arc::new(Mutex::new(server)));
        }
        Ok(())
    }

    /// Get diagnostics for `uri` from the server registered for `language`.
    pub async fn diagnostics(
        &self,
        uri: &str,
        language: &str,
    ) -> Result<Vec<Diagnostic>, LspError> {
        let server = self.lookup(language)?;
        let arc = server.clone();
        drop(server);
        let guard = arc.lock().await;
        guard.diagnostics(uri).await
    }

    /// Call `textDocument/references` on the server for `language`.
    pub async fn references(
        &self,
        uri: &str,
        language: &str,
        line: u32,
        char: u32,
    ) -> Result<Vec<Location>, LspError> {
        let server = self.lookup(language)?;
        let arc = server.clone();
        drop(server);
        let guard = arc.lock().await;
        guard.references(uri, line, char).await
    }

    /// Call `textDocument/definition` on the server for `language`.
    pub async fn definition(
        &self,
        uri: &str,
        language: &str,
        line: u32,
        char: u32,
    ) -> Result<Vec<Location>, LspError> {
        let server = self.lookup(language)?;
        let arc = server.clone();
        drop(server);
        let guard = arc.lock().await;
        guard.definition(uri, line, char).await
    }

    /// Shut down every running server.
    pub async fn shutdown_all(&mut self) -> Result<(), LspError> {
        let languages: Vec<String> = self.servers.iter().map(|r| r.key().clone()).collect();
        for language in languages {
            if let Some((_, arc)) = self.servers.remove(&language) {
                let mut guard = arc.lock().await;
                if let Err(e) = guard.shutdown().await {
                    warn!("LSP shutdown error for {language}: {e}");
                }
                guard.exit().await;
            }
        }
        Ok(())
    }

    fn lookup(&self, language: &str) -> Result<Arc<Mutex<LspServer>>, LspError> {
        if let Some(entry) = self.servers.get(language) {
            Ok(entry.clone())
        } else if self.is_registered(language) {
            Err(LspError::NotStarted(language.to_string()))
        } else {
            Err(LspError::UnknownLanguage(language.to_string()))
        }
    }
}

impl Default for LspManager {
    fn default() -> Self {
        Self::new()
    }
}

async fn read_loop(
    stdout: ChildStdout,
    pending: Arc<DashMap<u64, oneshot::Sender<Value>>>,
    diagnostics: Arc<DashMap<String, Vec<Diagnostic>>>,
) -> Result<(), LspError> {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .await
                .map_err(|e| LspError::Protocol(format!("read header: {e}")))?;
            if n == 0 {
                return Ok(());
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                content_length = Some(
                    rest.trim()
                        .parse::<usize>()
                        .map_err(|e| LspError::Protocol(format!("content-length: {e}")))?,
                );
            }
        }
        let len =
            content_length.ok_or_else(|| LspError::Protocol("missing Content-Length".into()))?;
        let mut buf = vec![0u8; len];
        reader
            .read_exact(&mut buf)
            .await
            .map_err(|e| LspError::Protocol(format!("read body: {e}")))?;
        let message: Value = serde_json::from_slice(&buf)
            .map_err(|e| LspError::Protocol(format!("parse body: {e}")))?;
        dispatch_message(message, &pending, &diagnostics);
    }
}

fn dispatch_message(
    message: Value,
    pending: &DashMap<u64, oneshot::Sender<Value>>,
    diagnostics: &DashMap<String, Vec<Diagnostic>>,
) {
    if message.get("id").is_some() {
        if let Some(id) = message.get("id").and_then(|v| v.as_u64()) {
            if let Some((_, sender)) = pending.remove(&id) {
                let _ = sender.send(message);
            }
        }
        return;
    }
    if message.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics") {
        if let Some(params) = message.get("params") {
            let uri = params
                .get("uri")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();
            let parsed = parse_diagnostics(params);
            diagnostics.insert(uri, parsed);
        }
    }
}

fn parse_diagnostics(params: &Value) -> Vec<Diagnostic> {
    params
        .get("diagnostics")
        .and_then(|d| d.as_array())
        .map(|arr| arr.iter().filter_map(parse_diagnostic).collect())
        .unwrap_or_default()
}

fn parse_diagnostic(value: &Value) -> Option<Diagnostic> {
    let range = value.get("range")?;
    let start = range.get("start")?;
    let line = start.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as u32;
    let character = start.get("character").and_then(|c| c.as_u64()).unwrap_or(0) as u32;
    let message = value
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let severity = value
        .get("severity")
        .and_then(|s| s.as_i64())
        .map(|i| match i {
            1 => DiagnosticSeverity::Error,
            2 => DiagnosticSeverity::Warning,
            3 => DiagnosticSeverity::Information,
            _ => DiagnosticSeverity::Hint,
        })
        .unwrap_or(DiagnosticSeverity::Error);
    Some(Diagnostic {
        line,
        character,
        message,
        severity,
    })
}

fn parse_locations(result: &Value) -> Vec<Location> {
    let items: Vec<Value> = match result {
        Value::Null => Vec::new(),
        Value::Array(arr) => arr.clone(),
        other => vec![other.clone()],
    };
    items.iter().filter_map(parse_location).collect()
}

fn parse_location(value: &Value) -> Option<Location> {
    let uri = value.get("uri")?.as_str()?.to_string();
    let range = value.get("range")?;
    let start = range.get("start")?;
    let line = start.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as u32;
    let character = start.get("character").and_then(|c| c.as_u64()).unwrap_or(0) as u32;
    Some(Location {
        uri,
        line,
        character,
    })
}

fn provider_enabled(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Object(_) => true,
        _ => false,
    }
}

fn path_to_uri(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    format!("file://{}", absolute.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_registration() {
        let mut manager = LspManager::new();
        assert!(!manager.is_registered("rust"));
        manager.register("rust", "rust-analyzer", &[]).unwrap();
        assert!(manager.is_registered("rust"));
    }

    #[test]
    fn manager_duplicate_registration_errors() {
        let mut manager = LspManager::new();
        manager.register("rust", "rust-analyzer", &[]).unwrap();
        let result = manager.register("rust", "rust-analyzer", &[]);
        assert!(matches!(result, Err(LspError::Protocol(_))));
    }

    #[tokio::test]
    async fn manager_unknown_language() {
        let manager = LspManager::new();
        let result = manager.diagnostics("file:///x.rs", "python").await;
        assert!(matches!(result, Err(LspError::UnknownLanguage(_))));
    }

    #[tokio::test]
    async fn manager_registered_but_not_started() {
        let mut manager = LspManager::new();
        manager.register("rust", "rust-analyzer", &[]).unwrap();
        let result = manager.diagnostics("file:///x.rs", "rust").await;
        assert!(matches!(result, Err(LspError::NotStarted(_))));
    }

    #[test]
    fn diagnostic_severity_serialization() {
        assert_eq!(
            serde_json::to_string(&DiagnosticSeverity::Error).unwrap(),
            "1"
        );
        let warning: DiagnosticSeverity = serde_json::from_str("2").unwrap();
        assert_eq!(warning, DiagnosticSeverity::Warning);
        let hint: DiagnosticSeverity = serde_json::from_str("9").unwrap();
        assert_eq!(hint, DiagnosticSeverity::Hint);
    }

    #[test]
    fn location_serialization() {
        let location = Location {
            uri: "file:///a.rs".into(),
            line: 3,
            character: 5,
        };
        let serialized = serde_json::to_string(&location).unwrap();
        let back: Location = serde_json::from_str(&serialized).unwrap();
        assert_eq!(back, location);
    }

    #[test]
    fn text_document_change_construction() {
        let range = TextRange {
            start_line: 0,
            start_char: 0,
            end_line: 0,
            end_char: 5,
        };
        let change = TextDocumentChange {
            range: Some(range),
            text: "hello".into(),
        };
        let serialized = serde_json::to_string(&change).unwrap();
        assert!(serialized.contains("hello"));
        let back: TextDocumentChange = serde_json::from_str(&serialized).unwrap();
        assert_eq!(back.text, "hello");
        assert!(back.range.is_some());
    }

    #[test]
    fn parse_locations_handles_variants() {
        assert!(parse_locations(&Value::Null).is_empty());
        let single = json!({
            "uri": "file:///a.rs",
            "range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 4}}
        });
        let locs = parse_locations(&single);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].line, 1);
        assert_eq!(locs[0].character, 2);
        let arr = json!([single, single]);
        assert_eq!(parse_locations(&arr).len(), 2);
    }

    #[test]
    fn parse_diagnostics_from_params() {
        let params = json!({
            "uri": "file:///a.rs",
            "diagnostics": [
                {"range": {"start": {"line": 4, "character": 7}, "end": {"line": 4, "character": 8}},
                 "message": "unused", "severity": 2}
            ]
        });
        let diags = parse_diagnostics(&params);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 4);
        assert_eq!(diags[0].character, 7);
        assert_eq!(diags[0].severity, DiagnosticSeverity::Warning);
    }

    #[test]
    fn provider_enabled_truthiness() {
        assert!(provider_enabled(&json!(true)));
        assert!(!provider_enabled(&json!(false)));
        assert!(provider_enabled(&json!({"options": {}})));
        assert!(!provider_enabled(&Value::Null));
    }
}
