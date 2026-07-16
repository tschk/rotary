//! Pi SDK surface — createAgentSession, AgentSessionHandle.
//!
//! Compatible with pi_agent_rust SDK:
//! ```ignore
//! use rx4::pi::sdk::{create_agent_session, AgentSessionOptions};
//!
//! # #[tokio::main]
//! # async fn main() {
//! let handle = create_agent_session(AgentSessionOptions::default());
//! handle.prompt("hello", |event| { /* ... */ }).await;
//! # }
//! ```

use crate::agent::{Agent, Event};
use crate::provider::Message;
use parking_lot::Mutex as SyncMutex;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

/// Transport for the session (pi pattern).
#[derive(Debug, Clone, Default)]
pub enum SessionTransport {
    /// Direct in-process embedding.
    #[default]
    InProcess,
    /// RPC subprocess (spawns a child process running `rx4 --mode rpc`).
    RpcSubprocess { command: String },
}

/// Options for creating an agent session (pi SDK).
#[derive(Debug, Clone)]
pub struct AgentSessionOptions {
    pub model: String,
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub scope: String,
    pub workspace_root: Option<std::path::PathBuf>,
    pub max_tool_iterations: usize,
    pub auto_compact_after: usize,
    pub transport: SessionTransport,
}

impl Default for AgentSessionOptions {
    fn default() -> Self {
        Self {
            model: "gpt-4o".into(),
            provider: None,
            api_key: None,
            scope: "coding".into(),
            workspace_root: None,
            max_tool_iterations: 50,
            auto_compact_after: 80,
            transport: SessionTransport::default(),
        }
    }
}

/// Event listener callback type.
pub type EventListener = Arc<dyn Fn(&Event) + Send + Sync>;

/// Handle to an agent session — pi SDK surface.
/// Uses tokio::sync::Mutex for the agent (async-safe across .await points)
/// and parking_lot::Mutex for the listeners (sync, never held across await).
pub struct AgentSessionHandle {
    agent: Arc<Mutex<Agent>>,
    listeners: SyncMutex<Vec<EventListener>>,
    transport: SessionTransport,
}

impl AgentSessionHandle {
    pub fn new(agent: Agent, transport: SessionTransport) -> Self {
        Self {
            agent: Arc::new(Mutex::new(agent)),
            listeners: SyncMutex::new(Vec::new()),
            transport,
        }
    }

    /// Subscribe to events from the session.
    pub fn subscribe(&self, listener: impl Fn(&Event) + Send + Sync + 'static) {
        self.listeners.lock().push(Arc::new(listener));
    }

    /// Send a prompt to the agent.
    pub async fn prompt(&self, text: &str, _on_event: impl Fn(&Event)) -> Result<(), SdkError> {
        let listeners: Vec<EventListener> = self.listeners.lock().clone();

        {
            let mut a = self.agent.lock().await;
            for listener in listeners {
                let l = listener;
                a.subscribe(move |e| l(e));
            }
        }

        let result = self.agent.lock().await.prompt(text).await;
        result.map_err(|e| SdkError::Agent(e.to_string()))
    }

    /// Set the model for the session.
    pub async fn set_model(&self, provider: &str, model: &str) {
        let _ = provider;
        let mut a = self.agent.lock().await;
        a.set_model(model);
    }

    /// Trigger context compaction.
    pub async fn compact(&self) {
        let a = self.agent.lock().await;
        a.compact("sdk compact");
    }

    /// Get the current model.
    pub async fn model(&self) -> String {
        self.agent.lock().await.model.clone()
    }

    /// Get the current message count.
    pub async fn message_count(&self) -> usize {
        self.agent.lock().await.message_count()
    }

    /// Get all messages.
    pub async fn messages(&self) -> Vec<Message> {
        self.agent.lock().await.messages.read().clone()
    }

    /// Clear all messages.
    pub async fn clear(&self) {
        self.agent.lock().await.clear_messages();
    }

    /// Abort the current operation (best-effort).
    pub fn abort(&self) {
        info!("abort requested via SDK");
    }

    /// Get the transport type.
    pub fn transport(&self) -> &SessionTransport {
        &self.transport
    }
}

impl Clone for AgentSessionHandle {
    fn clone(&self) -> Self {
        Self {
            agent: self.agent.clone(),
            listeners: SyncMutex::new(self.listeners.lock().clone()),
            transport: self.transport.clone(),
        }
    }
}

/// Create an agent session (pi SDK entry point).
pub fn create_agent_session(options: AgentSessionOptions) -> AgentSessionHandle {
    let mut agent = Agent::new();
    agent.set_model(&options.model);
    agent.max_tool_iterations = options.max_tool_iterations;
    agent.auto_compact_after = options.auto_compact_after;

    if let Some(workspace) = &options.workspace_root {
        agent.set_workspace_root(workspace.clone());
    }

    if let Some(scope) = crate::mode::Scope::parse_scope(&options.scope) {
        agent.set_scope(scope);
    }

    crate::tools::register_builtin_tools(&mut agent.tools);

    #[cfg(feature = "providers")]
    if let Some(api_key) = &options.api_key {
        let provider: Arc<dyn crate::provider::Provider> = match options.provider.as_deref() {
            Some("anthropic") | Some("claude") => {
                Arc::new(crate::provider::OpenAIProvider::anthropic(api_key))
            }
            Some("ollama") | Some("local") => Arc::new(crate::provider::OpenAIProvider::ollama()),
            _ => Arc::new(crate::provider::OpenAIProvider::new(api_key)),
        };
        agent.set_provider(provider);
    }

    info!(
        "created agent session: model={}, scope={}",
        options.model, options.scope
    );
    AgentSessionHandle::new(agent, options.transport)
}

#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    #[error("agent error: {0}")]
    Agent(String),
    #[error("transport error: {0}")]
    Transport(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_session_defaults() {
        let handle = create_agent_session(AgentSessionOptions::default());
        assert_eq!(handle.model().await, "gpt-4o");
        assert_eq!(handle.message_count().await, 0);
        assert!(matches!(handle.transport(), SessionTransport::InProcess));
    }

    #[tokio::test]
    async fn create_session_custom() {
        let handle = create_agent_session(AgentSessionOptions {
            model: "claude-3-opus".into(),
            scope: "research".into(),
            ..Default::default()
        });
        assert_eq!(handle.model().await, "claude-3-opus");
    }

    #[tokio::test]
    async fn session_handle_clone() {
        let handle = create_agent_session(AgentSessionOptions::default());
        let cloned = handle.clone();
        assert_eq!(handle.model().await, cloned.model().await);
    }
}
