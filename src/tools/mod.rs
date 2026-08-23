//! Built-in coding tools: read, write, edit, bash, grep, find, ls + extended tools.
//! Uses fff for fast indexed file search.

pub(crate) mod common;
mod extended;
pub(crate) mod fs;

use crate::agent::{ToolContext, ToolDefinition, ToolEffect, ToolRegistry, ToolResult};
use crate::subagent::{SubagentConfig, SubagentManager};
use parking_lot::Mutex;
use std::sync::Arc;

fn parse_spawn_args(args: &str) -> Result<(SubagentConfig, String), String> {
    let v: serde_json::Value =
        serde_json::from_str(args).map_err(|e| format!("invalid json: {e}"))?;

    let prompt = match v.get("prompt").and_then(|p| p.as_str()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => return Err("prompt required".to_string()),
    };
    let name = v
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("subagent")
        .to_string();
    let model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);
    let isolate = v.get("isolate").and_then(|i| i.as_bool()).unwrap_or(true);
    let config = SubagentConfig {
        name,
        model,
        workspace_isolation: isolate,
        ..SubagentConfig::default()
    };

    Ok((config, prompt))
}

fn spawn_subagent(
    manager: &Arc<Mutex<SubagentManager>>,
    config: SubagentConfig,
    prompt: &str,
    workspace_root: &std::path::Path,
) -> ToolResult {
    let spawn_res = manager
        .lock()
        .spawn_background(config, prompt, workspace_root);
    match spawn_res {
        Ok(handle) => ToolResult::ok(
            "spawn_agent",
            serde_json::json!({
                "id": handle.id(),
                "name": handle.name(),
                "status": "running",
            })
            .to_string(),
        ),
        Err(e) => ToolResult::err("spawn_agent", e.to_string()),
    }
}

async fn execute_spawn_agent(
    manager: Arc<Mutex<SubagentManager>>,
    ctx: Arc<ToolContext>,
    args: String,
) -> ToolResult {
    let (config, prompt) = match parse_spawn_args(&args) {
        Ok(res) => res,
        Err(e) => return ToolResult::err("spawn_agent", e),
    };

    spawn_subagent(&manager, config, &prompt, &ctx.workspace_root)
}

async fn execute_list_subagents(
    manager: Arc<Mutex<SubagentManager>>,
    _ctx: Arc<ToolContext>,
    _args: String,
) -> ToolResult {
    let manager = manager.lock();
    let subagents = manager
        .list()
        .into_iter()
        .map(|handle| {
            serde_json::json!({
                "id": handle.id(),
                "name": handle.name(),
                "status": handle.status(),
                "depth": handle.depth(),
                "children": handle.children().len(),
                "descendants": handle.descendant_count(),
            })
        })
        .collect::<Vec<_>>();
    ToolResult::ok(
        "list_subagents",
        serde_json::to_string(&subagents).unwrap_or_else(|_| "[]".to_string()),
    )
}

async fn execute_cancel_subagent(
    manager: Arc<Mutex<SubagentManager>>,
    _ctx: Arc<ToolContext>,
    args: String,
) -> ToolResult {
    let v: serde_json::Value = match serde_json::from_str(&args) {
        Ok(v) => v,
        Err(e) => return ToolResult::err("cancel_subagent", format!("invalid json: {e}")),
    };
    let id = match v.get("id").and_then(|id| id.as_str()) {
        Some(id) if !id.is_empty() => id,
        _ => return ToolResult::err("cancel_subagent", "id required"),
    };
    match manager.lock().cancel(id) {
        Ok(()) => ToolResult::ok("cancel_subagent", format!("cancelled {id}")),
        Err(e) => ToolResult::err("cancel_subagent", e.to_string()),
    }
}

