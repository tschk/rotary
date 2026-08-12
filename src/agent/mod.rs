//! Agent loop: event-driven turn cycling with tool execution, permissions, scopes,
//! cancellation, caching, and parallel tool dispatch.
//!
//! Architecture informed by codex-rs (turn-based loop with CancellationToken),
//! grok-build (moka cache, dashmap registry, parking_lot), and pi_agent_rust
//! (stable event ordering, bounded tool recursion).

mod tool_types;
pub use tool_types::*;

use crate::compaction::{
    apply_compaction, apply_compaction_result, compact_messages_semantically, estimate_messages,
    CompactionConfig,
};
use crate::cost::{PricingRegistry, SessionCost, TokenUsage};
use crate::guardrails::{
    plan_tool_effect_batches, GuardrailConfig, GuardrailDecision, SelfHealingRetry, ToolGuardrails,
};
use crate::hooks::HookRegistry;
use crate::mode::{self, Profile, Scope};
use crate::permissions::{
    Approver, AsyncApprover, Authorizer, Decision, PlanApprover, PlanDecision, PlanProposal,
    Policy, PolicyAuthorizer,
};
use crate::provider::{Message, Provider, Role};
use moka::future::Cache;
use parking_lot::RwLock;
use serde::Serialize;
use std::sync::Arc;
use std::time::Instant;
#[cfg(feature = "providers")]
use tracing::error;
use tracing::{debug, info, warn};

/// Stable event ordering (pi_agent_rust pattern).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum Event {
    AgentStart,
    ContextUsage {
        used_tokens: usize,
        context_window: usize,
        auto_compact_at: usize,
    },
    Usage {
        model: String,
        usage: TokenUsage,
        estimated: bool,
    },
    CompactionStart {
        reason: String,
        before_tokens: usize,
    },
    CompactionEnd {
        reason: String,
        result: crate::compaction::CompactionResult,
    },
    SkillActivated {
        id: String,
        name: String,
    },
    ToolSource {
        tool: String,
        source: ToolSource,
    },
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
    /// Host UX: the whole turn's plan needs approval before anything runs.
    PlanProposed(crate::permissions::PlanProposal),
    /// The host answered a [`Event::PlanProposed`].
    PlanDecided {
        decision: crate::permissions::PlanDecision,
    },
    /// Loop detection fired but the turn continues; the warning is also fed
    /// back to the model.
    GuardrailWarning {
        tool: String,
        reason: String,
    },
    /// Loop detection ended the turn.
    GuardrailStop {
        tool: String,
        reason: String,
    },
    /// A failing turn is being re-prompted with error context.
    SelfHealing {
        attempt: u8,
        max_attempts: u8,
        errors: Vec<String>,
    },
    ToolExecutionStart(ToolCall),
    ToolExecutionEnd(ToolResult),
    TurnEnd {
        turn: usize,
    },
    AgentEnd,
    Error(String),
    BudgetExceeded {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolSource {
    Builtin,
    Mcp { server: String },
    ComputerUse,
}

pub type Subscriber = Arc<dyn Fn(&Event) + Send + Sync>;

#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentBudget {
    pub max_cost: Option<f64>,
    pub max_duration_seconds: Option<u64>,
    pub reserve_budget: Option<f64>,
    pub reserve_budget_fraction: Option<f64>,
}

impl AgentBudget {
    pub fn effective_max_cost(&self) -> Option<f64> {
        let max = self.max_cost?;
        let reserve = match (self.reserve_budget, self.reserve_budget_fraction) {
            (Some(usd), Some(frac)) => usd + (max * frac),
            (Some(usd), None) => usd,
            (None, Some(frac)) => max * frac,
            (None, None) => 0.0,
        };
        Some((max - reserve).max(0.0))
    }

    pub fn exceeded(&self, start: Option<Instant>, total_cost: f64) -> Option<String> {
        if let Some(max_dur) = self.max_duration_seconds {
            if let Some(start) = start {
                let elapsed = start.elapsed().as_secs();
                if elapsed >= max_dur {
                    return Some(format!("time budget exceeded: {elapsed}s >= {max_dur}s"));
                }
            }
        }
        if let Some(max) = self.effective_max_cost() {
            if total_cost >= max {
                return Some(format!(
                    "cost budget exceeded: ${total_cost:.4} >= ${max:.4}"
                ));
            }
        }
        None
    }
}

fn append_active_skills(base: Option<String>, active_skills: Option<&str>) -> Option<String> {
    let Some(active_skills) = active_skills else {
        return base;
    };
    Some(match base {
        Some(base) => format!("{base}\n\n# Active Skills\n\n{active_skills}"),
        None => format!("# Active Skills\n\n{active_skills}"),
    })
}

/// The agent — owns the loop, tools, provider, policy, scope, hooks, cache.
pub struct Agent {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub system_prompt: Option<String>,
    base_system_prompt: Option<String>,
    pub tools: Arc<ToolRegistry>,
    pub policy: Policy,
    pub scope: Scope,
    scope_profile: Option<Profile>,
    pub hooks: Option<HookRegistry>,
    pub approver: Option<Arc<dyn Approver>>,
    /// Async Approver (pi `beforeToolCall` Promise shape). Preferred for UI hosts.
    pub async_approver: Option<Arc<dyn AsyncApprover>>,
    /// Pluggable pre-tool gate (default: [`PolicyAuthorizer`] from `policy`).
    pub authorizer: Option<Arc<dyn Authorizer>>,
    /// Whole-plan gate, consulted once before the first tool call of a turn.
    ///
    /// `None` (the default) runs tool calls as soon as the model emits them,
    /// which is the historical behaviour.
    pub plan_approver: Option<Arc<dyn PlanApprover>>,
    /// Loop-detection thresholds. `None` (the default) disables loop
    /// detection entirely, which is the historical behaviour.
    ///
    /// A fresh [`ToolGuardrails`] is built from this config for each
    /// `prompt()`, so observations never leak between turns.
    pub guardrails: Option<GuardrailConfig>,
    /// Self-healing re-prompt budget. `None` (the default) means a failing
    /// tool is reported to the model without extra coaching, which is the
    /// historical behaviour.
    ///
    /// The value is a template: each `prompt()` clones it, so the attempt
    /// budget is per-turn rather than per-agent.
    pub self_healing: Option<SelfHealingRetry>,
    pub provider: Option<Arc<dyn Provider>>,
    pub max_tool_iterations: usize,
    pub auto_compact_after: usize,
    pub workspace_root: std::path::PathBuf,
    /// Optional host-owned autoresearch controller. Attaching it exposes the
    /// SDK primitive without scheduling iterations or changing tool policy.
    pub autoresearch_controller:
        Option<crate::autoresearch_controller::AutoresearchControllerHandle>,
    pub sandbox: Option<Arc<crate::sandbox::SandboxManager>>,
    pub os_sandbox: Option<Arc<crate::sandbox::OsSandboxRunner>>,
    #[cfg(feature = "ipc")]
    lsp: Arc<crate::lsp::LspManager>,
    /// True when policy requested OS sandboxing but setup failed. Shell tools
    /// must refuse execution rather than silently falling through to bare bash.
    os_sandbox_failed: bool,
    #[cfg(feature = "skills")]
    pub skill_registry: Option<crate::skill_engine::SkillRegistry>,
    #[cfg(feature = "skills")]
    pub skill_engine: Option<crate::skill_engine::SkillEngine>,
    #[cfg(feature = "graph-memory")]
    pub graph_memory: Option<crate::graph_memory::GraphMemory>,
    /// When true and graph_memory is set, run one dream consolidation after each prompt.
    #[cfg(feature = "graph-memory")]
    pub auto_dream: bool,
    #[cfg(feature = "zkr-memory")]
    pub self_improve: Option<crate::self_improve::SelfImprove>,
    #[cfg(feature = "personality")]
    pub personality: Option<crate::personality::Personality>,
    turn_cancellation: CancellationHandle,
    subscribers: Vec<Subscriber>,
    pub messages: Arc<RwLock<Vec<Message>>>,
    tool_cache: Cache<String, ToolResult>,
    pub budget: Option<AgentBudget>,
    pub pricing_registry: PricingRegistry,
    /// Provider-reported prompt-cache usage for this agent session.
    pub cache_stats: crate::prompt_cache::CacheStatsTracker,
    session_cost: SessionCost,
    budget_start: Option<Instant>,
}

