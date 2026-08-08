//! JSON-RPC IPC server over Unix socket.

use crate::agent::{Agent, ToolRegistry};
use crate::plugin::PluginRegistry;
use crate::session::Session;
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, warn};

pub struct IpcServer {
    pub socket_path: String,
    pub agent: Arc<AsyncMutex<Agent>>,
    pub tools: Arc<Mutex<ToolRegistry>>,
    pub plugins: Arc<Mutex<PluginRegistry>>,
    pub session: Arc<Mutex<Session>>,
    pub token_hash: Option<Vec<u8>>,
}

impl Clone for IpcServer {
    fn clone(&self) -> Self {
        Self {
            socket_path: self.socket_path.clone(),
            agent: self.agent.clone(),
            tools: self.tools.clone(),
            plugins: self.plugins.clone(),
            session: self.session.clone(),
            token_hash: self.token_hash.clone(),
        }
    }
}

fn token_hash_from_env() -> Option<Vec<u8>> {
    static HASH: std::sync::OnceLock<Option<Vec<u8>>> = std::sync::OnceLock::new();
    HASH.get_or_init(|| {
        std::env::var("RX4_IPC_TOKEN")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|t| {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(t.as_bytes());
                hasher.finalize().to_vec()
            })
    })
    .clone()
}

impl IpcServer {
    pub fn new(socket_path: impl Into<String>) -> Self {
        Self {
            socket_path: socket_path.into(),
            agent: Arc::new(AsyncMutex::new(Agent::new())),
            tools: Arc::new(Mutex::new(ToolRegistry::new())),
            plugins: Arc::new(Mutex::new(PluginRegistry::new())),
            session: Arc::new(Mutex::new(Session::new("default", "default"))),
            token_hash: token_hash_from_env(),
        }
    }

    pub fn attach_agent(&self, agent: Agent) {
        // This synchronous setup method is intended to be called before the
        // server starts. `try_lock` makes that contract explicit instead of
        // silently racing a request that may observe the default agent.
        let mut guard = self
            .agent
            .try_lock()
            .expect("attach_agent must run before IPC requests are active");
        *guard = agent;
    }

    pub async fn attach_agent_async(&self, agent: Agent) {
        *self.agent.lock().await = agent;
    }

    pub fn attach_tools(&self, tools: ToolRegistry) {
        *self.tools.lock().unwrap() = tools;
    }

    pub fn attach_plugins(&self, plugins: PluginRegistry) {
        *self.plugins.lock().unwrap() = plugins;
    }

    pub fn attach_session(&self, session: Session) {
        *self.session.lock().unwrap() = session;
    }

