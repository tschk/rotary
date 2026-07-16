//! Agent loop: event-driven turn cycling with tool execution, permissions, scopes,
//! cancellation, caching, and parallel tool dispatch.
//!
//! Architecture informed by codex-rs (turn-based loop with CancellationToken),
//! grok-build (moka cache, dashmap registry, parking_lot), and pi_agent_rust
//! (stable event ordering, bounded tool recursion).

use crate::hooks::HookRegistry;
use crate::mode::{self, Profile, Scope};
use crate::permissions::{self, Approver, Decision, Policy};
use crate::provider::{Message, Provider, Role};
use moka::future::Cache;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

#[cfg(feature = "ipc")]
use cancellation_token::CancellationToken;

pub type ToolFuture = Pin<Box<dyn Future<Output = ToolResult> + Send>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub id: String,
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            is_error: false,
        }
    }
    pub fn err(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

/// Context passed to tool execution — provides workspace root, cancellation, etc.
pub struct ToolContext {
    pub workspace_root: std::path::PathBuf,
    #[cfg(feature = "ipc")]
    pub cancellation: CancellationToken,
}

impl ToolContext {
    pub fn new(workspace_root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            #[cfg(feature = "ipc")]
            cancellation: CancellationToken::new(false),
        }
    }
}

/// Trait-based tool execution (pi_agent_rust pattern).
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_json(&self) -> &str;
    async fn execute(&self, ctx: &ToolContext, arguments: &str) -> ToolResult;
}

/// Function-pointer tool (for simple builtins).
pub type ToolExecuteFn = fn(Arc<ToolContext>, String) -> ToolFuture;

pub enum ToolEntry {
    Trait(Box<dyn ToolExecutor>),
    Fn(ToolExecuteFn),
}

pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters_json: String,
    pub entry: ToolEntry,
}

impl ToolDefinition {
    pub fn new_fn(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_json: impl Into<String>,
        execute: ToolExecuteFn,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters_json: parameters_json.into(),
            entry: ToolEntry::Fn(execute),
        }
    }

    pub fn new_trait(executor: Box<dyn ToolExecutor>) -> Self {
        Self {
            name: executor.name().to_string(),
            description: executor.description().to_string(),
            parameters_json: executor.parameters_json().to_string(),
            entry: ToolEntry::Trait(executor),
        }
    }
}

