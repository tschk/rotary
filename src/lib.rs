//! rx4 — the agent harness engine.
//!
//! Models write. rx4 gives them tools, memory, loops, permissions, sessions,
//! and control planes. Hosts (CLIs, TUIs, IDEs) embed rx4.
//!
//! # Safety
//!
//! This crate is `#![forbid(unsafe_code)]` — no unsafe code is allowed anywhere.

#![forbid(unsafe_code)]
//!
//! ```no_run
//! use rx4::Agent;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut agent = Agent::new();
//! agent.set_scope(rx4::Scope::Coding);
//! agent.prompt("fix the failing test").await?;
//! # Ok(())
//! # }
//! ```

pub mod agent;
#[cfg(feature = "skills")]
pub mod background_review;
pub mod compaction;
pub mod config;
pub mod context;
pub mod cost;
#[cfg(feature = "graph-memory")]
pub mod dream_scheduler;
#[cfg(feature = "skills")]
pub mod embeddings;
pub mod extract;
#[cfg(feature = "graph-memory")]
pub mod graph_memory;
pub mod guardrails;
pub mod hooks;
pub mod mode;
pub mod model_router;
pub mod multiagent;
pub mod permissions;
#[cfg(feature = "personality")]
pub mod personality;
pub mod plugin;
pub mod prompt_cache;
pub mod provider;
pub mod ranking;
pub mod repomap;
pub mod rollout;
pub mod routing;
pub mod sandbox;
#[cfg(feature = "fff")]
pub mod search;
pub mod secrets;
#[cfg(feature = "zkr-memory")]
pub mod self_improve;
pub mod session;
#[cfg(feature = "skills")]
pub mod skill_curator;
#[cfg(feature = "skills")]
pub mod skill_engine;
pub mod slash;
pub mod sse;
pub mod subagent;
pub mod tools;
pub mod work_pack;

#[cfg(feature = "providers")]
pub mod http;

#[cfg(feature = "computer-use")]
pub mod computer_use;

#[cfg(feature = "ipc")]
pub mod ipc;

#[cfg(feature = "memory")]
pub mod memory;

pub mod models;

#[cfg(feature = "mcp")]
pub mod mcp;

#[cfg(feature = "ipc")]
pub mod acp;
#[cfg(feature = "ipc")]
pub mod lsp;
pub mod marketplace;

