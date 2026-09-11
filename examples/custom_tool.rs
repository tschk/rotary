//! Register a host tool and invoke it through the same registry the loop uses.
//!
//! ```bash
//! cargo run --example custom_tool
//! ```

use std::sync::Arc;

use rx4::{
    register_builtin_tools, ToolContext, ToolDefinition, ToolEffect, ToolFuture, ToolRegistry,
    ToolResult,
};

/// Every tool is `fn(Arc<ToolContext>, String) -> ToolFuture`.
fn echo(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let parsed: serde_json::Value = serde_json::from_str(&args).unwrap_or_default();
        let text = parsed
            .get("text")
            .and_then(|value| value.as_str())
            .unwrap_or("(missing text)");
        ToolResult::ok(
            "echo",
            format!("{} -> {text}", ctx.workspace_root.display()),
        )
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tools = ToolRegistry::new();
    register_builtin_tools(&tools);
    tools.register(
        ToolDefinition::new_fn(
            "echo",
            "Echo text back, prefixed with the workspace root.",
            r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#,
            echo,
        )
        .with_effect(ToolEffect::Read),
    );

    let ctx = Arc::new(ToolContext::new(std::env::current_dir()?));
    let result = tools
        .execute("echo", &ctx, r#"{"text":"hello from a host tool"}"#)
        .await
        .expect("echo is registered");

    println!("is_error: {}", result.is_error);
    println!("content: {}", result.content);

    Ok(())
}