/// Concurrent tool registry using dashmap (grok pattern).
pub struct ToolRegistry {
    tools: dashmap::DashMap<String, ToolDefinition>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: dashmap::DashMap::new(),
        }
    }

    pub fn register(&mut self, tool: ToolDefinition) {
        info!("registered tool: {}", tool.name);
        self.tools.insert(tool.name.clone(), tool);
    }

    pub fn get(&self, name: &str) -> Option<ToolEntry> {
        self.tools.get(name).map(|e| match &e.entry {
            ToolEntry::Trait(_) => ToolEntry::Trait(Box::new(FnAdapter(e.parameters_json.clone()))),
            ToolEntry::Fn(f) => ToolEntry::Fn(*f),
        })
    }

    pub fn count(&self) -> usize {
        self.tools.len()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|e| e.key().clone()).collect()
    }

    pub fn definitions(&self) -> Vec<serde_json::Value> {
        self.tools.iter().map(|t| serde_json::json!({
            "name": t.name,
            "description": t.description,
            "parameters": serde_json::from_str::<serde_json::Value>(&t.parameters_json).unwrap_or(serde_json::Value::Null),
        })).collect()
    }

    pub async fn execute(
        &self,
        name: &str,
        ctx: &Arc<ToolContext>,
        arguments: &str,
    ) -> Option<ToolResult> {
        let entry = self.tools.get(name)?;
        match &entry.entry {
            ToolEntry::Trait(executor) => Some(executor.execute(ctx, arguments).await),
            ToolEntry::Fn(f) => Some((f)(ctx.clone(), arguments.to_string()).await),
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

struct FnAdapter(String);
#[async_trait::async_trait]
impl ToolExecutor for FnAdapter {
    fn name(&self) -> &str {
        "fn_adapter"
    }
    fn description(&self) -> &str {
        ""
    }
    fn parameters_json(&self) -> &str {
        &self.0
    }
    async fn execute(&self, _ctx: &ToolContext, _arguments: &str) -> ToolResult {
        ToolResult::err("fn_adapter", "not directly executable")
    }
}

/// Stable event ordering (pi_agent_rust pattern).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum Event {
    AgentStart,
    TurnStart { turn: usize },
    MessageStart { role: Role },
    MessageDelta { delta: String },
    MessageEnd { role: Role, content: String },
    ToolCall(ToolCall),
    ToolExecutionStart(ToolCall),
    ToolExecutionEnd(ToolResult),
    TurnEnd { turn: usize },
    AgentEnd,
    Error(String),
}

pub type Subscriber = Arc<dyn Fn(&Event) + Send + Sync>;

/// The agent — owns the loop, tools, provider, policy, scope, hooks, cache.
pub struct Agent {
    pub model: String,
    pub system_prompt: Option<String>,
    pub tools: ToolRegistry,
    pub policy: Policy,
    pub scope: Scope,
    scope_profile: Option<Profile>,
    pub hooks: Option<HookRegistry>,
    pub approver: Option<Arc<dyn Approver>>,
    pub provider: Option<Arc<dyn Provider>>,
    pub max_tool_iterations: usize,
    pub auto_compact_after: usize,
    pub workspace_root: std::path::PathBuf,
    subscribers: Vec<Subscriber>,
    pub messages: RwLock<Vec<Message>>,
    tool_cache: Cache<String, ToolResult>,
}

impl Agent {
    pub fn new() -> Self {
        Self {
            model: "gpt-4o".into(),
            system_prompt: None,
            tools: ToolRegistry::new(),
            policy: Policy::full_access(),
            scope: Scope::Coding,
            scope_profile: None,
            hooks: None,
            approver: None,
            provider: None,
            max_tool_iterations: 50,
            auto_compact_after: 80,
            workspace_root: std::env::current_dir().unwrap_or_else(|_| ".".into()),
            subscribers: Vec::new(),
            messages: RwLock::new(Vec::new()),
            tool_cache: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(std::time::Duration::from_secs(3600))
                .time_to_idle(std::time::Duration::from_secs(900))
                .build(),
        }
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }

    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        self.system_prompt = Some(prompt.into());
    }

    pub fn set_tools(&mut self, tools: ToolRegistry) {
        self.tools = tools;
    }

    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
    }

    pub fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
        let profile = mode::profile(scope);
        self.policy = profile.policy.clone();
        let base = self.system_prompt.clone();
        self.system_prompt = Some(mode::compose_prompt(base.as_deref(), &profile));
        self.scope_profile = Some(profile);
    }

    pub fn set_hooks(&mut self, hooks: HookRegistry) {
        self.hooks = Some(hooks);
    }

    pub fn set_approver(&mut self, approver: Arc<dyn Approver>) {
        self.approver = Some(approver);
    }

    pub fn set_provider(&mut self, provider: Arc<dyn Provider>) {
        self.provider = Some(provider);
    }

    pub fn set_workspace_root(&mut self, path: impl Into<std::path::PathBuf>) {
        self.workspace_root = path.into();
    }

    pub fn subscribe(&mut self, callback: impl Fn(&Event) + Send + Sync + 'static) {
        self.subscribers.push(Arc::new(callback));
    }

    fn emit(&self, event: Event) {
        for sub in &self.subscribers {
            sub(&event);
        }
    }

    pub fn clear_messages(&self) {
        self.messages.write().clear();
    }

    pub fn message_count(&self) -> usize {
        self.messages.read().len()
    }

    /// Run a prompt through the agent loop.
    /// Streams events to subscribers, executes tools, cycles turns.
    pub async fn prompt(&mut self, text: &str) -> Result<(), AgentError> {
        let turn_id = uuid::Uuid::new_v4().to_string();

        if self.message_count() >= self.auto_compact_after {
            self.compact("auto-compact before prompt");
        }

        self.messages.write().push(Message::user(text));
        self.emit(Event::AgentStart);

        let provider = self.provider.clone().ok_or(AgentError::NoProvider)?;
        let ctx = Arc::new(ToolContext::new(self.workspace_root.clone()));

        for iteration in 0..self.max_tool_iterations {
            self.emit(Event::TurnStart { turn: iteration });

            let messages: Vec<Message> = self.messages.read().clone();
            let system = self.system_prompt.clone();

            let mut tool_calls = Vec::new();
            let mut assistant_content = String::new();

            self.emit(Event::MessageStart {
                role: Role::Assistant,
            });

            #[cfg(feature = "providers")]
            {
                use crate::provider::StreamEvent;
                use futures::StreamExt;
                let stream = provider
                    .stream(&messages, &system, &self.model, &self.tools.definitions())
                    .await
                    .map_err(|e| {
                        error!("provider stream error: {e}");
                        self.emit(Event::Error(e.to_string()));
                        AgentError::Provider(e.to_string())
                    })?;

                let mut stream = stream;
                while let Some(event_result) = stream.next().await {
                    match event_result {
                        Ok(StreamEvent::Delta(delta)) => {
                            assistant_content.push_str(&delta);
                            self.emit(Event::MessageDelta { delta });
                        }
                        Ok(StreamEvent::ToolCall(call)) => {
                            tool_calls.push(call.clone());
                            self.emit(Event::ToolCall(call));
                        }
                        Ok(StreamEvent::Done) => break,
                        Err(e) => {
                            error!("stream error: {e}");
                            self.emit(Event::Error(e.to_string()));
                            return Err(AgentError::Provider(e.to_string()));
                        }
                    }
                }
            }

            #[cfg(not(feature = "providers"))]
            {
                let _ = provider;
                assistant_content =
                    "[providers feature not enabled — enable with --features providers]"
                        .to_string();
            }

            self.emit(Event::MessageEnd {
                role: Role::Assistant,
                content: assistant_content.clone(),
            });

            if !assistant_content.is_empty() {
                self.messages
                    .write()
                    .push(Message::assistant(assistant_content));
            }

            if tool_calls.is_empty() {
                self.emit(Event::TurnEnd { turn: iteration });
                break;
            }

            let results = self.execute_tools_parallel(&tool_calls, &ctx).await;
            for result in &results {
                self.messages
                    .write()
                    .push(Message::tool(&result.id, &result.content));
            }

            self.emit(Event::TurnEnd { turn: iteration });
        }

        self.emit(Event::AgentEnd);
        let _ = turn_id;
        Ok(())
    }

    /// Execute tool calls in parallel (codex-rs pattern).
    async fn execute_tools_parallel(
        &self,
        calls: &[ToolCall],
        ctx: &Arc<ToolContext>,
    ) -> Vec<ToolResult> {
        let mut results = Vec::with_capacity(calls.len());

        for call in calls {
            self.emit(Event::ToolExecutionStart(call.clone()));

            let result = self.execute_single_tool(call, ctx).await;

            self.emit(Event::ToolExecutionEnd(result.clone()));
            results.push(result);
        }

        results
    }

    async fn execute_single_tool(&self, call: &ToolCall, ctx: &Arc<ToolContext>) -> ToolResult {
        // Pi tool name mapping: translate pi names (read_file, write_file, etc.)
        // to rx4 native names (read, write, etc.) before execution.
        #[cfg(feature = "pi-compat")]
        let resolved_name = crate::pi::tools::pi_to_rx4_tool(&call.name).to_string();
        #[cfg(not(feature = "pi-compat"))]
        let resolved_name = call.name.clone();

        if let Some(profile) = &self.scope_profile {
            if !mode::tool_allowed(profile, &call.name)
                && !mode::tool_allowed(profile, &resolved_name)
            {
                let msg = format!("tool not in scope {}: {}", profile.scope.name(), call.name);
                return ToolResult::err(&call.id, msg);
            }
        }

        let decision = permissions::authorize(
            &self.policy,
            &resolved_name,
            &call.arguments,
            self.approver.as_deref(),
        );

        match decision {
            Decision::Deny => ToolResult::err(&call.id, "denied by policy"),
            Decision::Ask => {
                warn!(
                    "approval required for tool: {} (no approver → deny)",
                    call.name
                );
                ToolResult::err(&call.id, "approval required")
            }
            Decision::Allow => {
                let cache_key = format!("{}:{}", resolved_name, call.arguments);
                if let Some(cached) = self.tool_cache.get(&cache_key).await {
                    debug!("tool cache hit: {}", resolved_name);
                    return ToolResult::ok(&call.id, cached.content);
                }

                let result = match self
                    .tools
                    .execute(&resolved_name, ctx, &call.arguments)
                    .await
                {
                    Some(r) => r,
                    None => ToolResult::err(&call.id, format!("unknown tool: {}", call.name)),
                };

                if !result.is_error {
                    self.tool_cache.insert(cache_key, result.clone()).await;
                }

                result
            }
        }
    }

    pub fn compact(&self, reason: &str) {
        info!("compacting context: {reason}");
        let mut msgs = self.messages.write();
        if msgs.len() <= 4 {
            return;
        }
        let first = msgs.first().cloned();
        let last = msgs.last().cloned();
        msgs.clear();
        if let Some(f) = first {
            msgs.push(f);
        }
        msgs.push(Message::system(format!("[context compacted: {reason}]")));
        if let Some(l) = last {
            msgs.push(l);
        }
    }
}

impl Default for Agent {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("provider error: {0}")]
    Provider(String),
    #[error("tool error: {0}")]
    Tool(String),
    #[error("no provider configured")]
    NoProvider,
}