pub fn register_builtin_tools(registry: &mut ToolRegistry) {
    let tools: &[(&str, &str, &str, crate::agent::ToolExecuteFn, ToolEffect)] = &[
        (
            "read",
            "Read the contents of a file at the given path. Returns content with line numbers.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"},"hashline":{"type":"boolean"}},"required":["path"]}"#,
            fs::exec_read,
            ToolEffect::Read,
        ),
        (
            "write",
            "Write content to a file, creating or overwriting it.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#,
            fs::exec_write,
            ToolEffect::Write,
        ),
        (
            "edit",
            "Perform a string replacement in a file. old_string must be unique.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}},"required":["path","old_string","new_string"]}"#,
            fs::exec_edit,
            ToolEffect::Write,
        ),
        (
            "hashline_edit",
            "Apply a hashline PUT/CUT/MV/REM script. Fails closed on stale tag, unseen/elided lines, and no-ops.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"tag":{"type":"string"},"script":{"type":"string"},"family":{"type":"string"}},"required":["path","tag","script"]}"#,
            fs::exec_hashline_edit,
            ToolEffect::Write,
        ),
        (
            "bash",
            "Execute a shell command and return stdout/stderr. Optional timeout in seconds. SECURITY WARNING: Allows arbitrary command execution; do not use with unsanitized external input.",
            r#"{"type":"object","properties":{"command":{"type":"string"},"cwd":{"type":"string"},"timeout":{"type":"integer"}},"required":["command"]}"#,
            fs::exec_bash,
            ToolEffect::Process,
        ),
        (
            "grep",
            "Search file contents using regex. Returns matching lines with context.",
            r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"context":{"type":"integer"}},"required":["pattern"]}"#,
            fs::exec_grep,
            ToolEffect::Read,
        ),
        (
            "find",
            "Find files by fuzzy/glob pattern. Uses fff indexed search.",
            r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}"#,
            fs::exec_find,
            ToolEffect::Read,
        ),
        (
            "ls",
            "List entries in a directory.",
            r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
            fs::exec_ls,
            ToolEffect::Read,
        ),
        (
            "web_fetch",
            "HTTP GET a URL and return response text (truncated). Requires providers feature.",
            r#"{"type":"object","properties":{"url":{"type":"string"},"max_bytes":{"type":"integer"}},"required":["url"]}"#,
            extended::exec_web_fetch,
            ToolEffect::Network,
        ),
        (
            "todo",
            "Manage a todo list. When the engine todo feature is enabled, creation requires confidence (0-100), and completion requires completion_confidence (0-100). Actions: list, create/add, update, complete, clear.",
            r#"{"type":"object","properties":{"action":{"type":"string","enum":["list","create","add","update","complete","clear"]},"items":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"content":{"type":"string"},"status":{"type":"string"},"confidence":{"type":"integer","minimum":0,"maximum":100},"completion_confidence":{"type":"integer","minimum":0,"maximum":100}}}}},"required":["action"]}"#,
            extended::exec_todo,
            ToolEffect::Write,
        ),
        (
            "spawn_agent",
            "Spawn a nested subagent with a prompt. Uses ToolContext provider/tools when present.",
            r#"{"type":"object","properties":{"prompt":{"type":"string"},"name":{"type":"string"},"model":{"type":"string"},"isolate":{"type":"boolean"}},"required":["prompt"]}"#,
            extended::exec_spawn_agent,
            ToolEffect::Process,
        ),
        (
            "enter_plan_mode",
            "Request Plan scope and return plan-mode instructions for the model.",
            r#"{"type":"object","properties":{}}"#,
            extended::exec_enter_plan_mode,
            ToolEffect::Read,
        ),
        (
            "exit_plan_mode",
            "Request Coding scope and leave plan mode.",
            r#"{"type":"object","properties":{}}"#,
            extended::exec_exit_plan_mode,
            ToolEffect::Read,
        ),
        (
            "lsp_diagnostics",
            "Get LSP diagnostics for a document URI and language.",
            r#"{"type":"object","properties":{"uri":{"type":"string"},"language":{"type":"string"}},"required":["uri","language"]}"#,
            extended::exec_lsp_diagnostics,
            ToolEffect::Read,
        ),
        (
            "lsp_definition",
            "Resolve definition locations via LSP.",
            r#"{"type":"object","properties":{"uri":{"type":"string"},"language":{"type":"string"},"line":{"type":"integer"},"character":{"type":"integer"}},"required":["uri","language","line","character"]}"#,
            extended::exec_lsp_definition,
            ToolEffect::Read,
        ),
        (
            "lsp_references",
            "Find references via LSP.",
            r#"{"type":"object","properties":{"uri":{"type":"string"},"language":{"type":"string"},"line":{"type":"integer"},"character":{"type":"integer"}},"required":["uri","language","line","character"]}"#,
            extended::exec_lsp_references,
            ToolEffect::Read,
        ),
    ];

    for (name, desc, params, exec, effect) in tools {
        registry
            .register(ToolDefinition::new_fn(*name, *desc, *params, *exec).with_effect(*effect));
    }
}

