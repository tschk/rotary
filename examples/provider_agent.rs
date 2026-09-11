//! Wire a provider, stream events, and drive one turn.
//!
//! Requires the `providers` feature and an API key:
//!
//! ```bash
//! OPENAI_API_KEY=sk-... cargo run --example provider_agent --features providers
//! ```

use std::sync::Arc;

use rx4::provider::OpenAIProvider;
use rx4::{register_builtin_tools, Agent, Event, Scope, ToolRegistry};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if key.is_empty() {
        eprintln!("set OPENAI_API_KEY to run a live turn");
        return Ok(());
    }

    let mut agent = Agent::new();
    let tools = ToolRegistry::new();
    register_builtin_tools(&tools);
    agent.set_tools(tools);
    agent.set_scope(Scope::Coding);
    agent.set_provider(Arc::new(OpenAIProvider::new(key)));

    agent.subscribe(|event| match event {
        Event::MessageDelta { delta } => print!("{delta}"),
        Event::ToolCall(call) => println!("\n[tool] {}", call.name),
        Event::ToolExecutionEnd(result) => {
            println!("[tool done] error={}", result.is_error);
        }
        Event::TurnEnded { turn, .. } => println!("\n[turn {turn} done]"),
        Event::Error(message) => eprintln!("\n[error] {message}"),
        _ => {}
    });

    agent
        .prompt("List the files in the current directory, then summarize what this project is.")
        .await?;

    println!("\ntotal cost (USD): {:.6}", agent.total_cost());
    Ok(())
}
