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
#[cfg(feature = "autoresearch")]
pub mod autoresearch;
#[cfg(feature = "autoresearch")]
pub mod autoresearch_controller;
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
#[cfg(feature = "extract")]
pub mod extract;
#[cfg(feature = "graph-memory")]
pub mod graph_memory;
pub mod guardrails;
pub mod hooks;
pub mod mode;
#[cfg(feature = "routing")]
pub mod model_router;
#[cfg(feature = "multiagent")]
pub mod multiagent;
pub mod permissions;
#[cfg(feature = "personality")]
pub mod personality;
#[cfg(feature = "marketplace")]
pub mod plugin;
pub mod prompt_cache;
pub mod provider;
#[cfg(feature = "extract")]
pub mod ranking;
pub mod repomap;
pub mod rollout;
#[cfg(feature = "routing")]
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
#[cfg(feature = "sse")]
pub mod sse;
pub mod subagent;
pub mod todo;
pub mod tools;
#[cfg(feature = "work-pack")]
pub mod work_pack;

#[cfg(feature = "providers")]
pub mod http;

#[cfg(feature = "computer-use")]
pub mod computer_use;
#[cfg(feature = "computer-use")]
mod computer_use_bridge;

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
#[cfg(feature = "marketplace")]
pub mod marketplace;

