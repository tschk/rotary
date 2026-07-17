//! ACP (Agent Client Protocol) host — JSON-RPC session surface over an embedded agent.
//!
//! Hosts drive the agent via methods:
//! - `initialize` — capability handshake
//! - `session/new` — create a session id
//! - `session/prompt` — run a user prompt on the attached agent
//! - `session/cancel` — mark a session cancelled
//! - `session/list` — list known sessions

use crate::agent::Agent;
use crate::provider::Role;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

/// Session record tracked by the ACP host.
#[derive(Debug, Clone)]
pub struct AcpSession {
    pub id: String,
    pub cancelled: bool,
    pub turns: usize,
}

/// ACP host wrapping a shared agent.
pub struct AcpHost {
    pub running: bool,
    agent: Arc<tokio::sync::Mutex<Agent>>,
    sessions: Mutex<HashMap<String, AcpSession>>,
    protocol_version: u32,
}

impl AcpHost {
    /// Create a host with a fresh agent.
    pub fn new() -> Self {
        Self {
            running: false,
            agent: Arc::new(tokio::sync::Mutex::new(Agent::new())),
            sessions: Mutex::new(HashMap::new()),
            protocol_version: 1,
        }
    }

    /// Create a host around an existing agent.
    pub fn with_agent(agent: Agent) -> Self {
        Self {
            running: false,
            agent: Arc::new(tokio::sync::Mutex::new(agent)),
            sessions: Mutex::new(HashMap::new()),
            protocol_version: 1,
        }
    }

    /// Shared agent handle for hosts that need direct access.
    pub fn agent(&self) -> Arc<tokio::sync::Mutex<Agent>> {
        self.agent.clone()
    }

    pub fn start(&mut self) {
        self.running = true;
    }

    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Handle one JSON-RPC request line and return a JSON-RPC response object.
    pub async fn handle_request(&self, request: &Value) -> Value {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => ok_response(
                id,
                json!({
                    "protocolVersion": self.protocol_version,
                    "serverInfo": {
                        "name": "rx4",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": {
                        "prompt": true,
                        "tools": true,
                        "sessions": true,
                    }
                }),
            ),
            "session/new" => {
                let sid = Uuid::new_v4().to_string();
                self.sessions.lock().insert(
                    sid.clone(),
                    AcpSession {
                        id: sid.clone(),
                        cancelled: false,
                        turns: 0,
                    },
                );
                ok_response(id, json!({ "sessionId": sid }))
            }
            "session/list" => {
                let sessions: Vec<Value> = self
                    .sessions
                    .lock()
                    .values()
                    .map(|s| {
                        json!({
                            "sessionId": s.id,
                            "cancelled": s.cancelled,
                            "turns": s.turns,
                        })
                    })
                    .collect();
                ok_response(id, json!({ "sessions": sessions }))
            }
            "session/cancel" => {
                let sid = params
                    .get("sessionId")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                let mut sessions = self.sessions.lock();
                if let Some(s) = sessions.get_mut(sid) {
                    s.cancelled = true;
                    ok_response(id, json!({ "cancelled": true }))
                } else {
                    error_response(id, -32001, &format!("unknown session: {sid}"))
                }
            }
            "session/prompt" => {
                let sid = params
                    .get("sessionId")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                let prompt = params
                    .get("prompt")
                    .or_else(|| params.get("text"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                if prompt.is_empty() {
                    return error_response(id, -32602, "prompt required");
                }
                {
                    let mut sessions = self.sessions.lock();
                    let Some(session) = sessions.get_mut(sid) else {
                        return error_response(id, -32001, &format!("unknown session: {sid}"));
                    };
                    if session.cancelled {
                        return error_response(id, -32002, "session cancelled");
                    }
                    session.turns += 1;
                }

                let mut agent = self.agent.lock().await;
                match agent.prompt(prompt).await {
                    Ok(()) => {
                        let msgs = agent.messages.read().clone();
                        let content = msgs
                            .iter()
                            .rev()
                            .find(|m| m.role == Role::Assistant)
                            .map(|m| m.content.clone())
                            .unwrap_or_default();
                        ok_response(
                            id,
                            json!({
                                "sessionId": sid,
                                "content": content,
                                "messageCount": msgs.len(),
                            }),
                        )
                    }
                    Err(e) => error_response(id, -32000, &e.to_string()),
                }
            }
            "" => error_response(id, -32600, "method required"),
            other => error_response(id, -32601, &format!("method not found: {other}")),
        }
    }

    /// Convenience: parse a JSON string request.
    pub async fn handle_line(&self, line: &str) -> String {
        match serde_json::from_str::<Value>(line) {
            Ok(req) => self.handle_request(&req).await.to_string(),
            Err(e) => error_response(Value::Null, -32700, &format!("parse error: {e}")).to_string(),
        }
    }
}

impl Default for AcpHost {
    fn default() -> Self {
        Self::new()
    }
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initialize_and_session_new() {
        let host = AcpHost::new();
        let init = host
            .handle_request(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"}))
            .await;
        assert!(init.get("result").is_some());
        let created = host
            .handle_request(&json!({"jsonrpc":"2.0","id":2,"method":"session/new"}))
            .await;
        let sid = created["result"]["sessionId"].as_str().unwrap();
        assert!(!sid.is_empty());
    }

    #[tokio::test]
    async fn prompt_without_provider_errors() {
        let host = AcpHost::new();
        let created = host
            .handle_request(&json!({"jsonrpc":"2.0","id":1,"method":"session/new"}))
            .await;
        let sid = created["result"]["sessionId"].as_str().unwrap().to_string();
        let resp = host
            .handle_request(&json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"session/prompt",
                "params":{"sessionId": sid, "prompt":"hi"}
            }))
            .await;
        assert!(resp.get("error").is_some());
    }

    #[tokio::test]
    async fn cancel_unknown_session() {
        let host = AcpHost::new();
        let resp = host
            .handle_request(&json!({
                "jsonrpc":"2.0","id":1,
                "method":"session/cancel",
                "params":{"sessionId":"nope"}
            }))
            .await;
        assert_eq!(resp["error"]["code"], -32001);
    }
}
