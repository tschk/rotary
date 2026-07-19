//! Agent loop: event-driven turn cycling with tool execution, permissions, scopes,
//! cancellation, caching, and parallel tool dispatch.
//!
//! Architecture informed by codex-rs (turn-based loop with CancellationToken),
//! grok-build (moka cache, dashmap registry, parking_lot), and pi_agent_rust
//! (stable event ordering, bounded tool recursion).

mod tool_types;
pub use tool_types::*;

use crate::compaction::{apply_compaction, estimate_messages, CompactionConfig};
use crate::guardrails::plan_tool_effect_batches;
use crate::hooks::HookRegistry;
use crate::mode::{self, Profile, Scope};
use crate::permissions::{Approver, Authorizer, Decision, Policy, PolicyAuthorizer};
use crate::provider::{Message, Provider, Role};
use moka::future::Cache;
use parking_lot::RwLock;
use serde::Serialize;
use std::sync::Arc;
#[cfg(feature = "providers")]
use tracing::error;
use tracing::{debug, info, warn};

/// Stable event ordering (pi_agent_rust pattern).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum Event {
    AgentStart,
    TurnStart {
        turn: usize,
    },
    MessageStart {
        role: Role,
    },
    MessageDelta {
        delta: String,
    },
    MessageEnd {
        role: Role,
        content: String,
    },
    ToolCall(ToolCall),
    /// Host UX: tool needs approval (Codex-style ask payload).
    ApprovalRequired(crate::permissions::ApprovalRequest),
    ToolExecutionStart(ToolCall),
    ToolExecutionEnd(ToolResult),
    TurnEnd {
        turn: usize,
    },
    AgentEnd,
    Error(String),
}

pub type Subscriber = Arc<dyn Fn(&Event) + Send + Sync>;

/// The agent — owns the loop, tools, provider, policy, scope, hooks, cache.
pub struct Agent {
    pub model: String,
    pub system_prompt: Option<String>,
    pub tools: Arc<ToolRegistry>,
    pub policy: Policy,
    pub scope: Scope,
    scope_profile: Option<Profile>,
    pub hooks: Option<HookRegistry>,
    pub approver: Option<Arc<dyn Approver>>,
    /// Pluggable pre-tool gate (default: [`PolicyAuthorizer`] from `policy`).
    pub authorizer: Option<Arc<dyn Authorizer>>,
    pub provider: Option<Arc<dyn Provider>>,
    pub max_tool_iterations: usize,
    pub auto_compact_after: usize,
    pub workspace_root: std::path::PathBuf,
    pub sandbox: Option<Arc<crate::sandbox::SandboxManager>>,
    pub os_sandbox: Option<Arc<crate::sandbox::OsSandboxRunner>>,
    #[cfg(feature = "skills")]
    pub skill_registry: Option<crate::skill_engine::SkillRegistry>,
    #[cfg(feature = "skills")]
    pub skill_engine: Option<crate::skill_engine::SkillEngine>,
    #[cfg(feature = "graph-memory")]
    pub graph_memory: Option<crate::graph_memory::GraphMemory>,
    /// When true and graph_memory is set, run one dream consolidation after each prompt.
    #[cfg(feature = "graph-memory")]
    pub auto_dream: bool,
    #[cfg(feature = "ipc")]
    turn_cancellation: CancellationHandle,
    subscribers: Vec<Subscriber>,
    pub messages: RwLock<Vec<Message>>,
    tool_cache: Cache<String, ToolResult>,
}