pub use agent::{
    normalize_tool_name, Agent, AgentBudget, CacheAudit, CacheDivergence, Event, GateResult,
    MemoryRecall, QualityGateConfig, SemanticEmbedder, SemanticRecallConfig, ToolCall, ToolContext,
    ToolDefinition, ToolEffect, ToolErrorKind, ToolExecuteBox, ToolExecuteFn, ToolExecutor,
    ToolFuture, ToolRegistry, ToolResult, TurnEndMetadata,
};
#[cfg(feature = "autoresearch")]
pub use autoresearch::{
    new_handle as new_autoresearch_handle, parse_metrics, AutoresearchConfig, AutoresearchError,
    AutoresearchHandle, AutoresearchSession, ExperimentMeasurement, ExperimentRun,
    ExperimentStatus, MetricDirection,
};
#[cfg(feature = "autoresearch")]
pub use autoresearch_controller::{
    new_controller_handle, AggregatedMeasurement, AutoresearchBudget, AutoresearchCancellation,
    AutoresearchCompletion, AutoresearchController, AutoresearchControllerConfig,
    AutoresearchControllerError, AutoresearchControllerHandle, AutoresearchEvent,
    AutoresearchIteration, AutoresearchSubscriber, BaselineResult, BudgetKind, CompletionReason,
    ExperimentHypothesis, ExperimentWorkspace, FinalPatch, HypothesisOutcome, IterationStatus,
};
#[cfg(feature = "skills")]
pub use background_review::{
    BackgroundReviewConfig, BackgroundReviewer, ReviewResult, ReviewSignal,
};
pub use compaction::{
    apply_compaction, compact_messages, compact_messages_semantically, CompactionConfig,
    CompactionMarker, CompactionResult,
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
#[cfg(feature = "extract")]
pub use extract::{
    extract_knowledge_loose, extract_proactive_loose, parse_knowledge, parse_proactive,
    ExtractedKnowledge, ProactiveItem,
};
#[cfg(feature = "graph-memory")]
pub use graph_memory::{
    ConversationExtractor, EdgeRelation, ExtractionResult, GraphMemory, GraphMemoryError,
    MemoryEdge as GraphMemoryEdge, MemoryNode as GraphMemoryNode, NodeType as GraphNodeType,
    SemanticRecall,
};
pub use guardrails::{
    classify_tool, GuardrailConfig, GuardrailDecision, SelfHealingRetry, ToolClass, ToolGuardrails,
};
pub use hooks::{HookDecision, HookEvent, HookFn, HookRegistry};
pub use mode::{Profile, Scope};
#[cfg(feature = "routing")]
pub use model_router::{
    ModelRouter, ModelRouterError, ModelTier, ProactiveMonitor, RouterConfig, SkillSuggestion,
    SubagentModelSelector, TaskTier, TaskType,
};
pub use models::{CompatConfig, ModelInfo, ModelRegistry};
#[cfg(all(feature = "ipc", feature = "multiagent"))]
pub use multiagent::CoordinatorEvent;
#[cfg(feature = "multiagent")]
pub use multiagent::{
    AgentProfile, AgentRole, MultiAgentCoordinator, MultiAgentError, TeamResult, TeamTask,
};
pub use permissions::{
    authorize, authorize_with_workspace, command_from_args, is_dangerous_shell_command,
    is_process_tool, is_read_only_tool, is_write_tool, path_outside_workspace, shell_argv,
    shell_ast, shell_command_allowed, shell_command_matches_all, shell_command_matches_any,
    shell_rule_matches, shell_segments, shell_simples, AlwaysAllow, AlwaysApprovePlan, AlwaysDeny,
    ApprovalRequest, Approver, AsyncApprover, Authorizer, ChannelApprover, ChannelAsyncApprover,
    ChannelPlanApprover, Decision, PermissionMode, PlanApprover, PlanDecision, PlanProposal,
    Policy, PolicyAuthorizer, ShellNode, ShellSimple,
};
pub use prompt_cache::{
    apply_cache_control, CachePoint, CachePosition, CacheProvider, CacheStats, CacheStatsTracker,
    CacheTtl, PromptCacheConfig,
};
pub use provider::{Message, Provider, ProviderRegistry, Role, StreamEvent};
#[cfg(feature = "extract")]
pub use ranking::{rank, rank_with_query, top_n};
pub use repomap::{RepoMap, RepoMapError};
pub use rollout::{RolloutEntry, RolloutManager, TraceWriter};
#[cfg(feature = "routing")]
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
#[cfg(feature = "sse")]
pub use sse::{SseError, SseEvent, SseParser};
pub use subagent::{
    SubagentBudget, SubagentConfig, SubagentError, SubagentEvent, SubagentHandle, SubagentLimits,
    SubagentManager, SubagentResult, SubagentStatus, SubagentSubscriber,
};
pub use todo::{TodoConfig, TodoItem, TodoState, TodoStatus};
#[cfg(feature = "autoresearch")]
pub use tools::register_autoresearch_tools;
pub use tools::{register_builtin_tools, register_spawn_agent_tool};
#[cfg(feature = "work-pack")]
pub use work_pack::{WorkPack, WorkPackError};

#[cfg(feature = "personality")]
pub use personality::{
    CalibrationRecord, ConversationEvent, ConversationHealth, MindHypothesis, ObservationFinding,
    ObservationSeverity, PersonaBlueprint, PersonaValidation, Personality, RiskAssessment,
    RiskRecommendation, RouterResult, RouterRules, SignalSummary, SocialSignal, TurnAction,
    TurnDecision, VoiceCard,
};

#[cfg(feature = "mcp")]
pub use mcp::{
    McpClient, McpError, McpRegistry, McpResourceInfo, McpServerConfig, McpToolInfo,
    McpTransportKind,
};

#[cfg(feature = "ipc")]
pub use acp::{AcpHost, AcpSession};

#[cfg(feature = "marketplace")]
pub use marketplace::{
    verify_plugin_integrity, InstalledPlugin, MarketplaceError, MarketplaceIndex, PluginBlocklist,
    PluginInstaller, PluginManifest,
};

#[cfg(feature = "providers")]
pub use http::{global_client, is_local_provider, HttpClient, TimeoutConfig};

#[cfg(feature = "ipc")]
pub use lsp::{Diagnostic, DiagnosticSeverity, Location, LspManager, LspServer};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty(), "VERSION should not be empty");
    }
}