    /// Run the IPC server on the current Tokio runtime.
    pub async fn run_async(&self) -> std::io::Result<()> {
        let path = Path::new(&self.socket_path);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let listener = tokio::net::UnixListener::bind(path)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        info!("IPC server listening on {}", self.socket_path);

        let this = self.clone();
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let s = this.clone();
                    tokio::spawn(async move {
                        if let Err(e) = s.handle_connection(stream).await {
                            warn!("connection error: {e}");
                        }
                    });
                }
                Err(e) => warn!("accept error: {e}"),
            }
        }
    }

    /// Blocking compatibility wrapper for hosts that run `serve` from a
    /// synchronous entry point. Async hosts should call [`Self::run_async`].
    pub fn run(&self) -> std::io::Result<()> {
        tokio::runtime::Runtime::new()?.block_on(self.run_async())
    }

    async fn handle_connection(&self, stream: tokio::net::UnixStream) -> std::io::Result<()> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await? {
            if line.is_empty() {
                continue;
            }
            let response = self.handle_request(&line).await;
            writer.write_all(response.as_bytes()).await?;
            writer.write_all(b"\n").await?;
        }
        Ok(())
    }

    async fn handle_request(&self, line: &str) -> String {
        self.handle_request_with_token(line, self.token_hash.as_deref())
            .await
    }

    async fn handle_request_with_token(
        &self,
        line: &str,
        required_token_hash: Option<&[u8]>,
    ) -> String {
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return error_response(None, -32700, &format!("parse error: {e}")),
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        let provided = params.get("token").and_then(|t| t.as_str()).unwrap_or("");
        let mutating = matches!(
            method,
            "prompt"
                | "set_scope"
                | "set_policy"
                | "set_approver"
                | "clear_authorizer"
                | "set_model"
                | "cancel"
                | "reset"
                | "load_session"
                | "save_session"
                | "session_clear"
        );
        if method != "ping" {
            match required_token_hash {
                Some(token_hash) => {
                    use sha2::{Digest, Sha256};
                    let mut hasher = Sha256::new();
                    hasher.update(provided.as_bytes());
                    let provided_hash = hasher.finalize();

                    if !bool::from(provided_hash.as_slice().ct_eq(token_hash)) {
                        return error_response(id, -32000, "invalid or missing token");
                    }
                }
                None if mutating => {
                    return error_response(
                        id,
                        -32000,
                        "RX4_IPC_TOKEN required for mutating IPC methods",
                    );
                }
                _ => {}
            }
        }

        let result: Result<Value, String> = self.execute_method(method, &params).await;

        match result {
            Ok(value) => {
                serde_json::json!({"jsonrpc": "2.0", "id": id, "result": value}).to_string()
            }
            Err(e) => {
                let code = if e.starts_with("unknown method:") {
                    -32601
                } else {
                    -32603
                };
                error_response(id, code, &e)
            }
        }
    }

    async fn execute_method(&self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "ping" => Ok(Value::String("pong".into())),
            "state" => {
                let agent = self.agent.lock().await;
                Ok(serde_json::json!({
                    "model": agent.model,
                    "scope": agent.scope.name(),
                    "policy_mode": format!("{:?}", agent.policy.mode),
                    "shell_allow": agent.policy.shell_allow.len(),
                    "shell_deny": agent.policy.shell_deny.len(),
                    "has_approver": agent.approver.is_some(),
                    "has_authorizer": agent.authorizer.is_some(),
                    "tools": self.tools.lock().unwrap().count(),
                    "plugins": self.plugins.lock().unwrap().count(),
                }))
            }
            "tools" => Ok(Value::Array(self.tools.lock().unwrap().definitions())),
            "plugins" => {
                let p = self.plugins.lock().unwrap();
                Ok(Value::Array(
                    p.plugins
                        .iter()
                        .map(|pl| serde_json::json!({"id": pl.id, "name": pl.name}))
                        .collect::<Vec<_>>(),
                ))
            }
            "messages" => {
                let agent = self.agent.lock().await;
                let msgs = agent.messages.read();
                Ok(Value::Array(
                    msgs.iter()
                        .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
                        .collect::<Vec<_>>(),
                ))
            }
            "set_model" => {
                let model = params
                    .get("model")
                    .and_then(|m| m.as_str())
                    .unwrap_or("gpt-4o");
                self.agent.lock().await.set_model(model);
                Ok(Value::String(format!("model set to {model}")))
            }
            "set_scope" => {
                if let Some(name) = params.get("scope").and_then(|s| s.as_str()) {
                    if let Some(scope) = crate::mode::Scope::parse_scope(name) {
                        self.agent.lock().await.set_scope(scope);
                        Ok(Value::String(format!("scope set to {scope}")))
                    } else {
                        Err(format!("unknown scope: {name}"))
                    }
                } else {
                    Err("missing scope".into())
                }
            }
            "get_policy" => {
                let agent = self.agent.lock().await;
                serde_json::to_value(&agent.policy).map_err(|e| e.to_string())
            }
            "set_policy" => {
                if let Some(raw) = params.get("policy").cloned() {
                    match serde_json::from_value::<crate::permissions::Policy>(raw) {
                        Ok(policy) => {
                            self.agent.lock().await.set_policy(policy);
                            Ok(Value::String("policy set".into()))
                        }
                        Err(e) => Err(e.to_string()),
                    }
                } else {
                    Err("missing policy".into())
                }
            }
            "set_approver" => {
                // Host product Approver stays in-process; IPC only offers always_allow / always_deny.
                let mode = params
                    .get("mode")
                    .and_then(|m| m.as_str())
                    .unwrap_or("always_deny");
                let mut agent = self.agent.lock().await;
                match mode {
                    "always_allow" | "allow" => {
                        agent.set_approver(std::sync::Arc::new(crate::permissions::AlwaysAllow));
                        Ok(Value::String(format!("approver set to {mode}")))
                    }
                    "always_deny" | "deny" => {
                        agent.set_approver(std::sync::Arc::new(crate::permissions::AlwaysDeny));
                        Ok(Value::String(format!("approver set to {mode}")))
                    }
                    "clear" | "none" => {
                        agent.approver = None;
                        Ok(Value::String("approver cleared".into()))
                    }
                    other => Err(format!("unknown approver mode: {other}")),
                }
            }
            "clear_authorizer" => {
                self.agent.lock().await.clear_authorizer();
                Ok(Value::String("authorizer cleared".into()))
            }
            "prompt" => {
                let text = params
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                if text.is_empty() {
                    return Err("missing prompt text".into());
                }
                let mut agent = self.agent.lock().await;
                agent
                    .prompt(&text)
                    .await
                    .map_err(|e| format!("prompt failed: {e}"))?;
                Ok(Value::String("prompt completed".into()))
            }
            "session_list" => {
                let s = self.session.lock().unwrap();
                Ok(serde_json::json!({"id": s.id, "entries": s.entries.len()}))
            }
            "session_clear" => {
                self.session.lock().unwrap().entries.clear();
                self.agent.lock().await.clear_messages();
                Ok(Value::String("cleared".into()))
            }
            _ => Err(format!("unknown method: {method}")),
        }
    }
}

