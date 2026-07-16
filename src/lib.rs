//! rx4 — the agent harness engine with full pi protocol compatibility.
//!
//! Models write. rx4 gives them tools, memory, loops, permissions, sessions,
//! and control planes. Hosts (CLIs, TUIs, IDEs) embed rx4.
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
pub mod session;
pub mod slash;
pub mod tools;

#[cfg(feature = "computer-use")]
pub mod computer_use;

#[cfg(feature = "ipc")]
pub mod ipc;

#[cfg(feature = "memory")]
pub mod memory;

#[cfg(feature = "mcp")]
pub mod mcp;

#[cfg(feature = "pi-compat")]
pub mod pi;

pub mod acp;
pub mod lsp;

pub use agent::{Agent, Event, ToolCall, ToolContext, ToolDefinition, ToolRegistry, ToolResult};
pub use hooks::HookRegistry;
pub use mode::{Profile, Scope};
pub use permissions::{Approver, Decision, PermissionMode, Policy};
pub use provider::{Message, Provider, ProviderRegistry, Role, StreamEvent};
pub use session::Session;
pub use tools::register_builtin_tools;

pub const VERSION: &str = "0.3.0";

pub fn print_banner() {
    eprintln!("rx4 {VERSION} — agent harness engine (pi-compatible)");
}
