//! Assemble a generic coding agent while keeping runtime decisions in the host.
//!
//! ```bash
//! cargo run --example harness_builder
//! ```

use rx4::{HarnessBuilder, Policy, Scope};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut agent = HarnessBuilder::new()
        .scope(Scope::Coding)
        .policy(Policy::workspace_write())
        .builtin_tools([
            "read", "write", "edit", "bash", "grep", "find", "ls", "todo",
        ])
        .instructions("Keep the public API backward compatible.")
        .build()?;

    // The builder returns a normal Agent. The host still owns provider setup,
    // authorization, hooks, persistence, and when to drive `prompt`.
    agent.load_project_context();

    println!("tools: {}", agent.tools.names().join(", "));
    println!("scope: {}", agent.scope);
    println!("todo engine: {}", agent.todo_config.is_some());
    Ok(())
}
