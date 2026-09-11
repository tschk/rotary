//! Smallest useful embed: build an agent, mount builtin tools, pick a scope.
//!
//! ```bash
//! cargo run --example minimal_agent
//! ```

use rx4::{register_builtin_tools, Agent, Scope, ToolRegistry};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut agent = Agent::new();

    let tools = ToolRegistry::new();
    register_builtin_tools(&tools);
    println!("tools: {}", tools.names().join(", "));
    println!("loadout fingerprint: {}", tools.definitions_fingerprint());

    agent.set_tools(tools);
    agent.set_scope(Scope::Coding);
    agent.set_policy(rx4::Policy::workspace_write());
    agent.load_project_context();

    println!("scope: {}", Scope::Coding.name());
    println!("workspace: {}", agent.workspace_root.display());
    println!("context window: {} tokens", agent.context_window());

    Ok(())
}