impl Agent {
    pub fn new() -> Self {
        let mut agent = Self {
            model: "gpt-4o".into(),
            system_prompt: None,
            tools: Arc::new(ToolRegistry::new()),
            policy: Policy::workspace_write(),
            scope: Scope::Coding,
            scope_profile: None,
            hooks: None,
            approver: None,
            authorizer: None,
            provider: None,
            max_tool_iterations: 50,
            auto_compact_after: 80,
            workspace_root: std::env::current_dir().unwrap_or_else(|_| ".".into()),
            sandbox: None,
            os_sandbox: None,
            #[cfg(feature = "skills")]
            skill_registry: None,
            #[cfg(feature = "skills")]
            skill_engine: None,
            #[cfg(feature = "graph-memory")]
            graph_memory: None,
            #[cfg(feature = "graph-memory")]
            auto_dream: false,
            #[cfg(feature = "ipc")]
            turn_cancellation: CancellationHandle::new(),
            subscribers: Vec::new(),
            messages: RwLock::new(Vec::new()),
            tool_cache: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(std::time::Duration::from_secs(3600))
                .time_to_idle(std::time::Duration::from_secs(900))
                .build(),
        };
        // Policy plugin: workspace_write enables OS sandbox by default.
        if agent.policy.enable_os_sandbox {
            let _ = agent.enable_os_sandbox();
        }
        agent
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }

    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        self.system_prompt = Some(prompt.into());
    }

    pub fn set_tools(&mut self, tools: ToolRegistry) {
        self.tools = Arc::new(tools);
    }

    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
        if self.policy.enable_os_sandbox && self.os_sandbox.is_none() {
            let _ = self.enable_os_sandbox();
        }
    }

    pub fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
        let profile = mode::profile(scope);
        // Scope changes mode/sandbox only — keep host shell lists / allowlists.
        self.policy.apply_scope(&profile.policy);
        if self.policy.enable_os_sandbox && self.os_sandbox.is_none() {
            let _ = self.enable_os_sandbox();
        }
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

    /// Replace the pre-tool authorizer (pi-style host policy). `None` uses [`PolicyAuthorizer`].
    pub fn set_authorizer(&mut self, authorizer: Arc<dyn Authorizer>) {
        self.authorizer = Some(authorizer);
    }

    pub fn clear_authorizer(&mut self) {
        self.authorizer = None;
    }

    pub fn set_provider(&mut self, provider: Arc<dyn Provider>) {
        self.provider = Some(provider);
    }

    pub fn set_workspace_root(&mut self, path: impl Into<std::path::PathBuf>) {
        self.workspace_root = path.into();
    }

    #[cfg(feature = "ipc")]
    pub fn cancel(&self) {
        self.turn_cancellation.cancel();
    }

    #[cfg(feature = "ipc")]
    pub fn cancellation_handle(&self) -> CancellationHandle {
        self.turn_cancellation.clone()
    }

    /// Load project instruction files (AGENTS.md / CLAUDE.md / .cursor/rules)
    /// from `workspace_root` and merge into the system prompt.
    pub fn load_project_context(&mut self) {
        if let Some(instr) = crate::context::load_project_instructions(&self.workspace_root) {
            self.system_prompt = crate::context::compose_system_prompt(
                self.system_prompt.as_deref(),
                &instr.content,
            );
        }
    }

    pub fn set_sandbox(&mut self, sb: Arc<crate::sandbox::SandboxManager>) {
        self.sandbox = Some(sb);
    }

    pub fn set_os_sandbox(&mut self, os: Arc<crate::sandbox::OsSandboxRunner>) {
        self.os_sandbox = Some(os);
    }

    /// Enable OS sandbox for bash using auto-detected seatbelt/bwrap backend.
    pub fn enable_os_sandbox(&mut self) -> Result<(), crate::sandbox::SandboxError> {
        let mode = crate::sandbox::detect_sandbox();
        let config = crate::sandbox::OsSandboxConfig::new(mode, self.workspace_root.clone());
        let runner = crate::sandbox::OsSandboxRunner::new(config)?;
        self.os_sandbox = Some(Arc::new(runner));
        Ok(())
    }

    #[cfg(feature = "skills")]
    pub fn set_skill_registry(&mut self, registry: crate::skill_engine::SkillRegistry) {
        self.skill_registry = Some(registry);
    }

    /// Attach a skill engine for post-prompt background review.
    #[cfg(feature = "skills")]
    pub fn set_skill_engine(&mut self, engine: crate::skill_engine::SkillEngine) {
        self.skill_engine = Some(engine);
    }

    #[cfg(feature = "graph-memory")]
    pub fn set_graph_memory(&mut self, graph: crate::graph_memory::GraphMemory) {
        self.graph_memory = Some(graph);
    }

    /// Run dream consolidation after each prompt when graph_memory is set.
    #[cfg(feature = "graph-memory")]
    pub fn enable_auto_dream(&mut self, enabled: bool) {
        self.auto_dream = enabled;
    }

    pub fn subscribe(&mut self, callback: impl Fn(&Event) + Send + Sync + 'static) {
        self.subscribers.push(Arc::new(callback));
    }

    fn emit(&self, event: Event) {
        if self.subscribers.is_empty() {
            return;
        }
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
        let tokens = estimate_messages(&self.messages.read());
        if tokens >= self.auto_compact_after {
            self.compact("auto-compact before prompt");
        }

        // Inject activated skill instructions into system prompt for this turn.
        #[cfg(feature = "skills")]
        if let Some(reg) = &self.skill_registry {
            let activated = reg.auto_activate(text);
            if !activated.is_empty() {
                let block = activated.join("\n\n---\n\n");
                let base = self.system_prompt.as_deref();
                let merged = match base {
                    Some(b) => format!("{b}\n\n# Active Skills\n\n{block}"),
                    None => format!("# Active Skills\n\n{block}"),
                };
                self.system_prompt = Some(merged);
            }
        }

        self.messages.write().push(Message::user(text));
        self.emit(Event::AgentStart);

        let provider = self.provider.clone().ok_or(AgentError::NoProvider)?;
        let mut tool_ctx = ToolContext::new(self.workspace_root.clone());
        #[cfg(feature = "ipc")]
        {
            tool_ctx.cancellation = self.turn_cancellation.reset();
        }
        if let Some(sb) = self.sandbox.clone() {
            tool_ctx = tool_ctx.with_sandbox(sb);
        }
        if let Some(os) = self.os_sandbox.clone() {
            tool_ctx = tool_ctx.with_os_sandbox(os);
        }
        tool_ctx.provider = Some(provider.clone());
        tool_ctx.tools = Some(Arc::clone(&self.tools));
        let pending_scope = Arc::new(parking_lot::Mutex::new(None));
        tool_ctx.pending_scope = Some(Arc::clone(&pending_scope));
        let ctx = Arc::new(tool_ctx);

        for iteration in 0..self.max_tool_iterations {
            self.emit(Event::TurnStart { turn: iteration });

            let messages: Vec<Message> = self.messages.read().clone();
            let system = self.system_prompt.clone();

            #[allow(unused_mut)]
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            #[allow(unused_assignments)]
            let mut assistant_content = String::new();

            self.emit(Event::MessageStart {
                role: Role::Assistant,
            });

            #[cfg(feature = "providers")]
            {
                use crate::provider::StreamEvent;
                use futures::StreamExt;
                let mut attempts = 0;
                let stream = loop {
                    #[cfg(feature = "ipc")]
                    let result = ctx
                        .cancellation
                        .run(provider.stream(
                            &messages,
                            &system,
                            &self.model,
                            &self.tools.definitions(),
                        ))
                        .await
                        .map_err(|_| AgentError::Cancelled)?;
                    #[cfg(not(feature = "ipc"))]
                    let result = provider
                        .stream(&messages, &system, &self.model, &self.tools.definitions())
                        .await;
                    match result {
                        Ok(stream) => break stream,
                        Err(e) if e.is_transient() && attempts < 2 => {
                            attempts += 1;
                            #[cfg(feature = "ipc")]
                            ctx.cancellation
                                .run(tokio::time::sleep(std::time::Duration::from_millis(
                                    250 * (1 << attempts),
                                )))
                                .await
                                .map_err(|_| AgentError::Cancelled)?;
                            #[cfg(not(feature = "ipc"))]
                            tokio::time::sleep(std::time::Duration::from_millis(
                                250 * (1 << attempts),
                            ))
                            .await;
                        }
                        Err(e) => {
                            error!("provider stream error: {e}");
                            self.emit(Event::Error(e.to_string()));
                            return Err(AgentError::Provider(e.to_string()));
                        }
                    }
                };

                let mut stream = stream;
                loop {
                    #[cfg(feature = "ipc")]
                    let next = ctx
                        .cancellation
                        .run(stream.next())
                        .await
                        .map_err(|_| AgentError::Cancelled)?;
                    #[cfg(not(feature = "ipc"))]
                    let next = stream.next().await;
                    let Some(event_result) = next else {
                        break;
                    };
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
                let _ = (&provider, &messages, &system);
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
            if let Some(scope) = pending_scope.lock().take() {
                self.set_scope(scope);
            }

            self.emit(Event::TurnEnd { turn: iteration });
        }

        #[cfg(feature = "graph-memory")]
        if let Some(graph) = self.graph_memory.as_mut() {
            let turns: Vec<crate::graph_memory::ConversationTurn> = self
                .messages
                .read()
                .iter()
                .map(|m| crate::graph_memory::ConversationTurn {
                    role: m.role.to_string(),
                    content: m.content.clone(),
                })
                .collect();
            let extracted = crate::graph_memory::ConversationExtractor::new().extract(&turns);
            for node in extracted.nodes {
                graph.add_node(node);
            }
            for edge in extracted.edges {
                let _ = graph.add_edge(edge);
            }
            if self.auto_dream {
                let _ = crate::dream_scheduler::DreamScheduler::new().run_cycle(graph);
            }
        }

        // Background skill review when a SkillEngine is attached (host opt-in).
        #[cfg(feature = "skills")]
        if let Some(engine) = self.skill_engine.as_mut() {
            let turns: Vec<crate::skill_engine::ConversationTurn> = self
                .messages
                .read()
                .iter()
                .map(|m| crate::skill_engine::ConversationTurn {
                    role: m.role.to_string(),
                    content: m.content.clone(),
                    tool_calls: Vec::new(),
                })
                .collect();
            let mut reviewer = crate::background_review::BackgroundReviewer::new(engine);
            if let Ok(reviews) =
                reviewer.review_conversation(&turns, crate::skill_engine::SkillOutcome::Success)
            {
                let _ = reviewer.apply_review(&reviews);
            }
        }

        self.emit(Event::AgentEnd);
        Ok(())
    }

    /// Execute tool calls: parallel batches for Read/Network, serial for Write/Process.
    async fn execute_tools_parallel(
        &self,
        calls: &[ToolCall],
        ctx: &Arc<ToolContext>,
    ) -> Vec<ToolResult> {
        let effects: Vec<ToolEffect> = calls
            .iter()
            .map(|c| {
                let name = normalize_tool_name(&c.name);
                self.tools.effect_of(name)
            })
            .collect();
        let batches = plan_tool_effect_batches(&effects);
        let mut results: Vec<Option<ToolResult>> = vec![None; calls.len()];

        for batch in batches {
            if batch.len() == 1 {
                let idx = batch[0];
                let original = &calls[idx];
                self.emit(Event::ToolExecutionStart(original.clone()));
                let (call, result) = self.execute_single_tool(original, ctx).await;
                if result.is_error && result.content == "approval required" {
                    self.emit(Event::ApprovalRequired(
                        crate::permissions::ApprovalRequest::from_call(&call, &self.policy),
                    ));
                }
                self.emit(Event::ToolExecutionEnd(result.clone()));
                results[idx] = Some(result);
                continue;
            }

            let tools = Arc::clone(&self.tools);
            let policy = self.policy.clone();
            let scope_profile = self.scope_profile.clone();
            let approver = self.approver.clone();
            let authorizer = self.authorizer.clone();
            let tool_cache = self.tool_cache.clone();
            let mut join_set = tokio::task::JoinSet::new();

            for idx in batch {
                let original = &calls[idx];
                let call = match self.apply_before_tool_hooks(original) {
                    Ok(c) => c,
                    Err(reason) => {
                        self.emit(Event::ToolExecutionStart(original.clone()));
                        let result = ToolResult::err(&original.id, reason);
                        self.emit(Event::ToolExecutionEnd(result.clone()));
                        results[idx] = Some(result);
                        continue;
                    }
                };
                self.emit(Event::ToolExecutionStart(call.clone()));
                let ctx = Arc::clone(ctx);
                let tools = Arc::clone(&tools);
                let policy = policy.clone();
                let scope_profile = scope_profile.clone();
                let approver = approver.clone();
                let authorizer = authorizer.clone();
                let tool_cache = tool_cache.clone();
                join_set.spawn(async move {
                    let result = Agent::run_tool_call(
                        &tools,
                        &policy,
                        authorizer.as_deref(),
                        scope_profile.as_ref(),
                        approver.as_deref(),
                        &tool_cache,
                        &call,
                        &ctx,
                    )
                    .await;
                    (idx, call, result)
                });
            }

            while let Some(joined) = join_set.join_next().await {
                match joined {
                    Ok((idx, call, result)) => {
                        if result.is_error && result.content == "approval required" {
                            self.emit(Event::ApprovalRequired(
                                crate::permissions::ApprovalRequest::from_call(&call, &self.policy),
                            ));
                        }
                        self.emit(Event::ToolExecutionEnd(result.clone()));
                        results[idx] = Some(result);
                    }
                    Err(e) => {
                        warn!("parallel tool task join error: {e}");
                    }
                }
            }
        }

        results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                r.unwrap_or_else(|| {
                    ToolResult::err(
                        calls.get(i).map(|c| c.id.as_str()).unwrap_or(""),
                        "tool execution failed",
                    )
                })
            })
            .collect()
    }

    fn apply_before_tool_hooks(&self, call: &ToolCall) -> Result<ToolCall, String> {
        match &self.hooks {
            Some(hooks) => hooks.run_before_tool(call),
            None => Ok(call.clone()),
        }
    }

    async fn execute_single_tool(
        &self,
        call: &ToolCall,
        ctx: &Arc<ToolContext>,
    ) -> (ToolCall, ToolResult) {
        let call = match self.apply_before_tool_hooks(call) {
            Ok(c) => c,
            Err(reason) => {
                let id = call.id.clone();
                return (call.clone(), ToolResult::err(&id, reason));
            }
        };
        let result = Self::run_tool_call(
            self.tools.as_ref(),
            &self.policy,
            self.authorizer.as_deref(),
            self.scope_profile.as_ref(),
            self.approver.as_deref(),
            &self.tool_cache,
            &call,
            ctx,
        )
        .await;
        (call, result)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_tool_call(
        tools: &ToolRegistry,
        policy: &Policy,
        authorizer: Option<&dyn Authorizer>,
        scope_profile: Option<&Profile>,
        approver: Option<&dyn Approver>,
        tool_cache: &Cache<String, ToolResult>,
        call: &ToolCall,
        ctx: &Arc<ToolContext>,
    ) -> ToolResult {
        let resolved_name = normalize_tool_name(&call.name).to_string();

        if let Some(profile) = scope_profile {
            if !mode::tool_allowed(profile, &call.name)
                && !mode::tool_allowed(profile, &resolved_name)
            {
                let msg = format!("tool not in scope {}: {}", profile.scope.name(), call.name);
                return ToolResult::err(&call.id, msg);
            }
        }

        // Host-supplied Authorizer, else default PolicyAuthorizer (pi beforeToolCall shape).
        let decision = match authorizer {
            Some(auth) => auth.authorize(
                &resolved_name,
                &call.arguments,
                approver,
                Some(ctx.workspace_root.as_path()),
            ),
            None => PolicyAuthorizer::new(policy.clone()).authorize(
                &resolved_name,
                &call.arguments,
                approver,
                Some(ctx.workspace_root.as_path()),
            ),
        };

        match decision {
            Decision::Deny => ToolResult::err(&call.id, "denied by policy"),
            Decision::Ask => {
                // Rich payload is emitted by callers that have Agent self; parallel path
                // only returns the error string — serial path re-emits below when possible.
                ToolResult::err(&call.id, "approval required")
            }
            Decision::Allow => {
                let effect = tools.effect_of(&resolved_name);
                let cache_key = format!("{}:{}", resolved_name, call.arguments);
                if effect == ToolEffect::Read {
                    if let Some(cached) = tool_cache.get(&cache_key).await {
                        debug!("tool cache hit: {}", resolved_name);
                        return ToolResult::ok(&call.id, cached.content);
                    }
                }

                let mut result = match tools.execute(&resolved_name, ctx, &call.arguments).await {
                    Some(r) => r,
                    None => ToolResult::err(&call.id, format!("unknown tool: {}", call.name)),
                };

                result.content = crate::secrets::Redactor::new().redact(&result.content);

                if !result.is_error {
                    match effect {
                        ToolEffect::Read => {
                            tool_cache.insert(cache_key, result.clone()).await;
                        }
                        ToolEffect::Write | ToolEffect::Process => {
                            tool_cache.invalidate_all();
                        }
                        ToolEffect::Network => {}
                    }
                }

                result
            }
        }
    }

    pub fn compact(&self, reason: &str) {
        info!("compacting context: {reason}");
        let mut msgs = self.messages.write();
        if msgs.len() <= 2 {
            return;
        }
        let trigger = self.auto_compact_after.max(64);
        let reserve = (trigger / 4).max(32);
        let keep_recent = (trigger / 4).max(32);
        let config = CompactionConfig::new(trigger + reserve, reserve, keep_recent);
        let result = apply_compaction(&mut msgs, &config);
        if !result.summary.is_empty() {
            msgs.push(Message::system(format!("[compact reason: {reason}]")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static PARALLEL_DELAY_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn delay_read_tool(name: &str) -> ToolDefinition {
        ToolDefinition::new_boxed(
            name,
            "delay read",
            "{}",
            Box::new(|_ctx, _args| {
                Box::pin(async {
                    PARALLEL_DELAY_CALLS.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    ToolResult::ok("id", "ok")
                })
            }),
        )
        .with_effect(ToolEffect::Read)
    }

    #[tokio::test]
    async fn parallel_read_tools_run_concurrently() {
        PARALLEL_DELAY_CALLS.store(0, Ordering::SeqCst);
        let mut registry = ToolRegistry::new();
        registry.register(delay_read_tool("a"));
        registry.register(delay_read_tool("b"));
        let mut agent = Agent::new();
        agent.set_tools(registry);
        agent.set_policy(Policy::full_access());
        let ctx = Arc::new(ToolContext::new("."));
        let calls = vec![
            ToolCall {
                id: "1".into(),
                name: "a".into(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "2".into(),
                name: "b".into(),
                arguments: "{}".into(),
            },
        ];
        let start = std::time::Instant::now();
        let results = agent.execute_tools_parallel(&calls, &ctx).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| !r.is_error));
        assert_eq!(PARALLEL_DELAY_CALLS.load(Ordering::SeqCst), 2);
        assert!(start.elapsed() < Duration::from_millis(70));
    }

    static CACHE_READ_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CACHE_WRITE_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[tokio::test]
    async fn cache_not_used_for_write_effect() {
        CACHE_READ_CALLS.store(0, Ordering::SeqCst);
        CACHE_WRITE_CALLS.store(0, Ordering::SeqCst);
        let mut registry = ToolRegistry::new();
        registry.register(
            ToolDefinition::new_boxed(
                "r",
                "read",
                "{}",
                Box::new(|_ctx, _args| {
                    Box::pin(async {
                        CACHE_READ_CALLS.fetch_add(1, Ordering::SeqCst);
                        ToolResult::ok("id", "data")
                    })
                }),
            )
            .with_effect(ToolEffect::Read),
        );
        registry.register(
            ToolDefinition::new_boxed(
                "w",
                "write",
                "{}",
                Box::new(|_ctx, _args| {
                    Box::pin(async {
                        CACHE_WRITE_CALLS.fetch_add(1, Ordering::SeqCst);
                        ToolResult::ok("id", "wrote")
                    })
                }),
            )
            .with_effect(ToolEffect::Write),
        );
        let mut agent = Agent::new();
        agent.set_tools(registry);
        agent.set_policy(Policy::full_access());
        let ctx = Arc::new(ToolContext::new("."));
        let read_call = ToolCall {
            id: "1".into(),
            name: "r".into(),
            arguments: "{}".into(),
        };
        let write_call = ToolCall {
            id: "2".into(),
            name: "w".into(),
            arguments: "{}".into(),
        };

        agent.execute_single_tool(&read_call, &ctx).await;
        agent.execute_single_tool(&read_call, &ctx).await;
        assert_eq!(CACHE_READ_CALLS.load(Ordering::SeqCst), 1);

        agent.execute_single_tool(&write_call, &ctx).await;
        agent.execute_single_tool(&write_call, &ctx).await;
        assert_eq!(CACHE_WRITE_CALLS.load(Ordering::SeqCst), 2);

        agent.execute_single_tool(&read_call, &ctx).await;
        assert_eq!(CACHE_READ_CALLS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn compact_uses_token_aware_compaction() {
        let mut agent = Agent::new();
        agent.auto_compact_after = 50;
        {
            let mut msgs = agent.messages.write();
            msgs.push(Message::system("sys"));
            for i in 0..20 {
                msgs.push(Message::user(
                    format!("old message {i} ",) + &"x".repeat(80),
                ));
                msgs.push(Message::assistant("reply".repeat(40)));
            }
            msgs.push(Message::user("recent tail"));
        }
        agent.compact("test");
        let msgs = agent.messages.read();
        assert!(msgs.len() < 42);
        assert!(msgs.iter().any(|m| m.content.contains("context compacted")));
        assert!(msgs.iter().any(|m| m.content.contains("recent tail")));
    }

    #[cfg(feature = "ipc")]
    #[test]
    fn cancellation_handle_cancels_reset_turn() {
        let handle = CancellationHandle::new();
        let external = handle.clone();
        let token = handle.reset();
        external.cancel();
        assert!(token.is_canceled());
    }

    #[test]
    fn set_scope_preserves_host_shell_policy() {
        let mut agent = Agent::new();
        agent.set_policy(
            Policy::workspace_write()
                .with_shell_allow(["git *", "cargo test*"])
                .with_shell_deny(["sudo *"])
                .with_enforce_dangerous_shell(false),
        );
        agent.set_scope(Scope::Research);
        assert_eq!(
            agent.policy.mode,
            crate::permissions::PermissionMode::ReadOnly
        );
        assert_eq!(
            agent.policy.shell_allow,
            vec!["git *".to_string(), "cargo test*".to_string()]
        );
        assert_eq!(agent.policy.shell_deny, vec!["sudo *".to_string()]);
        assert!(!agent.policy.enforce_dangerous_shell);
        // research is read_only → sandbox flag from profile
        assert!(!agent.policy.enable_os_sandbox);

        agent.set_scope(Scope::Coding);
        assert_eq!(
            agent.policy.mode,
            crate::permissions::PermissionMode::WorkspaceWrite
        );
        assert_eq!(
            agent.policy.shell_allow,
            vec!["git *".to_string(), "cargo test*".to_string()]
        );
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
    #[error("agent cancelled")]
    Cancelled,
}
