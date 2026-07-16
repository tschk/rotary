//! Pi protocol compatibility layer.
//!
//! Full compatibility with the pi agent protocol:
//! - JSONL v3 session format with entry types
//! - RPC over stdin/stdout (JSON Lines)
//! - Pi tool name aliases (read_file, write_file, list_dir, run_command, etc.)
//! - Extension protocol with capability policy
//! - SDK surface (createAgentSession, AgentSessionHandle)
//!
//! Enable with `pi-compat` feature (on by default).
//! Enable `pi-extensions` for QuickJS runtime execution of pi extensions.

pub mod extension;
pub mod policy;
pub mod rpc;
pub mod sdk;
pub mod session;
pub mod tools;

pub use extension::{PiExtension, PiExtensionManager, PiExtensionManifest, PiExtensionRuntime};
pub use policy::{PiCapability, PiCapabilityPolicy, PiExtensionOverride, PiPolicyMode};
pub use rpc::{PiRpcClient, PiRpcCommand, PiRpcEvent, PiRpcServer};
pub use sdk::{create_agent_session, AgentSessionHandle, AgentSessionOptions, SessionTransport};
pub use session::{PiEntry, PiEntryType, PiSession, PiSessionHeader};
pub use tools::{is_pi_tool_name, pi_to_rx4_tool, pi_tool_names, rx4_to_pi_tool};

/// Pi session format version.
pub const PI_SESSION_VERSION: u32 = 3;

/// Pi data directory.
pub fn pi_data_dir() -> std::path::PathBuf {
    #[cfg(feature = "pi-compat")]
    {
        if let Some(home) = dirs::home_dir() {
            return home.join(".pi").join("agent");
        }
    }
    std::path::PathBuf::from(".pi/agent")
}

/// Pi sessions directory for a project.
pub fn pi_sessions_dir(project_path: &std::path::Path) -> std::path::PathBuf {
    let encoded = encode_project_path(project_path);
    pi_data_dir().join("sessions").join(encoded)
}

/// Encode a project path the way pi does (replaces path separators).
pub fn encode_project_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    s.replace(['/', '\\'], "--")
        .replace(' ', "-")
        .replace(':', "")
}
