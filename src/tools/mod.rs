//! Built-in coding tools: read, write, edit, bash, grep, find, ls + extended tools.
//! Uses fff for fast indexed file search.

pub(crate) mod common;
mod extended;
pub(crate) mod fs;

use crate::agent::{ToolDefinition, ToolEffect, ToolRegistry, ToolResult};
use crate::subagent::{SubagentConfig, SubagentManager};
use parking_lot::Mutex;
use std::sync::Arc;

pub fn register_builtin_tools(registry: &mut ToolRegistry) {
    registry.register(
        ToolDefinition::new_fn(
            "read",
            "Read the contents of a file at the given path. Returns content with line numbers.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"}},"required":["path"]}"#,
            fs::exec_read,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "write",
            "Write content to a file, creating or overwriting it.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#,
            fs::exec_write,
        )
        .with_effect(ToolEffect::Write),
    );
    registry.register(
        ToolDefinition::new_fn(
            "edit",
            "Perform a string replacement in a file. old_string must be unique.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}},"required":["path","old_string","new_string"]}"#,
            fs::exec_edit,
        )
        .with_effect(ToolEffect::Write),
    );
    registry.register(
        ToolDefinition::new_fn(
            "bash",
            "Execute a shell command and return stdout/stderr. Optional timeout in seconds.",
            r#"{"type":"object","properties":{"command":{"type":"string"},"cwd":{"type":"string"},"timeout":{"type":"integer"}},"required":["command"]}"#,
            fs::exec_bash,
        )
        .with_effect(ToolEffect::Process),
    );
    registry.register(
        ToolDefinition::new_fn(
            "grep",
            "Search file contents using regex. Returns matching lines with context.",
            r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"context":{"type":"integer"}},"required":["pattern"]}"#,
            fs::exec_grep,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "find",
            "Find files by fuzzy/glob pattern. Uses fff indexed search.",
            r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}"#,
            fs::exec_find,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "ls",
            "List entries in a directory.",
            r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
            fs::exec_ls,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "web_fetch",
            "HTTP GET a URL and return response text (truncated). Requires providers feature.",
            r#"{"type":"object","properties":{"url":{"type":"string"},"max_bytes":{"type":"integer"}},"required":["url"]}"#,
            extended::exec_web_fetch,
        )
        .with_effect(ToolEffect::Network),
    );
    registry.register(
        ToolDefinition::new_fn(
            "todo",
            "Manage an in-memory session todo list. Actions: list, add, update, complete, clear.",
            r#"{"type":"object","properties":{"action":{"type":"string","enum":["list","add","update","complete","clear"]},"items":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"content":{"type":"string"},"status":{"type":"string"}},"required":["content"]}}},"required":["action"]}"#,
            extended::exec_todo,
        )
        .with_effect(ToolEffect::Write),
    );
    registry.register(
        ToolDefinition::new_fn(
            "spawn_agent",
            "Spawn a nested subagent with a prompt. Uses ToolContext provider/tools when present.",
            r#"{"type":"object","properties":{"prompt":{"type":"string"},"name":{"type":"string"},"model":{"type":"string"},"isolate":{"type":"boolean"}},"required":["prompt"]}"#,
            extended::exec_spawn_agent,
        )
        .with_effect(ToolEffect::Process),
    );
    registry.register(
        ToolDefinition::new_fn(
            "enter_plan_mode",
            "Request Plan scope and return plan-mode instructions for the model.",
            r#"{"type":"object","properties":{}}"#,
            extended::exec_enter_plan_mode,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "exit_plan_mode",
            "Request Coding scope and leave plan mode.",
            r#"{"type":"object","properties":{}}"#,
            extended::exec_exit_plan_mode,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "lsp_diagnostics",
            "Get LSP diagnostics for a document URI and language.",
            r#"{"type":"object","properties":{"uri":{"type":"string"},"language":{"type":"string"}},"required":["uri","language"]}"#,
            extended::exec_lsp_diagnostics,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "lsp_definition",
            "Resolve definition locations via LSP.",
            r#"{"type":"object","properties":{"uri":{"type":"string"},"language":{"type":"string"},"line":{"type":"integer"},"character":{"type":"integer"}},"required":["uri","language","line","character"]}"#,
            extended::exec_lsp_definition,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "lsp_references",
            "Find references via LSP.",
            r#"{"type":"object","properties":{"uri":{"type":"string"},"language":{"type":"string"},"line":{"type":"integer"},"character":{"type":"integer"}},"required":["uri","language","line","character"]}"#,
            extended::exec_lsp_references,
        )
        .with_effect(ToolEffect::Read),
    );
}