impl Agent {
    pub fn new() -> Self {
        let mut agent = Self {
            model: "gpt-4o".into(),
            reasoning_effort: None,
            system_prompt: None,
            base_system_prompt: None,
            tools: Arc::new(ToolRegistry::new()),
            policy: Policy::workspace_write(),
            scope: Scope::Coding,
            scope_profile: None,
            hooks: None,
            approver: None,
            async_approver: None,
            plan_approver: None,
            guardrails: None,
            self_healing: None,
            authorizer: None,
            provider: None,
            max_tool_iterations: 50,
            auto_compact_after: 0,
            workspace_root: std::env::current_dir().unwrap_or_else(|_| ".".into()),
            autoresearch_controller: None,
            sandbox: None,
            os_sandbox: None,
            #[cfg(feature = "ipc")]
            lsp: Arc::new(crate::lsp::LspManager::new()),
            os_sandbox_failed: false,
            #[cfg(feature = "skills")]
            skill_registry: None,
            #[cfg(feature = "skills")]
            skill_engine: None,
            #[cfg(feature = "graph-memory")]
            graph_memory: None,
            #[cfg(feature = "graph-memory")]
            auto_dream: false,
            #[cfg(feature = "zkr-memory")]
            self_improve: None,
            #[cfg(feature = "personality")]
            personality: None,
            turn_cancellation: CancellationHandle::new(),
            subscribers: Vec::new(),
            messages: Arc::new(RwLock::new(Vec::new())),
            tool_cache: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(std::time::Duration::from_secs(3600))
                .time_to_idle(std::time::Duration::from_secs(900))
                .build(),
            budget: None,
            pricing_registry: PricingRegistry::new(),
            cache_stats: crate::prompt_cache::CacheStatsTracker::new(),
            session_cost: SessionCost::new(),
            budget_start: None,
        };
        // Always attach userspace workspace sandbox (path confinement for FS tools).
        agent.ensure_userspace_sandbox();
        // OS sandbox when policy requests it — fail closed (no silent bare bash).
        if agent.policy.enable_os_sandbox {
            if let Err(e) = agent.enable_os_sandbox() {
                // Do NOT clear enable_os_sandbox — hosts must see the requested
                // policy. Track the failure so shell tools refuse execution.
                agent.os_sandbox_failed = true;
                tracing::warn!("OS sandbox unavailable — shell tools will be blocked: {e}");
            }
        }
        agent
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.reasoning_effort = effort;
    }

    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        self.base_system_prompt = Some(prompt.into());
        self.refresh_system_prompt();
    }

    pub fn set_tools(&mut self, tools: ToolRegistry) {
        self.tools = Arc::new(tools);
    }

    #[cfg(feature = "ipc")]
    pub fn set_lsp_manager(&mut self, lsp: Arc<crate::lsp::LspManager>) {
        self.lsp = lsp;
    }

    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
        // A custom authorizer may have captured the previous policy. Drop the
        // snapshot so subsequent calls use the new live policy by default.
        self.authorizer = None;
        self.ensure_userspace_sandbox();
        if self.policy.enable_os_sandbox && self.os_sandbox.is_none() && !self.os_sandbox_failed {
            if let Err(e) = self.enable_os_sandbox() {
                self.os_sandbox_failed = true;
                tracing::warn!("OS sandbox unavailable — shell tools will be blocked: {e}");
            }
        }
    }

    pub fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
        let profile = mode::profile(scope);
        // Scope changes mode/sandbox only — keep host shell lists / allowlists.
        self.policy.apply_scope(&profile.policy);
        self.authorizer = None;
        self.ensure_userspace_sandbox();
        if self.policy.enable_os_sandbox && self.os_sandbox.is_none() && !self.os_sandbox_failed {
            if let Err(e) = self.enable_os_sandbox() {
                self.os_sandbox_failed = true;
                tracing::warn!("OS sandbox unavailable — shell tools will be blocked: {e}");
            }
        }
        self.scope_profile = Some(profile);
        self.refresh_system_prompt();
    }

    pub fn set_hooks(&mut self, hooks: HookRegistry) {
        self.hooks = Some(hooks);
    }

    pub fn set_approver(&mut self, approver: Arc<dyn Approver>) {
        self.approver = Some(approver);
    }

    /// Async Approver (preferred for interactive hosts; pi beforeToolCall is async).
    pub fn set_async_approver(&mut self, approver: Arc<dyn AsyncApprover>) {
        self.async_approver = Some(approver);
    }

    pub fn clear_async_approver(&mut self) {
        self.async_approver = None;
    }

    /// Replace the pre-tool authorizer (pi-style host policy).
    /// Prefer leaving unset so each tool call uses a fresh [`PolicyAuthorizer`] from `policy`.
    /// If you install a snapshot authorizer, re-set it after `set_policy` / `set_scope`.
    pub fn set_authorizer(&mut self, authorizer: Arc<dyn Authorizer>) {
        self.authorizer = Some(authorizer);
    }

    /// Drop custom authorizer; subsequent tools use live `policy` via [`PolicyAuthorizer`].
    /// Gate the turn's plan before any tool runs.
    pub fn set_plan_approver(&mut self, approver: Arc<dyn PlanApprover>) {
        self.plan_approver = Some(approver);
    }

    pub fn clear_plan_approver(&mut self) {
        self.plan_approver = None;
    }

    /// Enable loop detection with the given thresholds.
    pub fn set_guardrails(&mut self, config: GuardrailConfig) {
        self.guardrails = Some(config);
    }

    pub fn clear_guardrails(&mut self) {
        self.guardrails = None;
    }

    /// Enable self-healing re-prompts, allowing `max_attempts` per turn.
    pub fn set_self_healing(&mut self, max_attempts: u8) {
        self.self_healing = Some(SelfHealingRetry::new(max_attempts));
    }

    pub fn clear_self_healing(&mut self) {
        self.self_healing = None;
    }

    pub fn clear_authorizer(&mut self) {
        self.authorizer = None;
    }

    pub fn set_provider(&mut self, provider: Arc<dyn Provider>) {
        self.provider = Some(provider);
    }

    pub fn set_workspace_root(&mut self, path: impl Into<std::path::PathBuf>) {
        let new_root = path.into();
        let mut sandbox_config = self.sandbox.as_ref().map(|sb| sb.config());
        if let Some(config) = sandbox_config.as_mut() {
            config.workspace_root = new_root.clone();
        }
        let mut os_config = self.os_sandbox.as_ref().map(|os| os.config().clone());
        if let Some(config) = os_config.as_mut() {
            config.workspace = new_root.clone();
        }

        self.workspace_root = new_root;
        self.authorizer = None;
        // Rebuild confinement against the new root while retaining custom
        // allow/deny lists and network policy.
        self.sandbox = Some(Arc::new(match sandbox_config {
            Some(config) => crate::sandbox::SandboxManager::from_config(config),
            None => {
                let mut sb = crate::sandbox::SandboxManager::new(
                    crate::sandbox::SandboxProfile::Workspace,
                    self.workspace_root.clone(),
                );
                sb.set_allow_network(true);
                sb
            }
        }));
        self.tool_cache.invalidate_all();
        self.os_sandbox = None;
        self.os_sandbox_failed = false;
        if self.policy.enable_os_sandbox {
            let result = match os_config {
                Some(config) => crate::sandbox::OsSandboxRunner::new(config)
                    .map(Arc::new)
                    .map(|runner| self.os_sandbox = Some(runner)),
                None => self.enable_os_sandbox().map(|_| ()),
            };
            if let Err(e) = result {
                self.os_sandbox_failed = true;
                tracing::warn!("OS sandbox unavailable after workspace change — shell tools will be blocked: {e}");
            }
        }
    }

    /// Attach an explicitly created autoresearch controller. The controller is
    /// host-driven; the agent loop never starts, schedules, accepts, or
    /// applies an experiment because of this attachment.
    pub fn set_autoresearch_controller(
        &mut self,
        controller: crate::autoresearch_controller::AutoresearchControllerHandle,
    ) {
        self.autoresearch_controller = Some(controller);
    }

    pub fn autoresearch_controller(
        &self,
    ) -> Option<crate::autoresearch_controller::AutoresearchControllerHandle> {
        self.autoresearch_controller.clone()
    }

    pub fn clear_autoresearch_controller(&mut self) {
        self.autoresearch_controller = None;
    }

    /// Attach a `zkr`-backed self-improvement loop.
    #[cfg(feature = "zkr-memory")]
    pub fn set_self_improve(&mut self, improve: crate::self_improve::SelfImprove) {
        self.self_improve = Some(improve);
    }

    /// Attach a `zkr`-backed personality behavioral runtime.
    #[cfg(feature = "personality")]
    pub fn set_personality(&mut self, personality: crate::personality::Personality) {
        self.personality = Some(personality);
    }

    pub fn cancel(&self) {
        self.turn_cancellation.cancel();
    }

    pub fn cancellation_handle(&self) -> CancellationHandle {
        self.turn_cancellation.clone()
    }

    /// Load project instruction files (AGENTS.md / CLAUDE.md / .cursor/rules)
    /// from `workspace_root` and merge into the system prompt.
    pub fn load_project_context(&mut self) {
        if let Some(instr) = crate::context::load_project_instructions(&self.workspace_root) {
            self.base_system_prompt = crate::context::compose_system_prompt(
                self.base_system_prompt.as_deref(),
                &instr.content,
            );
            self.refresh_system_prompt();
        }
    }

    fn refresh_system_prompt(&mut self) {
        self.system_prompt = self.scope_profile.as_ref().map_or_else(
            || self.base_system_prompt.clone(),
            |profile| {
                Some(mode::compose_prompt(
                    self.base_system_prompt.as_deref(),
                    profile,
                ))
            },
        );
    }

    pub fn set_sandbox(&mut self, sb: Arc<crate::sandbox::SandboxManager>) {
        self.sandbox = Some(sb);
    }

    pub fn set_os_sandbox(&mut self, os: Arc<crate::sandbox::OsSandboxRunner>) {
        self.os_sandbox = Some(os);
    }

    pub fn set_budget(&mut self, budget: AgentBudget) {
        self.budget = Some(budget);
    }

    pub fn set_pricing_registry(&mut self, registry: PricingRegistry) {
        self.pricing_registry = registry;
    }

    pub fn total_cost(&self) -> f64 {
        self.session_cost.total_cost()
    }

    pub fn session_cost(&self) -> &SessionCost {
        &self.session_cost
    }

    /// Current provider prompt-cache statistics.
    pub fn cache_stats(&self) -> crate::prompt_cache::CacheStats {
        self.cache_stats.stats()
    }

    fn check_budget(&self) -> Option<String> {
        self.budget
            .as_ref()
            .and_then(|b| b.exceeded(self.budget_start, self.session_cost.total_cost()))
    }

    /// Attach userspace workspace path sandbox if missing.
    pub fn ensure_userspace_sandbox(&mut self) {
        if self.sandbox.is_none() {
            let mut sb = crate::sandbox::SandboxManager::new(
                crate::sandbox::SandboxProfile::Workspace,
                self.workspace_root.clone(),
            );
            // Path confinement is the primary goal; network tools still pass Policy.
            // Hosts that need hard network deny replace sandbox or call set_allow_network(false).
            sb.set_allow_network(true);
            self.sandbox = Some(Arc::new(sb));
        }
    }

    /// Enable OS sandbox for bash using seatbelt/bwrap. Errors if backend missing
    /// (no silent fail-open to bare bash). Always ensures userspace sandbox too.
    pub fn enable_os_sandbox(&mut self) -> Result<(), crate::sandbox::SandboxError> {
        self.ensure_userspace_sandbox();
        let mode = crate::sandbox::detect_sandbox();
        if matches!(mode, crate::sandbox::OsSandbox::UserspaceOnly) {
            return Err(crate::sandbox::SandboxError::PathDenied(
                "no seatbelt/bwrap on this host".into(),
            ));
        }
        let config = crate::sandbox::OsSandboxConfig::new(mode, self.workspace_root.clone());
        let runner = crate::sandbox::OsSandboxRunner::new(config)?;
        self.os_sandbox = Some(Arc::new(runner));
        self.policy.enable_os_sandbox = true;
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

    /// Shared handle to the agent's message history.
    ///
    /// This is the supported way for a host to observe or append messages
    /// without holding a lock on the [`Agent`] itself. [`Agent::prompt`] takes
    /// `&mut self`, so a host that wraps the agent in a mutex would otherwise
    /// block every read behind a whole turn.
    ///
    /// The agent never replaces the message vector — compaction, session
    /// loading and every other mutation happen in place through this same
    /// `RwLock` — so a handle stays valid for the life of the agent.
    ///
    /// Appends are picked up mid-turn: the tool loop re-reads the history at
    /// the start of every tool iteration, so a message pushed through this
    /// handle while a turn is in flight lands on the next iteration of that
    /// turn rather than waiting for it to finish.
    ///
    /// Drop the guard as soon as the mutation is done. The tool loop takes
    /// this same lock at the top of every iteration, so a guard held longer
    /// than necessary — and especially one held across an `.await` — stalls
    /// the running turn. Take the lock, mutate, and let it go in one
    /// statement.
    ///
    /// Note that compaction and [`Agent::clear_messages`] mutate the same
    /// vector, so a host observing across either will see entries disappear
    /// underneath it. That is inherent to sharing the history.
    pub fn messages_handle(&self) -> Arc<RwLock<Vec<Message>>> {
        Arc::clone(&self.messages)
    }

    pub fn message_count(&self) -> usize {
        self.messages.read().len()
    }

    pub fn context_window(&self) -> usize {
        crate::models::ModelRegistry::load()
            .get(&self.model)
            .map(|model| model.context_window)
            .unwrap_or(CompactionConfig::DEFAULT_CONTEXT_WINDOW)
    }

    pub fn auto_compact_threshold(&self) -> usize {
        if self.auto_compact_after == 0 {
            let context_window = self.context_window();
            context_window.saturating_sub(context_window / 10)
        } else {
            self.auto_compact_after
        }
    }

    pub fn context_tokens(&self) -> usize {
        estimate_messages(&self.messages.read())
            + self
                .system_prompt
                .as_deref()
                .map(crate::compaction::estimate_tokens)
                .unwrap_or(0)
    }

    fn compaction_config(&self) -> CompactionConfig {
        if self.auto_compact_after == 0 {
            let context_window = self.context_window();
            let reserve = context_window / 10;
            CompactionConfig::new(context_window, reserve, reserve)
        } else {
            let reserve = (self.auto_compact_after / 4).max(32);
            CompactionConfig::new(self.auto_compact_after + reserve, reserve, reserve)
        }
    }

    /// Run a prompt through the agent loop.
    /// Streams events to subscribers, executes tools, cycles turns.
    pub async fn prompt(&mut self, text: &str) -> Result<(), AgentError> {
        let provider = self.provider.clone().ok_or(AgentError::NoProvider)?;
        let tokens = self.context_tokens();
        let context_window = self.context_window();
        let auto_compact_at = self.auto_compact_threshold();
        self.emit(Event::ContextUsage {
            used_tokens: tokens,
            context_window,
            auto_compact_at,
        });
        if tokens >= auto_compact_at {
            if let Err(error) = self
                .compact_semantically("auto-compact before prompt", provider.as_ref())
                .await
            {
                warn!("automatic context compaction failed: {error}");
            }
        }

        let redactor = crate::secrets::Redactor::new();
        let safe_text = redactor.redact(text);

        // Inject activated skill instructions into system prompt for this turn.
        #[cfg(feature = "skills")]
        let active_skills = if let Some(reg) = &self.skill_registry {
            let matched = reg.match_prompt(&safe_text);
            for skill in &matched {
                self.emit(Event::SkillActivated {
                    id: skill.id.clone(),
                    name: skill.name.clone(),
                });
            }
            let activated: Vec<String> = matched
                .into_iter()
                .map(|skill| skill.instructions.clone())
                .collect();
            (!activated.is_empty()).then(|| activated.join("\n\n---\n\n"))
        } else {
            None
        };
        #[cfg(not(feature = "skills"))]
        let active_skills: Option<String> = None;

        self.messages.write().push(Message::user(safe_text.clone()));
        self.emit(Event::AgentStart);
        self.budget_start = Some(Instant::now());

        // Route the incoming user event through the personality turn router.
        // This evaluates hard rules (mentions, commands, rate limits, consecutive
        // turns) + learned policy, records the decision, and derives social
        // signals — all automatically before the first turn.
        #[cfg(feature = "personality")]
        if let Some(pers) = &self.personality {
            let event = crate::personality::ConversationEvent {
                epoch: 0,
                participant: "user".to_string(),
                event_kind: "message".to_string(),
                content: safe_text.chars().take(500).collect(),
            };
            match pers.route_event(&event).await {
                Ok(result) => {
                    debug!(
                        "personality router: {:?} via {} (confidence {}bps) — {}",
                        result.decision.action,
                        result.decision.strategy,
                        result.decision.confidence_basis_points,
                        result.decision.rationale
                    );
                }
                Err(error) => {
                    warn!("personality routing failed: {error}");
                }
            }
        }

        let mut tool_ctx = self.tool_context();
        tool_ctx.provider = Some(provider.clone());
        tool_ctx.tools = Some(Arc::clone(&self.tools));
        let pending_scope = Arc::new(parking_lot::Mutex::new(None));
        tool_ctx.pending_scope = Some(Arc::clone(&pending_scope));
        let ctx = Arc::new(tool_ctx);

        #[cfg(feature = "zkr-memory")]
        let mut tool_error_seen = false;

        // Loop detection and self-healing are per-turn: a fresh observer each
        // `prompt()` so a repeat in one turn is not counted against the next,
        // and a fresh attempt budget so a healed turn does not exhaust the
        // allowance for later ones.
        let mut guardrails = self.guardrails.clone().map(ToolGuardrails::new);
        let mut self_healing = self.self_healing.clone();
        let mut plan_approved = false;

        for iteration in 0..self.max_tool_iterations {
            if let Some(reason) = self.check_budget() {
                self.emit(Event::BudgetExceeded {
                    reason: reason.clone(),
                });
                return Err(AgentError::BudgetExceeded(reason));
            }
            self.emit(Event::TurnStart { turn: iteration });

            let messages: Vec<Message> = self.messages.read().clone();
            let base_system =
                append_active_skills(self.system_prompt.clone(), active_skills.as_deref());
            #[cfg(feature = "zkr-memory")]
            let system = if let Some(improve) = &self.self_improve {
                let base = base_system.as_deref().unwrap_or("");
                match improve.augment(&safe_text, base).await {
                    Ok(augmented) => Some(augmented),
                    Err(error) => {
                        warn!("self-improve augmentation failed: {error}");
                        base_system.clone()
                    }
                }
            } else {
                base_system
            };
            #[cfg(not(feature = "zkr-memory"))]
            let system = base_system;

            // Personality augmentation chains after self-improve (or base prompt).
            #[cfg(feature = "personality")]
            let system = if let Some(pers) = &self.personality {
                let base = system.as_deref().unwrap_or("");
                match pers.augment(&safe_text, base).await {
                    Ok(augmented) => Some(augmented),
                    Err(error) => {
                        warn!("personality augmentation failed: {error}");
                        system
                    }
                }
            } else {
                system
            };

            #[cfg_attr(not(feature = "providers"), allow(unused_mut))]
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut assistant_content;
            #[cfg_attr(not(feature = "providers"), allow(unused_mut))]
            let mut provider_usage: Option<TokenUsage> = None;

            self.emit(Event::MessageStart {
                role: Role::Assistant,
            });

            #[cfg(feature = "providers")]
            {
                assistant_content = String::new();
                use crate::provider::StreamEvent;
                use futures::StreamExt;
                let mut attempts = 0;
                let stream = loop {
                    let result = ctx
                        .cancellation
                        .run(provider.stream(
                            &messages,
                            &system,
                            &self.model,
                            &self.tools.definitions(),
                            self.reasoning_effort.as_deref(),
                        ))
                        .await
                        .map_err(|_| AgentError::Cancelled)?;
                    match result {
                        Ok(stream) => break stream,
                        Err(e) if e.is_transient() && attempts < 2 => {
                            attempts += 1;
                            ctx.cancellation
                                .run(tokio::time::sleep(std::time::Duration::from_millis(
                                    250 * (1 << attempts),
                                )))
                                .await
                                .map_err(|_| AgentError::Cancelled)?;
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
                    let next = ctx
                        .cancellation
                        .run(stream.next())
                        .await
                        .map_err(|_| AgentError::Cancelled)?;
                    let Some(event_result) = next else {
                        break;
                    };
                    match event_result {
                        Ok(StreamEvent::Delta(delta)) => {
                            assistant_content.push_str(&delta);
                            // Deltas are emitted after the complete assistant
                            // response is redacted below. This prevents a
                            // credential split across provider chunks from
                            // leaking through the streaming event path.
                        }
                        Ok(StreamEvent::ToolCall(call)) => {
                            tool_calls.push(call.clone());
                            self.emit(Event::ToolSource {
                                tool: call.name.clone(),
                                source: tool_source(&call.name),
                            });
                            self.emit(Event::ToolCall(redact_tool_call(&call)));
                        }
                        Ok(StreamEvent::Usage(usage)) => {
                            provider_usage = Some(usage);
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

            let redacted_assistant = redactor.redact(&assistant_content);
            if !redacted_assistant.is_empty() {
                self.emit(Event::MessageDelta {
                    delta: redacted_assistant.clone(),
                });
            }
            assistant_content = redacted_assistant;
            self.emit(Event::MessageEnd {
                role: Role::Assistant,
                content: assistant_content.clone(),
            });

            self.messages.write().push(Message::assistant_with_tools(
                assistant_content.clone(),
                tool_calls.clone(),
            ));

            let input_tokens = estimate_messages(&messages)
                + system
                    .as_deref()
                    .map(crate::compaction::estimate_tokens)
                    .unwrap_or(0);
            let output_tokens = crate::compaction::estimate_tokens(&assistant_content);
            let estimated = provider_usage.is_none();
            let usage = provider_usage.unwrap_or(TokenUsage {
                input_tokens,
                output_tokens,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            });
            if !estimated {
                self.cache_stats.record_tokens(usage);
            }
            self.session_cost
                .record(&self.model, usage, &self.pricing_registry);
            self.emit(Event::Usage {
                model: self.model.clone(),
                usage,
                estimated,
            });
            self.emit(Event::ContextUsage {
                used_tokens: self.context_tokens(),
                context_window,
                auto_compact_at,
            });
            if let Some(reason) = self.check_budget() {
                self.emit(Event::BudgetExceeded {
                    reason: reason.clone(),
                });
                return Err(AgentError::BudgetExceeded(reason));
            }

            if tool_calls.is_empty() {
                self.emit(Event::TurnEnd { turn: iteration });

                #[cfg(feature = "zkr-memory")]
                if let Some(improve) = &self.self_improve {
                    let outcome = if tool_error_seen { "error" } else { "success" };
                    let lesson = if tool_error_seen {
                        "avoid repeating the failing tool"
                    } else {
                        "continue the current strategy"
                    };
                    if let Err(error) = improve
                        .record(&safe_text, &assistant_content, outcome, lesson)
                        .await
                    {
                        warn!("self-improve reflection failed: {error}");
                    }
                }

                #[cfg(feature = "personality")]
                if let Some(pers) = &self.personality {
                    let epoch = (iteration + 1) as u64;

                    // Record the assistant's response as a conversation event.
                    // Signals are derived automatically inside record_event.
                    let assistant_event = crate::personality::ConversationEvent {
                        epoch,
                        participant: "agent".to_string(),
                        event_kind: if tool_error_seen { "error" } else { "message" }.to_string(),
                        content: assistant_content.chars().take(500).collect(),
                    };
                    if let Err(error) = pers.record_event(&assistant_event).await {
                        warn!("personality assistant event recording failed: {error}");
                    }

                    // Assess risk of the candidate reply toward the user.
                    let risk = pers.assess_risk("user", &assistant_content).await;
                    if risk.recommendation == crate::personality::RiskRecommendation::Abort {
                        warn!(
                            "personality risk assessment: ABORT (overall {}bps) — {:?}",
                            risk.overall_risk_basis_points, risk
                        );
                    } else if risk.recommendation == crate::personality::RiskRecommendation::Refine
                    {
                        debug!(
                            "personality risk assessment: REFINE (overall {}bps)",
                            risk.overall_risk_basis_points
                        );
                    }

                    // Record a ToM hypothesis about the user based on this turn.
                    let hyp = crate::personality::MindHypothesis {
                        participant: "user".to_string(),
                        belief: format!(
                            "user sent: {}",
                            safe_text.chars().take(100).collect::<String>()
                        ),
                        emotion: if tool_error_seen {
                            Some("frustrated".into())
                        } else {
                            None
                        },
                        goal: None,
                        predicted_reaction: Some(
                            if tool_error_seen {
                                "likely frustrated by errors"
                            } else {
                                "likely satisfied with response"
                            }
                            .into(),
                        ),
                        confidence_basis_points: if tool_error_seen { 4000 } else { 7000 },
                        valid_until: None,
                    };
                    if let Err(error) = pers.record_hypothesis(&hyp).await {
                        warn!("personality ToM recording failed: {error}");
                    }
                }

                break;
            }

            // ── Whole-plan gate ──
            // Consulted once, before the first tool of the turn executes. A
            // `Revise` answer loops back to the model without running
            // anything, so the host can redirect the approach rather than
            // approving or killing it.
            if !plan_approved {
                if let Some(gate) = self.plan_approver.clone() {
                    let proposal = PlanProposal {
                        prompt: safe_text.clone(),
                        plan: assistant_content.clone(),
                        calls: tool_calls.iter().map(redact_tool_call).collect(),
                        turn: iteration,
                    };
                    self.emit(Event::PlanProposed(proposal.clone()));
                    let decision = gate.approve_plan(&proposal).await;
                    self.emit(Event::PlanDecided {
                        decision: decision.clone(),
                    });
                    match decision {
                        PlanDecision::Approve => plan_approved = true,
                        PlanDecision::Reject(reason) => {
                            info!("plan rejected: {reason}");
                            self.messages.write().push(Message::user(format!(
                                "The plan was rejected: {reason}. Do not run it."
                            )));
                            self.emit(Event::TurnEnd { turn: iteration });
                            break;
                        }
                        PlanDecision::Revise(guidance) => {
                            info!("plan revision requested: {guidance}");
                            self.messages.write().push(Message::user(format!(
                                "Do not run that plan. Revise it: {guidance}"
                            )));
                            self.emit(Event::TurnEnd { turn: iteration });
                            continue;
                        }
                    }
                } else {
                    plan_approved = true;
                }
            }

            let results = self.execute_tools_parallel(&tool_calls, &ctx).await;
            for result in &results {
                #[cfg(feature = "zkr-memory")]
                {
                    tool_error_seen |= result.is_error;
                }
                self.messages
                    .write()
                    .push(Message::tool(&result.id, &result.content));
            }
            if let Some(scope) = pending_scope.lock().take() {
                self.set_scope(scope);
            }

            // ── Loop detection ──
            // Observe every call this turn made. A warning is fed back to the
            // model so it can change approach; a stop ends the turn, because
            // by then the model has demonstrated it will not.
            let mut stopped: Option<(String, String)> = None;
            if let Some(rails) = guardrails.as_mut() {
                for (call, result) in tool_calls.iter().zip(results.iter()) {
                    match rails.observe(&call.name, &call.arguments, result.is_error) {
                        GuardrailDecision::Proceed => {}
                        GuardrailDecision::Warn(reason) => {
                            warn!("guardrail warning on '{}': {reason}", call.name);
                            self.emit(Event::GuardrailWarning {
                                tool: call.name.clone(),
                                reason: reason.clone(),
                            });
                            self.messages
                                .write()
                                .push(Message::user(format!("Guardrail warning: {reason}")));
                        }
                        GuardrailDecision::Stop(reason) => {
                            warn!("guardrail stop on '{}': {reason}", call.name);
                            stopped = Some((call.name.clone(), reason));
                            break;
                        }
                    }
                }
            }
            if let Some((tool, reason)) = stopped {
                self.emit(Event::GuardrailStop {
                    tool,
                    reason: reason.clone(),
                });
                self.messages
                    .write()
                    .push(Message::user(format!("Stopped by guardrail: {reason}")));
                self.emit(Event::TurnEnd { turn: iteration });
                break;
            }

            // ── Self-healing ──
            // The model already sees the failing tool results; this adds the
            // explicit "try a different approach" nudge, budgeted so a
            // genuinely stuck turn still terminates.
            let errors: Vec<String> = results
                .iter()
                .filter(|r| r.is_error)
                .map(|r| r.content.clone())
                .collect();
            if !errors.is_empty() {
                if let Some(healer) = self_healing.as_mut() {
                    if healer.should_retry() {
                        let message = healer.build_healing_message(&errors);
                        self.emit(Event::SelfHealing {
                            attempt: healer.attempts_used,
                            max_attempts: healer.max_attempts,
                            errors,
                        });
                        self.messages.write().push(Message::user(message));
                    }
                }
            }

            self.emit(Event::TurnEnd { turn: iteration });
        }

        // Personality observability: analyze the conversation window after the
        // prompt completes. Computes participation balance, error rate, and
        // generates evidence-cited findings with recommendations.
        #[cfg(feature = "personality")]
        if let Some(pers) = &self.personality {
            let scope = format!(
                "prompt-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            match pers.analyze_conversation(&scope).await {
                Ok(health) => {
                    if !health.findings.is_empty() {
                        info!(
                            "personality observability: {} findings for {} (balance={:.2}, error_rate={:.2})",
                            health.findings.len(),
                            health.scope,
                            health.participation_balance,
                            health.error_rate
                        );
                    }
                }
                Err(error) => {
                    warn!("personality observability analysis failed: {error}");
                }
            }
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
        let mut join_failures: Vec<Option<String>> = vec![None; calls.len()];

        for batch in batches {
            if batch.len() == 1 {
                let idx = batch[0];
                let original = &calls[idx];
                self.emit(Event::ToolExecutionStart(redact_tool_call(original)));
                let (call, result) = self.execute_single_tool(original, ctx).await;
                if result.requires_approval() {
                    self.emit(Event::ApprovalRequired(
                        crate::permissions::ApprovalRequest::from_call(
                            &redact_tool_call(&call),
                            &self.policy,
                        ),
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
            let async_approver = self.async_approver.clone();
            let authorizer = self.authorizer.clone();
            let tool_cache = self.tool_cache.clone();
            let mut join_set = tokio::task::JoinSet::new();
            let mut task_ids = std::collections::HashMap::new();

            for idx in batch {
                let original = &calls[idx];
                let call = match self.apply_before_tool_hooks(original) {
                    Ok(c) => c,
                    Err(reason) => {
                        self.emit(Event::ToolExecutionStart(redact_tool_call(original)));
                        let result = ToolResult::err(&original.id, reason);
                        self.emit(Event::ToolExecutionEnd(result.clone()));
                        results[idx] = Some(result);
                        continue;
                    }
                };
                self.emit(Event::ToolExecutionStart(redact_tool_call(&call)));
                let ctx = Arc::clone(ctx);
                let tools = Arc::clone(&tools);
                let policy = policy.clone();
                let scope_profile = scope_profile.clone();
                let approver = approver.clone();
                let async_approver = async_approver.clone();
                let authorizer = authorizer.clone();
                let tool_cache = tool_cache.clone();
                let handle = join_set.spawn(async move {
                    let result = Agent::run_tool_call(
                        &tools,
                        &policy,
                        authorizer.as_deref(),
                        scope_profile.as_ref(),
                        approver.clone(),
                        async_approver.as_deref(),
                        &tool_cache,
                        &call,
                        &ctx,
                    )
                    .await;
                    (idx, call, result)
                });
                task_ids.insert(handle.id(), idx);
            }

            while let Some(joined) = join_set.join_next_with_id().await {
                match joined {
                    Ok((_id, (idx, call, result))) => {
                        if result.requires_approval() {
                            self.emit(Event::ApprovalRequired(
                                crate::permissions::ApprovalRequest::from_call(
                                    &redact_tool_call(&call),
                                    &self.policy,
                                ),
                            ));
                        }
                        self.emit(Event::ToolExecutionEnd(result.clone()));
                        results[idx] = Some(result);
                    }
                    Err(e) => {
                        warn!("parallel tool task join error: {e}");
                        if let Some(&idx) = task_ids.get(&e.id()) {
                            join_failures[idx] = Some(format!("parallel tool task failed: {e}"));
                        }
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
                        join_failures[i]
                            .as_deref()
                            .unwrap_or("tool execution failed"),
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
            self.approver.clone(),
            self.async_approver.as_deref(),
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
        approver: Option<Arc<dyn Approver>>,
        async_approver: Option<&dyn AsyncApprover>,
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

        // Policy evaluate without Approver (pi: beforeToolCall is separate async gate).
        let mut decision = match authorizer {
            Some(auth) => auth.authorize(
                policy,
                &resolved_name,
                &call.arguments,
                None,
                Some(ctx.workspace_root.as_path()),
            ),
            None => PolicyAuthorizer::new().authorize(
                policy,
                &resolved_name,
                &call.arguments,
                None,
                Some(ctx.workspace_root.as_path()),
            ),
        };
        if decision == Decision::Ask {
            let ask_call = ToolCall {
                id: call.id.clone(),
                name: resolved_name.clone(),
                arguments: call.arguments.clone(),
            };
            if let Some(app) = async_approver {
                decision = app.approve(&ask_call).await;
            } else if let Some(app) = approver {
                // Offload blocking Approver so parallel JoinSet workers do not
                // stall the multi-thread runtime (ChannelApprover uses recv).
                decision = tokio::task::spawn_blocking(move || app.approve(&ask_call))
                    .await
                    .unwrap_or(Decision::Deny);
            }
        }

        match decision {
            Decision::Deny => ToolResult::err(&call.id, "denied by policy"),
            Decision::Ask => {
                // No Approver, or Approver returned Ask: tool fails this turn.
                // Prefer AsyncApprover / ChannelApprover for interactive Allow.
                ToolResult::approval_required(&call.id)
            }
            Decision::Allow => {
                let effect = tools.effect_of(&resolved_name);
                let cache_key = format!(
                    "{}:{}:{}",
                    ctx.workspace_root.display(),
                    resolved_name,
                    call.arguments
                );
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
                // Tools stamp name as id; providers need tool_call_id.
                result.id = call.id.clone();

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
        if self.message_count() <= 2 {
            return;
        }
        let before_tokens = self.context_tokens();
        self.emit(Event::CompactionStart {
            reason: reason.to_string(),
            before_tokens,
        });
        let result = {
            let mut msgs = self.messages.write();
            let result = apply_compaction(&mut msgs, &self.compaction_config());
            if !result.summary.is_empty() {
                msgs.push(Message::system(format!("[compact reason: {reason}]")));
            }
            result
        };
        self.emit(Event::CompactionEnd {
            reason: reason.to_string(),
            result,
        });
    }

    fn tool_context(&self) -> ToolContext {
        let mut tool_ctx = ToolContext::new(self.workspace_root.clone());
        tool_ctx.os_sandbox_required = self.policy.enable_os_sandbox && self.os_sandbox.is_none();
        tool_ctx.cancellation = self.turn_cancellation.reset();
        #[cfg(feature = "ipc")]
        {
            tool_ctx.lsp = Some(Arc::clone(&self.lsp));
        }
        if let Some(sandbox) = self.sandbox.clone() {
            tool_ctx = tool_ctx.with_sandbox(sandbox);
        }
        if let Some(os_sandbox) = self.os_sandbox.clone() {
            tool_ctx = tool_ctx.with_os_sandbox(os_sandbox);
        }
        tool_ctx
    }

    async fn compact_semantically(
        &self,
        reason: &str,
        provider: &dyn Provider,
    ) -> Result<(), crate::provider::ProviderError> {
        info!("compacting context: {reason}");
        if self.message_count() <= 2 {
            return Ok(());
        }
        let before_tokens = self.context_tokens();
        let snapshot = self.messages.read().clone();
        let result = compact_messages_semantically(
            &snapshot,
            &self.compaction_config(),
            provider,
            &self.model,
        )
        .await?;
        if result.removed_count == 0 {
            return Ok(());
        }
        {
            let mut messages = self.messages.write();
            if !apply_compaction_result(&mut messages, &snapshot, &result) {
                return Ok(());
            }
            messages.push(Message::system(format!("[compact reason: {reason}]")));
        }
        self.emit(Event::CompactionStart {
            reason: reason.to_string(),
            before_tokens,
        });
        self.emit(Event::CompactionEnd {
            reason: reason.to_string(),
            result,
        });
        Ok(())
    }
}

#[cfg(any(feature = "providers", test))]
fn tool_source(name: &str) -> ToolSource {
    if let Some(rest) = name.strip_prefix("mcp__") {
        return ToolSource::Mcp {
            server: rest.split("__").next().unwrap_or(rest).to_string(),
        };
    }
    if name.starts_with("cu_") {
        return ToolSource::ComputerUse;
    }
    ToolSource::Builtin
}

fn redact_tool_call(call: &ToolCall) -> ToolCall {
    ToolCall {
        id: call.id.clone(),
        name: call.name.clone(),
        arguments: crate::secrets::Redactor::new().redact(&call.arguments),
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

    #[test]
    fn default_compaction_threshold_tracks_model_window() {
        let mut agent = Agent::new();
        agent.set_model("gemini-2.0-flash");
        assert_eq!(agent.context_window(), 1_048_576);
        assert_eq!(agent.auto_compact_threshold(), 943_719);
    }

    #[test]
    fn explicit_compaction_threshold_is_preserved() {
        let mut agent = Agent::new();
        agent.auto_compact_after = 50;
        assert_eq!(agent.auto_compact_threshold(), 50);
    }

    #[test]
    fn compaction_emits_lifecycle() {
        let mut agent = Agent::new();
        agent.auto_compact_after = 50;
        agent
            .messages
            .write()
            .extend((0..10).map(|_| Message::user("x".repeat(100))));
        let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let received = Arc::clone(&events);
        agent.subscribe(move |event| {
            received.lock().push(event.clone());
        });
        agent.compact("test");
        let events = events.lock();
        assert!(events
            .iter()
            .any(|event| matches!(event, Event::CompactionStart { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, Event::CompactionEnd { .. })));
    }

    #[test]
    fn tool_sources_are_classified() {
        assert_eq!(tool_source("read"), ToolSource::Builtin);
        assert_eq!(tool_source("cu_click"), ToolSource::ComputerUse);
        assert_eq!(
            tool_source("mcp__supabase__query"),
            ToolSource::Mcp {
                server: "supabase".into()
            }
        );
    }

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

    #[test]
    fn set_scope_replaces_the_previous_scope_prompt() {
        let mut agent = Agent::new();
        agent.set_system_prompt("host instructions");
        agent.set_scope(Scope::Plan);
        assert!(agent
            .system_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("multi-step plan")));
        agent.set_scope(Scope::Research);
        let prompt = agent.system_prompt.as_deref().expect("system prompt");
        assert!(prompt.contains("Explore and explain"));
        assert!(!prompt.contains("multi-step plan"));
        assert_eq!(prompt.matches("host instructions").count(), 1);
    }

    #[test]
    fn changing_workspace_refreshes_custom_sandbox_and_cache_boundary() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let mut agent = Agent::new();
        let mut sandbox = crate::sandbox::SandboxManager::new(
            crate::sandbox::SandboxProfile::Custom,
            first.path().to_path_buf(),
        );
        sandbox.set_allow_network(false);
        agent.set_sandbox(Arc::new(sandbox));
        agent.set_authorizer(Arc::new(crate::permissions::PolicyAuthorizer::new()));

        agent.set_workspace_root(second.path());

        let current = agent.sandbox.as_ref().expect("sandbox attached");
        assert_eq!(current.workspace_root(), second.path());
        assert!(current.validate_network().is_err());
        assert_eq!(agent.tool_cache.entry_count(), 0);
        assert!(agent.authorizer.is_none());
    }

    #[test]
    fn active_skills_are_added_to_a_turn_prompt_without_mutating_the_base() {
        let base = Some("host instructions".to_string());
        let prompt = append_active_skills(base.clone(), Some("skill instructions"));
        assert!(prompt
            .as_deref()
            .is_some_and(|value| value.contains("# Active Skills")));
        assert_eq!(base.as_deref(), Some("host instructions"));
    }

    #[cfg(feature = "ipc")]
    #[test]
    fn agent_tool_context_gets_lsp_manager() {
        let agent = Agent::new();
        let tool_ctx = agent.tool_context();
        assert!(tool_ctx.lsp.is_some());
    }

    #[tokio::test]
    async fn tool_result_id_matches_call_id() {
        let mut tools = ToolRegistry::new();
        tools.register(
            ToolDefinition::new_boxed(
                "echo_id",
                "echo",
                "{}",
                Box::new(|_ctx, _args| Box::pin(async { ToolResult::ok("wrong-id", "ok") })),
            )
            .with_effect(ToolEffect::Read),
        );
        let mut agent = Agent::new();
        agent.set_policy(Policy::full_access());
        agent.tools = std::sync::Arc::new(tools);
        let ctx = std::sync::Arc::new(ToolContext::new(agent.workspace_root.clone()));
        let call = ToolCall {
            id: "call_xyz".into(),
            name: "echo_id".into(),
            arguments: "{}".into(),
        };
        let (_c, result) = agent.execute_single_tool(&call, &ctx).await;
        assert_eq!(result.id, "call_xyz");
        assert_eq!(result.content, "ok");
    }

    #[test]
    fn approval_required_results_are_typed() {
        let result = ToolResult::approval_required("call_approval");
        assert!(result.requires_approval());
        assert_eq!(result.error_kind, Some(ToolErrorKind::ApprovalRequired));
    }

    // === Security regression tests ===

    #[tokio::test]
    async fn h1_os_sandbox_required_flag_blocks_bash() {
        // When policy requires OS sandbox but runner is absent, bash must be blocked.
        let mut agent = Agent::new();
        agent.set_policy(Policy::workspace_write()); // enable_os_sandbox = true
                                                     // Simulate failed sandbox setup.
        agent.os_sandbox_failed = true;
        let ctx = std::sync::Arc::new({
            let mut tc = ToolContext::new(agent.workspace_root.clone());
            tc.os_sandbox_required = true;
            tc
        });
        let result = crate::tools::fs::exec_bash(ctx, r#"{"command":"echo hi"}"#.to_string());
        let result = result.await;
        assert!(result.is_error);
        assert!(result.content.contains("OS sandbox required"));
    }

    #[test]
    fn messages_handle_shares_the_agent_history() {
        let agent = Agent::new();
        let handle = agent.messages_handle();
        handle.write().push(Message::user("from host"));
        assert_eq!(agent.message_count(), 1);
        agent
            .messages
            .write()
            .push(Message::assistant("from agent"));
        assert_eq!(handle.read().len(), 2);
        agent.clear_messages();
        assert!(handle.read().is_empty());
    }

    #[cfg(feature = "providers")]
    struct SteeringProvider {
        handle: Arc<RwLock<Vec<Message>>>,
        calls: Arc<parking_lot::Mutex<Vec<Vec<String>>>>,
    }

    #[cfg(feature = "providers")]
    #[async_trait::async_trait]
    impl crate::provider::Provider for SteeringProvider {
        fn id(&self) -> &str {
            "steering"
        }

        fn name(&self) -> &str {
            "steering"
        }

        async fn stream(
            &self,
            messages: &[Message],
            _system: &Option<String>,
            _model: &str,
            _tools: &[serde_json::Value],
            _reasoning_effort: Option<&str>,
        ) -> Result<crate::provider::StreamResult, crate::provider::ProviderError> {
            let seen: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();
            let first = {
                let mut calls = self.calls.lock();
                calls.push(seen);
                calls.len() == 1
            };
            if first {
                // Host steers mid-turn through the shared handle.
                self.handle.write().push(Message::user("steer"));
                Ok(Box::new(futures::stream::iter([
                    Ok(crate::provider::StreamEvent::ToolCall(ToolCall {
                        id: "call_1".into(),
                        name: "noop".into(),
                        arguments: "{}".into(),
                    })),
                    Ok(crate::provider::StreamEvent::Done),
                ])))
            } else {
                Ok(Box::new(futures::stream::iter([Ok(
                    crate::provider::StreamEvent::Done,
                )])))
            }
        }
    }

    #[cfg(feature = "providers")]
    #[tokio::test]
    async fn handle_append_is_seen_mid_turn() {
        let mut registry = ToolRegistry::new();
        registry.register(
            ToolDefinition::new_boxed(
                "noop",
                "noop",
                "{}",
                Box::new(|_ctx, _args| Box::pin(async { ToolResult::ok("call_1", "ok") })),
            )
            .with_effect(ToolEffect::Read),
        );
        let mut agent = Agent::new();
        agent.set_tools(registry);
        agent.set_policy(Policy::full_access());
        let calls = Arc::new(parking_lot::Mutex::new(Vec::new()));
        agent.set_provider(Arc::new(SteeringProvider {
            handle: agent.messages_handle(),
            calls: Arc::clone(&calls),
        }));
        agent.prompt("hello").await.unwrap();
        let calls = calls.lock();
        assert!(calls.len() >= 2, "expected a second tool iteration");
        assert!(!calls[0].iter().any(|c| c == "steer"));
        assert!(
            calls[1].iter().any(|c| c == "steer"),
            "mid-turn append not observed on the next iteration: {:?}",
            calls[1]
        );
    }

    // ── Guardrails, self-healing and the plan gate ───────────────────────

    /// A provider that keeps asking for the same tool call, so a turn only
    /// ends when the loop itself decides to stop it.
    #[cfg(feature = "providers")]
    struct RepeatingProvider {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        limit: usize,
    }

    #[cfg(feature = "providers")]
    #[async_trait::async_trait]
    impl crate::provider::Provider for RepeatingProvider {
        fn id(&self) -> &str {
            "repeating"
        }

        fn name(&self) -> &str {
            "repeating"
        }

        async fn stream(
            &self,
            _messages: &[Message],
            _system: &Option<String>,
            _model: &str,
            _tools: &[serde_json::Value],
            _reasoning_effort: Option<&str>,
        ) -> Result<crate::provider::StreamResult, crate::provider::ProviderError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n >= self.limit {
                return Ok(Box::new(futures::stream::iter([Ok(
                    crate::provider::StreamEvent::Done,
                )])));
            }
            Ok(Box::new(futures::stream::iter([
                Ok(crate::provider::StreamEvent::Delta("working on it".into())),
                Ok(crate::provider::StreamEvent::ToolCall(ToolCall {
                    id: "call_1".into(),
                    name: "flaky".into(),
                    arguments: "{}".into(),
                })),
                Ok(crate::provider::StreamEvent::Done),
            ])))
        }
    }

    /// Builds an agent whose only tool always fails, wired to a provider that
    /// will keep retrying it forever unless something intervenes.
    #[cfg(feature = "providers")]
    fn looping_agent(
        limit: usize,
    ) -> (
        Agent,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let tool_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let runs = Arc::clone(&tool_runs);
        let mut registry = ToolRegistry::new();
        registry.register(
            ToolDefinition::new_boxed(
                "flaky",
                "always fails",
                "{}",
                Box::new(move |_ctx, _args| {
                    let runs = Arc::clone(&runs);
                    Box::pin(async move {
                        runs.fetch_add(1, Ordering::SeqCst);
                        ToolResult::err("call_1", "disk on fire")
                    })
                }),
            )
            .with_effect(ToolEffect::Read),
        );
        let mut agent = Agent::new();
        agent.set_tools(registry);
        agent.set_policy(Policy::full_access());
        agent.max_tool_iterations = 12;
        let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        agent.set_provider(Arc::new(RepeatingProvider {
            calls: Arc::clone(&provider_calls),
            limit,
        }));
        (agent, tool_runs, provider_calls)
    }

    /// Collects event labels so a test can assert on what the host would see.
    #[cfg(feature = "providers")]
    fn event_sink(agent: &mut Agent) -> Arc<parking_lot::Mutex<Vec<String>>> {
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        agent.subscribe(move |event| {
            let label = match event {
                Event::GuardrailWarning { tool, .. } => format!("warn:{tool}"),
                Event::GuardrailStop { tool, .. } => format!("stop:{tool}"),
                Event::SelfHealing { attempt, .. } => format!("heal:{attempt}"),
                Event::PlanProposed(p) => format!("plan_proposed:{}", p.calls.len()),
                Event::PlanDecided { decision } => format!("plan_decided:{decision:?}"),
                _ => return,
            };
            sink.lock().push(label);
        });
        seen
    }

    #[cfg(feature = "providers")]
    fn message_texts(agent: &Agent) -> Vec<String> {
        agent
            .messages
            .read()
            .iter()
            .map(|m| m.content.clone())
            .collect()
    }

    /// Default behaviour must not change: with nothing configured the loop
    /// runs exactly as it did before guardrails existed.
    #[cfg(feature = "providers")]
    #[tokio::test]
    async fn defaults_leave_the_loop_untouched() {
        let (mut agent, tool_runs, _) = looping_agent(3);
        let seen = event_sink(&mut agent);
        agent.prompt("go").await.unwrap();
        assert_eq!(tool_runs.load(Ordering::SeqCst), 3);
        assert!(
            seen.lock().is_empty(),
            "unconfigured agent emitted guardrail events: {:?}",
            seen.lock()
        );
    }

    /// A repeated identical call must warn, and the warning must reach the
    /// model — a warning the model never sees cannot change its behaviour.
    #[cfg(feature = "providers")]
    #[tokio::test]
    async fn guardrails_warn_on_a_repeated_call_and_tell_the_model() {
        let (mut agent, _, _) = looping_agent(4);
        agent.set_guardrails(GuardrailConfig {
            warnings_enabled: true,
            hard_stop_enabled: false,
            same_tool_failure_warn_after: 1,
            ..GuardrailConfig::default()
        });
        let seen = event_sink(&mut agent);
        agent.prompt("go").await.unwrap();

        assert!(
            seen.lock().iter().any(|l| l == "warn:flaky"),
            "no guardrail warning: {:?}",
            seen.lock()
        );
        assert!(
            message_texts(&agent)
                .iter()
                .any(|m| m.starts_with("Guardrail warning:")),
            "the warning never reached the model"
        );
    }

    /// A hard stop must end the turn early, not merely complain. The proof is
    /// that the tool stops running well before `max_tool_iterations`.
    #[cfg(feature = "providers")]
    #[tokio::test]
    async fn guardrails_stop_a_runaway_turn() {
        let (mut agent, tool_runs, _) = looping_agent(usize::MAX);
        agent.set_guardrails(GuardrailConfig {
            warnings_enabled: false,
            hard_stop_enabled: true,
            same_tool_failure_halt_after: 2,
            ..GuardrailConfig::default()
        });
        let seen = event_sink(&mut agent);
        agent.prompt("go").await.unwrap();

        assert!(
            seen.lock().iter().any(|l| l == "stop:flaky"),
            "no guardrail stop: {:?}",
            seen.lock()
        );
        let runs = tool_runs.load(Ordering::SeqCst);
        assert!(
            runs < 12,
            "guardrail did not end the turn: {runs} tool runs against a 12 iteration cap"
        );
    }

    /// A failing tool must produce an explicit re-prompt, budgeted so a turn
    /// that cannot recover still terminates.
    #[cfg(feature = "providers")]
    #[tokio::test]
    async fn self_healing_reprompts_within_its_budget() {
        let (mut agent, _, _) = looping_agent(6);
        agent.set_self_healing(2);
        let seen = event_sink(&mut agent);
        agent.prompt("go").await.unwrap();

        let heals: Vec<String> = seen
            .lock()
            .iter()
            .filter(|l| l.starts_with("heal:"))
            .cloned()
            .collect();
        assert_eq!(
            heals,
            vec!["heal:1", "heal:2"],
            "healing budget not honoured"
        );
        assert!(
            message_texts(&agent)
                .iter()
                .any(|m| m.contains("The following tool call(s) failed")),
            "no healing message reached the model"
        );
    }

    #[cfg(feature = "providers")]
    struct FixedPlanApprover(PlanDecision);

    #[cfg(feature = "providers")]
    #[async_trait::async_trait]
    impl crate::permissions::PlanApprover for FixedPlanApprover {
        async fn approve_plan(&self, _proposal: &PlanProposal) -> PlanDecision {
            self.0.clone()
        }
    }

    /// The whole point of the gate: a rejected plan runs nothing at all.
    #[cfg(feature = "providers")]
    #[tokio::test]
    async fn a_rejected_plan_executes_no_tools() {
        let (mut agent, tool_runs, _) = looping_agent(usize::MAX);
        agent.set_plan_approver(Arc::new(FixedPlanApprover(PlanDecision::Reject(
            "wrong approach".into(),
        ))));
        let seen = event_sink(&mut agent);
        agent.prompt("go").await.unwrap();

        assert_eq!(
            tool_runs.load(Ordering::SeqCst),
            0,
            "a rejected plan still ran its tools"
        );
        assert!(seen.lock().iter().any(|l| l == "plan_proposed:1"));
        assert!(
            message_texts(&agent)
                .iter()
                .any(|m| m.contains("The plan was rejected")),
            "the rejection reason never reached the model"
        );
    }

    /// An approved plan runs, and the gate is consulted once for the turn
    /// rather than before every iteration.
    #[cfg(feature = "providers")]
    #[tokio::test]
    async fn an_approved_plan_runs_and_is_gated_once() {
        let (mut agent, tool_runs, _) = looping_agent(3);
        agent.set_plan_approver(Arc::new(crate::permissions::AlwaysApprovePlan));
        let seen = event_sink(&mut agent);
        agent.prompt("go").await.unwrap();

        assert_eq!(tool_runs.load(Ordering::SeqCst), 3);
        let proposals = seen
            .lock()
            .iter()
            .filter(|l| l.starts_with("plan_proposed"))
            .count();
        assert_eq!(proposals, 1, "the plan gate fired more than once per turn");
    }

    /// `Revise` must loop back to the model without running anything, which is
    /// what separates it from `Reject`.
    #[cfg(feature = "providers")]
    #[tokio::test]
    async fn a_revised_plan_goes_back_to_the_model_unrun() {
        let (mut agent, tool_runs, provider_calls) = looping_agent(usize::MAX);
        agent.max_tool_iterations = 3;
        agent.set_plan_approver(Arc::new(FixedPlanApprover(PlanDecision::Revise(
            "use the other tool".into(),
        ))));
        agent.prompt("go").await.unwrap();

        assert_eq!(
            tool_runs.load(Ordering::SeqCst),
            0,
            "a plan awaiting revision still ran"
        );
        assert!(
            provider_calls.load(Ordering::SeqCst) > 1,
            "revision did not loop back to the model"
        );
        assert!(
            message_texts(&agent)
                .iter()
                .any(|m| m.contains("use the other tool")),
            "the revision guidance never reached the model"
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
    #[error("budget exceeded: {0}")]
    BudgetExceeded(String),
}