fn error_response(id: Option<Value>, code: i32, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;

    #[tokio::test]
    async fn test_attach_agent() {
        let server = IpcServer::new("/tmp/test_ipc_socket");
        let mut new_agent = Agent::new();
        new_agent.model = "test-model-abc".to_string();

        server.attach_agent(new_agent);

        // Yield to let the spawned task execute
        tokio::task::yield_now().await;
        // Just in case it needs a tiny bit of time
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let agent_lock = server.agent.lock().await;
        assert_eq!(agent_lock.model, "test-model-abc");
    }

    #[tokio::test]
    async fn mutating_requests_require_a_token_when_unconfigured() {
        let server = IpcServer::new("/tmp/test_ipc_auth_socket");
        let response = server
            .handle_request_with_token(
                r#"{"jsonrpc":"2.0","id":1,"method":"set_model","params":{"model":"unsafe"}}"#,
                None,
            )
            .await;
        assert!(response.contains("RX4_IPC_TOKEN required"), "{response}");

        let ping = server
            .handle_request_with_token(
                r#"{"jsonrpc":"2.0","id":2,"method":"ping","params":{}}"#,
                None,
            )
            .await;
        assert!(ping.contains("pong"), "{ping}");
    }

    #[tokio::test]
    async fn configured_ipc_token_authenticates_all_non_ping_methods() {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"secret");
        let secret_hash = hasher.finalize();

        let server = IpcServer::new("/tmp/test_ipc_auth_socket_2");
        let denied = server
            .handle_request_with_token(
                r#"{"jsonrpc":"2.0","id":1,"method":"state","params":{}}"#,
                Some(secret_hash.as_slice()),
            )
            .await;
        assert!(denied.contains("invalid or missing token"), "{denied}");

        let allowed = server
            .handle_request_with_token(
                r#"{"jsonrpc":"2.0","id":2,"method":"state","params":{"token":"secret"}}"#,
                Some(secret_hash.as_slice()),
            )
            .await;
        assert!(allowed.contains("policy_mode"), "{allowed}");
    }
}
