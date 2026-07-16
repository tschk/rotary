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
pub mod background_review;
pub mod compaction;
pub mod config;
pub mod context;
pub mod cost;
pub mod dream_scheduler;
pub mod embeddings;
pub mod extract;
pub mod graph_memory;
pub mod guardrails;
pub mod hooks;
pub mod mode;
pub mod model_router;
pub mod multiagent;
pub mod permissions;
pub mod plugin;
pub mod prompt_cache;
pub mod provider;
pub mod ranking;
pub mod repomap;
pub mod rollout;
pub mod routing;
pub mod sandbox;
pub mod secrets;
pub mod session;
pub mod skill_engine;
pub mod skill_curator;
pub mod slash;
pub mod sse;
pub mod subagent;
pub mod tools;

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

pub mod acp;
#[cfg(feature = "ipc")]
pub mod lsp;
pub mod marketplace;

pub use agent::{
    Agent, Event, ToolCall, ToolContext, ToolDefinition, ToolEffect, ToolExecuteBox,
    ToolExecuteFn, ToolExecutor, ToolFuture, ToolRegistry, ToolResult, normalize_tool_name,
};
pub use background_review::{
    BackgroundReviewConfig, BackgroundReviewer, ReviewResult, ReviewSignal,
};
pub use compaction::{compact_messages, CompactionConfig, CompactionMarker, CompactionResult};
pub use cost::{CostEntry, ModelPricing, PricingRegistry, SessionCost, TokenUsage};
pub use dream_scheduler::{DreamReport, DreamScheduler};
pub use embeddings::{
    cosine_similarity, EmbedError, EmbeddingClient, EmbeddingConfig, EmbeddingProvider,
    SemanticSearch,
};
pub use graph_memory::{
    ConversationExtractor, EdgeRelation, ExtractionResult, GraphMemory, GraphMemoryError,
    MemoryEdge as GraphMemoryEdge, MemoryNode as GraphMemoryNode, NodeType as GraphNodeType,
};
pub use guardrails::{
    classify_tool, GuardrailConfig, GuardrailDecision, SelfHealingRetry, ToolClass, ToolGuardrails,
};
pub use hooks::HookRegistry;
pub use mode::{Profile, Scope};
pub use model_router::{
    ModelRouter, ModelRouterError, ModelTier, ProactiveMonitor, RouterConfig, SkillSuggestion,
    SubagentModelSelector, TaskTier, TaskType,
};
pub use models::{CompatConfig, ModelInfo, ModelRegistry};
pub use permissions::{Approver, Decision, PermissionMode, Policy};
pub use prompt_cache::{
    apply_cache_control, CachePoint, CachePosition, CacheProvider, CacheStats, CacheStatsTracker,
    CacheTtl, PromptCacheConfig,
};
pub use provider::{Message, Provider, ProviderRegistry, Role, StreamEvent};
pub use repomap::{RepoMap, RepoMapError};
pub use rollout::{RolloutEntry, RolloutManager, TraceWriter};
pub use routing::{
    AgentRoute, AgentRouter, RoutingConfig, RoutingStats, SmartRouter, TurnComplexity,
};
pub use sandbox::{SandboxConfig, SandboxError, SandboxManager, SandboxProfile, SandboxViolation};
pub use secrets::{
    filter_env_vars, is_sensitive_env_var, RedactionConfig, Redactor, SecretMatch, SecretPattern,
};
pub use session::Session;
pub use skill_engine::{Skill, SkillEngine, SkillError, SkillFrontmatter, SkillOutcome, SkillState};
pub use skill_curator::{CuratorConfig, CuratorSuggestion, SkillCurator, SuggestionKind};
pub use sse::{SseError, SseEvent, SseParser};
pub use tools::register_builtin_tools;

#[cfg(feature = "mcp")]
pub use mcp::{McpClient, McpError, McpRegistry, McpResourceInfo, McpToolInfo};

pub use marketplace::{
    InstalledPlugin, MarketplaceError, MarketplaceIndex, McpServerConfig, PluginBlocklist,
    PluginInstaller, PluginManifest,
};

#[cfg(feature = "providers")]
pub use http::{global_client, is_local_provider, HttpClient, TimeoutConfig};

#[cfg(feature = "ipc")]
pub use lsp::{Diagnostic, DiagnosticSeverity, Location, LspManager, LspServer};

pub const VERSION: &str = "0.3.0";

pub fn print_banner() {
    eprintln!("rx4 {VERSION} — agent harness engine");
}
