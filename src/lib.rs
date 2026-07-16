//! rx4 — the agent harness engine with full pi protocol compatibility.
//!
//! Models write. rx4 gives them tools, memory, loops, permissions, sessions,
//! and control planes. Hosts (CLIs, TUIs, IDEs) embed rx4.
//!
//! # Safety
//!
//! This crate is `#![forbid(unsafe_code)]` — no unsafe code is allowed anywhere.

#![forbid(unsafe_code)]
//!
//! Pi-compatible: JSONL v3 sessions, RPC over stdin/stdout, pi tool names,
//! extension protocol with capability policy, SDK surface.
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
pub mod compaction;
pub mod config;
pub mod context;
pub mod extract;
pub mod guardrails;
pub mod hooks;
pub mod mode;
pub mod permissions;
pub mod plugin;
pub mod provider;
pub mod ranking;
pub mod rollout;
pub mod sandbox;
pub mod secrets;
pub mod session;
pub mod slash;
pub mod sse;
pub mod tools;

#[cfg(feature = "computer-use")]
pub mod computer_use;

#[cfg(feature = "ipc")]
pub mod ipc;

#[cfg(feature = "memory")]
pub mod memory;

pub mod models;

#[cfg(feature = "mcp")]
pub mod mcp;

#[cfg(feature = "pi-compat")]
pub mod pi;

pub mod acp;
pub mod lsp;

pub use agent::{
    Agent, Event, ToolCall, ToolContext, ToolDefinition, ToolEffect, ToolRegistry, ToolResult,
};
pub use compaction::{compact_messages, CompactionConfig, CompactionMarker, CompactionResult};
pub use hooks::HookRegistry;
pub use mode::{Profile, Scope};
pub use models::{CompatConfig, ModelInfo, ModelRegistry};
pub use permissions::{Approver, Decision, PermissionMode, Policy};
pub use provider::{Message, Provider, ProviderRegistry, Role, StreamEvent};
pub use rollout::{RolloutEntry, RolloutManager, TraceWriter};
pub use sandbox::{SandboxConfig, SandboxError, SandboxManager, SandboxProfile, SandboxViolation};
pub use secrets::{
    filter_env_vars, is_sensitive_env_var, RedactionConfig, Redactor, SecretMatch, SecretPattern,
};
pub use session::Session;
pub use sse::{SseError, SseEvent, SseParser};
pub use tools::register_builtin_tools;

pub const VERSION: &str = "0.3.0";

pub fn print_banner() {
    eprintln!("rx4 {VERSION} — agent harness engine (pi-compatible)");
}