/// Register spawn_agent backed by a host-owned SubagentManager.
pub fn register_spawn_agent_tool(
    registry: &mut ToolRegistry,
    manager: Arc<Mutex<SubagentManager>>,
) {
    registry.register(
        ToolDefinition::new_boxed(
            "spawn_agent",
            "Spawn a nested subagent with a prompt via host SubagentManager.",
            r#"{"type":"object","properties":{"prompt":{"type":"string"},"name":{"type":"string"},"model":{"type":"string"},"isolate":{"type":"boolean"}},"required":["prompt"]}"#,
            Box::new(move |ctx, args| {
                let manager = Arc::clone(&manager);
                Box::pin(async move {
                    let v: serde_json::Value = match serde_json::from_str(&args) {
                        Ok(v) => v,
                        Err(e) => {
                            return ToolResult::err("spawn_agent", format!("invalid json: {e}"))
                        }
                    };
                    let prompt = match v.get("prompt").and_then(|p| p.as_str()) {
                        Some(p) if !p.is_empty() => p.to_string(),
                        _ => return ToolResult::err("spawn_agent", "prompt required"),
                    };
                    let name = v
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("subagent")
                        .to_string();
                    let model = v
                        .get("model")
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string());
                    let isolate = v
                        .get("isolate")
                        .and_then(|i| i.as_bool())
                        .unwrap_or(false);
                    let config = SubagentConfig {
                        name: name.clone(),
                        model,
                        workspace_isolation: isolate,
                        ..SubagentConfig::default()
                    };
                    let spawn_res = {
                        let mut mgr = manager.lock();
                        mgr.spawn(config, &prompt, &ctx.workspace_root)
                    };
                    match spawn_res {
                        Ok(handle) => {
                            let id = handle.id().to_string();
                            let name = handle.name().to_string();
                            let result = tokio::task::spawn_blocking(move || handle.wait_sync())
                                .await
                                .unwrap_or_else(|e| crate::subagent::SubagentResult {
                                    output: format!("join error: {e}"),
                                    files_modified: vec![],
                                    tool_calls: 0,
                                    error: Some(e.to_string()),
                                    cost: 0.0,
                                    duration_seconds: 0,
                                });
                            let body = serde_json::json!({
                                "id": id,
                                "name": name,
                                "status": if result.error.is_some() { "Failed" } else { "Completed" },
                                "output": result.output,
                                "tool_calls": result.tool_calls,
                                "error": result.error,
                            });
                            ToolResult::ok("spawn_agent", body.to_string())
                        }
                        Err(e) => ToolResult::err("spawn_agent", e.to_string()),
                    }
                })
            }),
        )
        .with_effect(ToolEffect::Process),
    );
}

#[cfg(test)]
mod tests {
    use super::extended;
    use super::fs;
    use crate::agent::ToolContext;
    use crate::mode::Scope;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_and_read() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));

        let write_result = fs::exec_write(
            ctx.clone(),
            r#"{"path":"test.txt","content":"hello world"}"#.to_string(),
        )
        .await;
        assert!(!write_result.is_error);

        let read_result = fs::exec_read(ctx, r#"{"path":"test.txt"}"#.to_string()).await;
        assert!(!read_result.is_error);
        assert!(read_result.content.contains("hello world"));
    }

    #[tokio::test]
    async fn test_edit() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));

        fs::exec_write(
            ctx.clone(),
            r#"{"path":"edit.txt","content":"foo bar baz"}"#.to_string(),
        )
        .await;
        let edit_result = fs::exec_edit(
            ctx,
            r#"{"path":"edit.txt","old_string":"bar","new_string":"qux"}"#.to_string(),
        )
        .await;
        assert!(!edit_result.is_error);

        let content = std::fs::read_to_string(tmp.path().join("edit.txt")).unwrap();
        assert_eq!(content, "foo qux baz");
    }

    #[tokio::test]
    async fn test_bash() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));
        let result = fs::exec_bash(ctx, r#"{"command":"echo hello"}"#.to_string()).await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_bash_large_stdout() {
        let ctx = Arc::new(ToolContext::new(std::env::temp_dir()));
        let args = r#"{"command":"python3 -c 'print(\"x\"*200000)'","timeout":30}"#;
        let result = fs::exec_bash(ctx, args.to_string()).await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.len() > 100_000, "{}", result.content.len());
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));
        let result = fs::exec_bash(ctx, r#"{"command":"sleep 2","timeout":1}"#.to_string()).await;
        assert!(result.is_error);
        assert!(result.content.contains("timed out"));
    }

    #[cfg(feature = "ipc")]
    #[tokio::test]
    async fn test_bash_cancellation() {
        let source = cancellation_token::CancellationTokenSource::new();
        let mut ctx = ToolContext::new(".");
        ctx.cancellation = source.token();
        source.cancel();
        let result = fs::exec_bash(Arc::new(ctx), r#"{"command":"sleep 5"}"#.to_string()).await;
        assert!(result.is_error);
        assert_eq!(result.content, "command cancelled");
    }

    #[tokio::test]
    async fn test_todo_add_list_complete() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));
        let add = extended::exec_todo(
            ctx.clone(),
            r#"{"action":"add","items":[{"id":"1","content":"ship tools"}]}"#.to_string(),
        )
        .await;
        assert!(!add.is_error, "{}", add.content);
        assert!(add.content.contains("ship tools"));

        let listed = extended::exec_todo(ctx.clone(), r#"{"action":"list"}"#.to_string()).await;
        assert!(!listed.is_error);
        assert!(listed.content.contains("ship tools"));

        let done = extended::exec_todo(
            ctx.clone(),
            r#"{"action":"complete","items":[{"id":"1"}]}"#.to_string(),
        )
        .await;
        assert!(!done.is_error);
        assert!(done.content.contains("completed"));

        let _ = extended::exec_todo(ctx, r#"{"action":"clear"}"#.to_string()).await;
    }

    #[tokio::test]
    async fn test_enter_plan_mode_sets_pending_scope() {
        let pending = Arc::new(parking_lot::Mutex::new(None));
        let mut ctx = ToolContext::new(".");
        ctx.pending_scope = Some(Arc::clone(&pending));
        let result = extended::exec_enter_plan_mode(Arc::new(ctx), "{}".to_string()).await;
        assert!(!result.is_error);
        assert_eq!(*pending.lock(), Some(Scope::Plan));
    }

    #[tokio::test]
    async fn test_web_fetch_without_providers_or_offline() {
        let ctx = Arc::new(ToolContext::new("."));
        let result =
            extended::exec_web_fetch(ctx, r#"{"url":"https://example.invalid/"}"#.to_string())
                .await;
        assert!(result.is_error);
        assert!(
            result.content.contains("providers feature required")
                || result.content.contains("request failed")
                || result.content.contains("error")
        );
    }
}
