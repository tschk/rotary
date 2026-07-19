//! JSON-RPC IPC server over Unix socket.

use crate::agent::{Agent, ToolRegistry};
use crate::plugin::PluginRegistry;
use crate::session::Session;
use serde_json::Value;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, warn};

pub struct IpcServer {
    pub socket_path: String,
    pub agent: Arc<AsyncMutex<Agent>>,
    pub tools: Arc<Mutex<ToolRegistry>>,
    pub plugins: Arc<Mutex<PluginRegistry>>,
    pub session: Arc<Mutex<Session>>,
}

impl Clone for IpcServer {
    fn clone(&self) -> Self {
        Self {
            socket_path: self.socket_path.clone(),
            agent: self.agent.clone(),
            tools: self.tools.clone(),
            plugins: self.plugins.clone(),
            session: self.session.clone(),
        }
    }
}

impl IpcServer {
    pub fn new(socket_path: impl Into<String>) -> Self {
        Self {
            socket_path: socket_path.into(),
            agent: Arc::new(AsyncMutex::new(Agent::new())),
            tools: Arc::new(Mutex::new(ToolRegistry::new())),
            plugins: Arc::new(Mutex::new(PluginRegistry::new())),
            session: Arc::new(Mutex::new(Session::new("default", "default"))),
        }
    }

    pub fn attach_agent(&self, agent: Agent) {
        let agent_arc = self.agent.clone();
        tokio::spawn(async move {
            *agent_arc.lock().await = agent;
        });
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

    pub fn run(&self) -> std::io::Result<()> {
        let path = Path::new(&self.socket_path);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        info!("IPC server listening on {}", self.socket_path);

        let runtime = tokio::runtime::Handle::try_current().map_err(std::io::Error::other)?;
        let this = self.clone();
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let s = this.clone();
                    runtime.spawn(async move {
                        if let Err(e) = s.handle_connection(stream).await {
                            warn!("connection error: {e}");
                        }
                    });
                }
                Err(e) => warn!("accept error: {e}"),
            }
        }
        Ok(())
    }

    async fn handle_connection(
        &self,
        mut stream: std::os::unix::net::UnixStream,
    ) -> std::io::Result<()> {
        use std::io::{BufRead, BufReader, Write};
        let reader = BufReader::new(stream.try_clone()?);
        for line in reader.lines() {
            let line = line?;
            if line.is_empty() {
                continue;
            }
            let response = self.handle_request(&line).await;
            writeln!(stream, "{response}")?;
        }
        Ok(())
    }

    async fn handle_request(&self, line: &str) -> String {
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return error_response(None, -32700, &format!("parse error: {e}")),
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        if method != "ping" {
            if let Ok(token) = std::env::var("RX4_IPC_TOKEN") {
                if !token.is_empty() {
                    let provided = params.get("token").and_then(|t| t.as_str()).unwrap_or("");
                    if provided != token {
                        return error_response(id, -32000, "invalid or missing token");
                    }
                }
            }
        }

        let result: Result<Value, String> = match method {
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
                    .unwrap_or("always_allow");
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
                let agent = self.agent.clone();
                tokio::spawn(async move {
                    let mut a = agent.lock().await;
                    let _ = a.prompt(&text).await;
                });
                Ok(Value::String("prompt accepted".into()))
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
            _ => return error_response(id, -32601, &format!("unknown method: {method}")),
        };

        match result {
            Ok(value) => {
                serde_json::json!({"jsonrpc": "2.0", "id": id, "result": value}).to_string()
            }
            Err(e) => error_response(id, -32603, &e),
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