pub use agent::{
    normalize_tool_name, Agent, AgentBudget, Event, ToolCall, ToolContext, ToolDefinition,
    ToolEffect, ToolExecuteBox, ToolExecuteFn, ToolExecutor, ToolFuture, ToolRegistry, ToolResult,
};
#[cfg(feature = "skills")]
pub use background_review::{
    BackgroundReviewConfig, BackgroundReviewer, ReviewResult, ReviewSignal,
};
pub use compaction::{
    apply_compaction, compact_messages, CompactionConfig, CompactionMarker, CompactionResult,
};
pub use context::{compose_system_prompt, load_project_instructions, ProjectInstructions};
pub use cost::{CostEntry, ModelPricing, PricingRegistry, SessionCost, TokenUsage};
#[cfg(feature = "graph-memory")]
pub use dream_scheduler::{DreamReport, DreamScheduler};
#[cfg(feature = "skills")]
pub use embeddings::{
    cosine_similarity, EmbedError, EmbeddingClient, EmbeddingConfig, EmbeddingProvider,
    SemanticSearch,
};
pub use extract::{
    extract_knowledge_loose, extract_proactive_loose, parse_knowledge, parse_proactive,
    ExtractedKnowledge, ProactiveItem,
};
#[cfg(feature = "graph-memory")]
pub use graph_memory::{
    ConversationExtractor, EdgeRelation, ExtractionResult, GraphMemory, GraphMemoryError,
    MemoryEdge as GraphMemoryEdge, MemoryNode as GraphMemoryNode, NodeType as GraphNodeType,
};
pub use guardrails::{
    classify_tool, GuardrailConfig, GuardrailDecision, SelfHealingRetry, ToolClass, ToolGuardrails,
};
pub use hooks::{HookDecision, HookEvent, HookFn, HookRegistry};
pub use mode::{Profile, Scope};
pub use model_router::{
    ModelRouter, ModelRouterError, ModelTier, ProactiveMonitor, RouterConfig, SkillSuggestion,
    SubagentModelSelector, TaskTier, TaskType,
};
pub use models::{CompatConfig, ModelInfo, ModelRegistry};
#[cfg(feature = "ipc")]
pub use multiagent::CoordinatorEvent;
pub use multiagent::{
    AgentProfile, AgentRole, MultiAgentCoordinator, MultiAgentError, TeamResult, TeamTask,
};
pub use permissions::{
    authorize, authorize_with_workspace, command_from_args, is_dangerous_shell_command,
    is_process_tool, is_read_only_tool, is_write_tool, path_outside_workspace, shell_argv,
    shell_ast, shell_command_allowed, shell_command_matches_all, shell_command_matches_any,
    shell_rule_matches, shell_segments, shell_simples, AlwaysAllow, AlwaysDeny, ApprovalRequest,
    Approver, AsyncApprover, Authorizer, ChannelApprover, ChannelAsyncApprover, Decision,
    PermissionMode, Policy, PolicyAuthorizer, ShellNode, ShellSimple,
};
pub use prompt_cache::{
    apply_cache_control, CachePoint, CachePosition, CacheProvider, CacheStats, CacheStatsTracker,
    CacheTtl, PromptCacheConfig,
};
pub use provider::{Message, Provider, ProviderRegistry, Role, StreamEvent};
pub use ranking::{rank, rank_with_query, top_n};
pub use repomap::{RepoMap, RepoMapError};
pub use rollout::{RolloutEntry, RolloutManager, TraceWriter};
pub use routing::{
    AgentRoute, AgentRouter, RoutingConfig, RoutingStats, SmartRouter, TurnComplexity,
};
pub use sandbox::{
    detect_sandbox, OsSandbox, OsSandboxConfig, OsSandboxRunner, SandboxConfig, SandboxError,
    SandboxManager, SandboxProfile, SandboxViolation,
};
pub use secrets::{
    filter_env_vars, is_sensitive_env_var, RedactionConfig, Redactor, SecretMatch, SecretPattern,
};
pub use session::Session;
#[cfg(feature = "skills")]
pub use skill_curator::{CuratorConfig, CuratorSuggestion, SkillCurator, SuggestionKind};
#[cfg(feature = "skills")]
pub use skill_engine::{
    ConfidencePrior, Skill, SkillEngine, SkillError, SkillFrontmatter, SkillOutcome, SkillRegistry,
    SkillState,
};
pub use slash::{help_text as slash_help_text, parse as parse_slash, Command as SlashCommand};
pub use sse::{SseError, SseEvent, SseParser};
pub use subagent::{
    SubagentBudget, SubagentConfig, SubagentError, SubagentHandle, SubagentLimits, SubagentManager,
    SubagentResult, SubagentStatus,
};
pub use tools::{register_builtin_tools, register_spawn_agent_tool};
pub use work_pack::{WorkPack, WorkPackError};

#[cfg(feature = "personality")]
pub use personality::{
    ConversationEvent, MindHypothesis, ObservationFinding, ObservationSeverity, PersonaBlueprint,
    Personality, SocialSignal, TurnAction, TurnDecision, VoiceCard,
};

#[cfg(feature = "mcp")]
pub use mcp::{McpClient, McpError, McpRegistry, McpResourceInfo, McpToolInfo};

#[cfg(feature = "ipc")]
pub use acp::{AcpHost, AcpSession};

pub use marketplace::{
    verify_plugin_integrity, InstalledPlugin, MarketplaceError, MarketplaceIndex, McpServerConfig,
    McpTransportKind, PluginBlocklist, PluginInstaller, PluginManifest,
};

#[cfg(feature = "providers")]
pub use http::{global_client, is_local_provider, HttpClient, TimeoutConfig};

#[cfg(feature = "ipc")]
pub use lsp::{Diagnostic, DiagnosticSeverity, Location, LspManager, LspServer};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn print_banner() {
    eprintln!("rx4 {VERSION} — agent harness engine");
}