/// Register the opt-in autoresearch tools. They are kept separate from the
/// default coding loadout so ordinary prompts retain a small, stable prefix.
#[cfg(feature = "autoresearch")]
pub fn register_autoresearch_tools(
    registry: &mut ToolRegistry,
    handle: crate::autoresearch::AutoresearchHandle,
) {
    crate::autoresearch::register_tools(registry, handle);
}

/// Register spawn_agent backed by a host-owned SubagentManager.
pub fn register_spawn_agent_tool(
    registry: &mut ToolRegistry,
    manager: Arc<Mutex<SubagentManager>>,
) {
    let spawn_manager = Arc::clone(&manager);
    registry.register(
        ToolDefinition::new_boxed(
            "spawn_agent",
            "Spawn a nested subagent with a prompt via host SubagentManager.",
            r#"{"type":"object","properties":{"prompt":{"type":"string"},"name":{"type":"string"},"model":{"type":"string"},"isolate":{"type":"boolean"}},"required":["prompt"]}"#,
            Box::new(move |ctx, args| {
                let manager = Arc::clone(&spawn_manager);
                Box::pin(execute_spawn_agent(manager, ctx, args))
            }),
        )
        .with_effect(ToolEffect::Process),
    );
    let list_manager = Arc::clone(&manager);
    registry.register(
        ToolDefinition::new_boxed(
            "list_subagents",
            "List subagents spawned by this session and their current status.",
            r#"{"type":"object","properties":{}}"#,
            Box::new(move |ctx, args| {
                let manager = Arc::clone(&list_manager);
                Box::pin(execute_list_subagents(manager, ctx, args))
            }),
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_boxed(
            "cancel_subagent",
            "Cancel a subagent by id.",
            r#"{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}"#,
            Box::new(move |ctx, args| {
                let manager = Arc::clone(&manager);
                Box::pin(execute_cancel_subagent(manager, ctx, args))
            }),
        )
        .with_effect(ToolEffect::Process),
    );
}

#[cfg(test)]
mod tests {
    use super::extended;
    use super::fs;
    use super::register_spawn_agent_tool;
    use crate::agent::ToolContext;
    use crate::mode::Scope;
    use crate::subagent::SubagentManager;
    use crate::ToolRegistry;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn test_register_builtin_tools() {
        let mut registry = ToolRegistry::new();
        super::register_builtin_tools(&mut registry);

        let expected_tools = vec![
            "read",
            "write",
            "edit",
            "hashline_edit",
            "bash",
            "grep",
            "find",
            "ls",
            "web_fetch",
            "todo",
            "spawn_agent",
            "enter_plan_mode",
            "exit_plan_mode",
            "lsp_diagnostics",
            "lsp_definition",
            "lsp_references",
        ];

        assert_eq!(registry.count(), expected_tools.len());

        let defs = registry.definitions();
        for tool in expected_tools {
            assert!(
                defs.iter()
                    .any(|d| d.get("name").unwrap().as_str().unwrap() == tool),
                "Missing tool: {}",
                tool
            );
        }
    }

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

    #[cfg(feature = "builtin-tools")]
    #[tokio::test]
    async fn test_grep_and_find_stdlib_or_fff() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));
        fs::exec_write(
            ctx.clone(),
            r#"{"path":"needle.rs","content":"fn find_me() {}\nfn other() {}\n"}"#.to_string(),
        )
        .await;

        let grep = fs::exec_grep(ctx.clone(), r#"{"pattern":"find_me"}"#.to_string()).await;
        assert!(!grep.is_error, "{}", grep.content);
        assert!(grep.content.contains("find_me"), "{}", grep.content);

        let find = fs::exec_find(ctx, r#"{"pattern":"needle.rs"}"#.to_string()).await;
        assert!(!find.is_error, "{}", find.content);
        assert!(find.content.contains("needle.rs"), "{}", find.content);
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

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn test_bash_in_macos_sandbox() {
        let workspace = std::env::current_dir().unwrap();
        let config = crate::sandbox::OsSandboxConfig::new(
            crate::sandbox::OsSandbox::MacosSeatbelt,
            workspace.clone(),
        );
        let runner = crate::sandbox::OsSandboxRunner::new(config).unwrap();
        let ctx = Arc::new(ToolContext::new(&workspace).with_os_sandbox(Arc::new(runner)));
        let result = fs::exec_bash(
            ctx,
            r#"{"command":"git status --short >/dev/null && pwd"}"#.to_string(),
        )
        .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(!result.content.contains("exit code"), "{}", result.content);
        assert!(
            !result.content.contains("Operation not permitted"),
            "{}",
            result.content
        );
        assert!(result.content.contains(workspace.to_str().unwrap()));
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

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_to_outside_workspace_is_rejected() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        symlink("/etc/passwd", tmp.path().join("outside-link")).unwrap();
        let sandbox = crate::sandbox::SandboxManager::new(
            crate::sandbox::SandboxProfile::Workspace,
            tmp.path().to_path_buf(),
        );
        let ctx = Arc::new(ToolContext::new(tmp.path()).with_sandbox(Arc::new(sandbox)));
        let result = fs::exec_read(ctx, r#"{"path":"outside-link"}"#.to_string()).await;
        assert!(
            result.is_error,
            "symlink escape was read: {}",
            result.content
        );
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

    #[tokio::test]
    async fn web_fetch_honors_network_denial() {
        let tmp = TempDir::new().unwrap();
        let mut sandbox = crate::sandbox::SandboxManager::new(
            crate::sandbox::SandboxProfile::Workspace,
            tmp.path().to_path_buf(),
        );
        sandbox.set_allow_network(false);
        let ctx = Arc::new(ToolContext::new(tmp.path()).with_sandbox(Arc::new(sandbox)));
        let result =
            extended::exec_web_fetch(ctx, r#"{"url":"https://example.invalid/"}"#.to_string())
                .await;
        assert!(result.is_error);
        assert!(result.content.contains("network"), "{}", result.content);
    }

    #[tokio::test]
    async fn test_register_spawn_agent_tool_registers_expected_tools() {
        let mut registry = ToolRegistry::new();
        let manager = Arc::new(Mutex::new(SubagentManager::new()));

        register_spawn_agent_tool(&mut registry, manager);

        let definitions = registry.definitions();
        let tool_names: Vec<&str> = definitions
            .iter()
            .filter_map(|d| d.get("name").and_then(|n| n.as_str()))
            .collect();

        assert_eq!(tool_names.len(), 3);
        assert!(tool_names.contains(&"spawn_agent"));
        assert!(tool_names.contains(&"list_subagents"));
        assert!(tool_names.contains(&"cancel_subagent"));
    }

    #[tokio::test]
    async fn host_subagent_tools_share_lifecycle_state() {
        let tmp = TempDir::new().unwrap();
        let manager = Arc::new(Mutex::new(SubagentManager::new()));
        let mut registry = ToolRegistry::new();
        register_spawn_agent_tool(&mut registry, Arc::clone(&manager));
        let ctx = Arc::new(ToolContext::new(tmp.path()));

        let spawned = registry
            .execute(
                "spawn_agent",
                &ctx,
                r#"{"prompt":"inspect","name":"explore","isolate":false}"#,
            )
            .await
            .unwrap();
        assert!(!spawned.is_error, "{}", spawned.content);
        let id = serde_json::from_str::<serde_json::Value>(&spawned.content).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let listed = registry
            .execute("list_subagents", &ctx, "{}")
            .await
            .unwrap();
        assert!(listed.content.contains(&id));
        let cancelled = registry
            .execute(
                "cancel_subagent",
                &ctx,
                &serde_json::json!({"id": id}).to_string(),
            )
            .await
            .unwrap();
        assert!(!cancelled.is_error, "{}", cancelled.content);
    }
}
