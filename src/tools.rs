//! Built-in coding tools: read, write, edit, bash, grep, find, ls.
//! Uses rayon for parallel search (grok pattern).

use crate::agent::{ToolContext, ToolDefinition, ToolEffect, ToolFuture, ToolRegistry, ToolResult};
use crate::mode::Scope;
use crate::subagent::{SubagentConfig, SubagentManager};
use dashmap::DashMap;
use parking_lot::Mutex;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tracing::debug;

#[cfg(feature = "builtin-tools")]
use rayon::prelude::*;

pub fn register_builtin_tools(registry: &mut ToolRegistry) {
    registry.register(
        ToolDefinition::new_fn(
            "read",
            "Read the contents of a file at the given path. Returns content with line numbers.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"}},"required":["path"]}"#,
            exec_read,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "write",
            "Write content to a file, creating or overwriting it.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#,
            exec_write,
        )
        .with_effect(ToolEffect::Write),
    );
    registry.register(
        ToolDefinition::new_fn(
            "edit",
            "Perform a string replacement in a file. old_string must be unique.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}},"required":["path","old_string","new_string"]}"#,
            exec_edit,
        )
        .with_effect(ToolEffect::Write),
    );
    registry.register(
        ToolDefinition::new_fn(
            "bash",
            "Execute a shell command and return stdout/stderr. Optional timeout in seconds.",
            r#"{"type":"object","properties":{"command":{"type":"string"},"cwd":{"type":"string"},"timeout":{"type":"integer"}},"required":["command"]}"#,
            exec_bash,
        )
        .with_effect(ToolEffect::Process),
    );
    registry.register(
        ToolDefinition::new_fn(
            "grep",
            "Search file contents using regex. Returns matching lines with context.",
            r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"context":{"type":"integer"}},"required":["pattern"]}"#,
            exec_grep,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "find",
            "Find files by glob pattern. Uses rayon for parallel traversal.",
            r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}"#,
            exec_find,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "ls",
            "List entries in a directory.",
            r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
            exec_ls,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "web_fetch",
            "HTTP GET a URL and return response text (truncated). Requires providers feature.",
            r#"{"type":"object","properties":{"url":{"type":"string"},"max_bytes":{"type":"integer"}},"required":["url"]}"#,
            exec_web_fetch,
        )
        .with_effect(ToolEffect::Network),
    );
    registry.register(
        ToolDefinition::new_fn(
            "todo",
            "Manage an in-memory session todo list. Actions: list, add, update, complete, clear.",
            r#"{"type":"object","properties":{"action":{"type":"string","enum":["list","add","update","complete","clear"]},"items":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"content":{"type":"string"},"status":{"type":"string"}},"required":["content"]}}},"required":["action"]}"#,
            exec_todo,
        )
        .with_effect(ToolEffect::Write),
    );
    registry.register(
        ToolDefinition::new_fn(
            "spawn_agent",
            "Spawn a nested subagent with a prompt. Uses ToolContext provider/tools when present.",
            r#"{"type":"object","properties":{"prompt":{"type":"string"},"name":{"type":"string"},"model":{"type":"string"},"isolate":{"type":"boolean"}},"required":["prompt"]}"#,
            exec_spawn_agent,
        )
        .with_effect(ToolEffect::Process),
    );
    registry.register(
        ToolDefinition::new_fn(
            "enter_plan_mode",
            "Request Plan scope and return plan-mode instructions for the model.",
            r#"{"type":"object","properties":{}}"#,
            exec_enter_plan_mode,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "exit_plan_mode",
            "Request Coding scope and leave plan mode.",
            r#"{"type":"object","properties":{}}"#,
            exec_exit_plan_mode,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "lsp_diagnostics",
            "Get LSP diagnostics for a document URI and language.",
            r#"{"type":"object","properties":{"uri":{"type":"string"},"language":{"type":"string"}},"required":["uri","language"]}"#,
            exec_lsp_diagnostics,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "lsp_definition",
            "Resolve definition locations via LSP.",
            r#"{"type":"object","properties":{"uri":{"type":"string"},"language":{"type":"string"},"line":{"type":"integer"},"character":{"type":"integer"}},"required":["uri","language","line","character"]}"#,
            exec_lsp_definition,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "lsp_references",
            "Find references via LSP.",
            r#"{"type":"object","properties":{"uri":{"type":"string"},"language":{"type":"string"},"line":{"type":"integer"},"character":{"type":"integer"}},"required":["uri","language","line","character"]}"#,
            exec_lsp_references,
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
                    let mut mgr = manager.lock();
                    match mgr.spawn(config, &prompt, &ctx.workspace_root) {
                        Ok(handle) => {
                            let result = handle.wait_sync();
                            let body = serde_json::json!({
                                "id": handle.id(),
                                "name": handle.name(),
                                "status": format!("{:?}", handle.status()),
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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct TodoItem {
    id: String,
    content: String,
    status: String,
}

fn todo_store() -> &'static DashMap<String, Vec<TodoItem>> {
    static STORE: OnceLock<DashMap<String, Vec<TodoItem>>> = OnceLock::new();
    STORE.get_or_init(DashMap::new)
}

fn workspace_key(ctx: &ToolContext) -> String {
    ctx.workspace_root.to_string_lossy().into_owned()
}

fn exec_web_fetch(_ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let url = match parse_str_field(&args, "url") {
            Some(u) => u,
            None => return ToolResult::err("web_fetch", "url required"),
        };
        let max_bytes = parse_num_field(&args, "max_bytes").unwrap_or(100_000) as usize;
        let max_bytes = max_bytes.min(100_000);

        #[cfg(feature = "providers")]
        {
            let client = crate::http::global_client();
            match client.get(&url).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    match resp.bytes().await {
                        Ok(bytes) => {
                            let truncated = bytes.len() > max_bytes;
                            let slice = &bytes[..bytes.len().min(max_bytes)];
                            let mut text = String::from_utf8_lossy(slice).into_owned();
                            if truncated {
                                text.push_str(&format!(
                                    "\n\n[truncated to {max_bytes} bytes; total {}]",
                                    bytes.len()
                                ));
                            }
                            if !status.is_success() {
                                text = format!("HTTP {status}\n{text}");
                            }
                            ToolResult::ok("web_fetch", text)
                        }
                        Err(e) => ToolResult::err("web_fetch", format!("body read failed: {e}")),
                    }
                }
                Err(e) => ToolResult::err("web_fetch", format!("request failed: {e}")),
            }
        }

        #[cfg(not(feature = "providers"))]
        {
            let _ = max_bytes;
            ToolResult::err(
                "web_fetch",
                format!("providers feature required to fetch {url}"),
            )
        }
    })
}

fn exec_todo(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let v: serde_json::Value = match serde_json::from_str(&args) {
            Ok(v) => v,
            Err(e) => return ToolResult::err("todo", format!("invalid json: {e}")),
        };
        let action = match v.get("action").and_then(|a| a.as_str()) {
            Some(a) => a,
            None => return ToolResult::err("todo", "action required"),
        };
        let key = workspace_key(&ctx);
        let store = todo_store();

        match action {
            "list" => {
                let items = store.get(&key).map(|e| e.clone()).unwrap_or_default();
                ToolResult::ok(
                    "todo",
                    serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into()),
                )
            }
            "clear" => {
                store.insert(key, Vec::new());
                ToolResult::ok("todo", "[]")
            }
            "add" => {
                let mut items = store.get(&key).map(|e| e.clone()).unwrap_or_default();
                let incoming = v
                    .get("items")
                    .and_then(|i| i.as_array())
                    .cloned()
                    .unwrap_or_default();
                if incoming.is_empty() {
                    return ToolResult::err("todo", "items required for add");
                }
                for item in incoming {
                    let content = item
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    if content.is_empty() {
                        continue;
                    }
                    let id = item
                        .get("id")
                        .and_then(|i| i.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    let status = item
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("pending")
                        .to_string();
                    items.push(TodoItem {
                        id,
                        content,
                        status,
                    });
                }
                let out = serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into());
                store.insert(key, items);
                ToolResult::ok("todo", out)
            }
            "update" | "complete" => {
                let mut items = store.get(&key).map(|e| e.clone()).unwrap_or_default();
                let incoming = v
                    .get("items")
                    .and_then(|i| i.as_array())
                    .cloned()
                    .unwrap_or_default();
                if incoming.is_empty() {
                    return ToolResult::err("todo", format!("items required for {action}"));
                }
                for item in incoming {
                    let id = item.get("id").and_then(|i| i.as_str());
                    let content = item.get("content").and_then(|c| c.as_str());
                    let status = if action == "complete" {
                        Some("completed")
                    } else {
                        item.get("status").and_then(|s| s.as_str())
                    };
                    if let Some(id) = id {
                        if let Some(existing) = items.iter_mut().find(|t| t.id == id) {
                            if let Some(c) = content {
                                existing.content = c.to_string();
                            }
                            if let Some(s) = status {
                                existing.status = s.to_string();
                            }
                        }
                    } else if let Some(c) = content {
                        if let Some(existing) = items.iter_mut().find(|t| t.content == c) {
                            if let Some(s) = status {
                                existing.status = s.to_string();
                            }
                        }
                    }
                }
                let out = serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into());
                store.insert(key, items);
                ToolResult::ok("todo", out)
            }
            other => ToolResult::err("todo", format!("unknown action: {other}")),
        }
    })
}

fn exec_spawn_agent(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let v: serde_json::Value = match serde_json::from_str(&args) {
            Ok(v) => v,
            Err(e) => return ToolResult::err("spawn_agent", format!("invalid json: {e}")),
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
        let isolate = v.get("isolate").and_then(|i| i.as_bool()).unwrap_or(false);

        let mut manager = SubagentManager::new();
        if let Some(provider) = ctx.provider.clone() {
            manager = manager.with_provider(provider);
        }
        if let Some(tools) = ctx.tools.clone() {
            manager = manager.with_tools(tools);
        }
        let config = SubagentConfig {
            name: name.clone(),
            model,
            workspace_isolation: isolate,
            ..SubagentConfig::default()
        };
        match manager.spawn(config, &prompt, &ctx.workspace_root) {
            Ok(handle) => {
                let result = handle.wait_sync();
                let body = serde_json::json!({
                    "id": handle.id(),
                    "name": handle.name(),
                    "status": format!("{:?}", handle.status()),
                    "output": result.output,
                    "tool_calls": result.tool_calls,
                    "error": result.error,
                });
                ToolResult::ok("spawn_agent", body.to_string())
            }
            Err(e) => ToolResult::err("spawn_agent", e.to_string()),
        }
    })
}

fn exec_enter_plan_mode(ctx: Arc<ToolContext>, _args: String) -> ToolFuture {
    Box::pin(async move {
        if let Some(slot) = ctx.pending_scope.as_ref() {
            *slot.lock() = Some(Scope::Plan);
        }
        ToolResult::ok(
            "enter_plan_mode",
            "Entered plan mode. Produce a concrete multi-step plan: files, risks, verification. Do not modify the workspace. Host should set_scope(Plan).",
        )
    })
}

fn exec_exit_plan_mode(ctx: Arc<ToolContext>, _args: String) -> ToolFuture {
    Box::pin(async move {
        if let Some(slot) = ctx.pending_scope.as_ref() {
            *slot.lock() = Some(Scope::Coding);
        }
        ToolResult::ok(
            "exit_plan_mode",
            "Exited plan mode. Resume coding scope. Host should set_scope(Coding).",
        )
    })
}

fn exec_lsp_diagnostics(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        #[cfg(feature = "ipc")]
        {
            let uri = match parse_str_field(&args, "uri") {
                Some(u) => u,
                None => return ToolResult::err("lsp_diagnostics", "uri required"),
            };
            let language = match parse_str_field(&args, "language") {
                Some(l) => l,
                None => return ToolResult::err("lsp_diagnostics", "language required"),
            };
            let Some(lsp) = ctx.lsp.clone() else {
                return ToolResult::err("lsp_diagnostics", "lsp not configured");
            };
            match lsp.diagnostics(&uri, &language).await {
                Ok(diags) => ToolResult::ok(
                    "lsp_diagnostics",
                    serde_json::to_string_pretty(&diags).unwrap_or_else(|e| e.to_string()),
                ),
                Err(e) => ToolResult::err("lsp_diagnostics", e.to_string()),
            }
        }
        #[cfg(not(feature = "ipc"))]
        {
            let _ = (ctx, args);
            ToolResult::err("lsp_diagnostics", "lsp not configured")
        }
    })
}

fn exec_lsp_definition(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        #[cfg(feature = "ipc")]
        {
            let uri = match parse_str_field(&args, "uri") {
                Some(u) => u,
                None => return ToolResult::err("lsp_definition", "uri required"),
            };
            let language = match parse_str_field(&args, "language") {
                Some(l) => l,
                None => return ToolResult::err("lsp_definition", "language required"),
            };
            let line = match parse_num_field(&args, "line") {
                Some(n) => n as u32,
                None => return ToolResult::err("lsp_definition", "line required"),
            };
            let character = parse_num_field(&args, "character")
                .or_else(|| parse_num_field(&args, "char"))
                .unwrap_or(0) as u32;
            let Some(lsp) = ctx.lsp.clone() else {
                return ToolResult::err("lsp_definition", "lsp not configured");
            };
            match lsp.definition(&uri, &language, line, character).await {
                Ok(locs) => ToolResult::ok(
                    "lsp_definition",
                    serde_json::to_string_pretty(&locs).unwrap_or_else(|e| e.to_string()),
                ),
                Err(e) => ToolResult::err("lsp_definition", e.to_string()),
            }
        }
        #[cfg(not(feature = "ipc"))]
        {
            let _ = (ctx, args);
            ToolResult::err("lsp_definition", "lsp not configured")
        }
    })
}

fn exec_lsp_references(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        #[cfg(feature = "ipc")]
        {
            let uri = match parse_str_field(&args, "uri") {
                Some(u) => u,
                None => return ToolResult::err("lsp_references", "uri required"),
            };
            let language = match parse_str_field(&args, "language") {
                Some(l) => l,
                None => return ToolResult::err("lsp_references", "language required"),
            };
            let line = match parse_num_field(&args, "line") {
                Some(n) => n as u32,
                None => return ToolResult::err("lsp_references", "line required"),
            };
            let character = parse_num_field(&args, "character")
                .or_else(|| parse_num_field(&args, "char"))
                .unwrap_or(0) as u32;
            let Some(lsp) = ctx.lsp.clone() else {
                return ToolResult::err("lsp_references", "lsp not configured");
            };
            match lsp.references(&uri, &language, line, character).await {
                Ok(locs) => ToolResult::ok(
                    "lsp_references",
                    serde_json::to_string_pretty(&locs).unwrap_or_else(|e| e.to_string()),
                ),
                Err(e) => ToolResult::err("lsp_references", e.to_string()),
            }
        }
        #[cfg(not(feature = "ipc"))]
        {
            let _ = (ctx, args);
            ToolResult::err("lsp_references", "lsp not configured")
        }
    })
}

fn parse_str_field(args: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get(field)?.as_str().map(|s| s.to_string())
}

fn parse_num_field(args: &str, field: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get(field)?.as_u64()
}

fn resolve_path(ctx: &ToolContext, path: &str, write: bool) -> Result<std::path::PathBuf, String> {
    let p = std::path::PathBuf::from(path);
    let full = if p.is_absolute() {
        p
    } else {
        ctx.workspace_root.join(p)
    };
    if let Some(sb) = ctx.sandbox.as_ref() {
        sb.validate_path(&full, write).map_err(|e| e.to_string())?;
    }
    Ok(full)
}

fn exec_read(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let path = match parse_str_field(&args, "path") {
            Some(p) => p,
            None => return ToolResult::err("read", "path required"),
        };
        let offset = parse_num_field(&args, "offset").unwrap_or(0) as usize;
        let limit = parse_num_field(&args, "limit").unwrap_or(2000) as usize;

        let full = match resolve_path(&ctx, &path, false) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("read", e),
        };
        match std::fs::read_to_string(&full) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let start = offset.min(lines.len());
                let end = (start + limit).min(lines.len());
                let mut out = String::new();
                for (i, line) in lines[start..end].iter().enumerate() {
                    out.push_str(&format!("{:>6}\t{}\n", start + i + 1, line));
                }
                if out.is_empty() {
                    out = "(empty file)".to_string();
                }
                ToolResult::ok("read", out)
            }
            Err(e) => ToolResult::err("read", format!("{e}")),
        }
    })
}

fn exec_write(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let path = match parse_str_field(&args, "path") {
            Some(p) => p,
            None => return ToolResult::err("write", "path required"),
        };
        let content = match parse_str_field(&args, "content") {
            Some(c) => c,
            None => return ToolResult::err("write", "content required"),
        };
        let full = match resolve_path(&ctx, &path, true) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("write", e),
        };
        if let Some(parent) = full.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return ToolResult::err("write", format!("mkdir failed: {e}"));
                }
            }
        }
        match std::fs::write(&full, &content) {
            Ok(_) => {
                debug!("wrote {} bytes to {}", content.len(), full.display());
                ToolResult::ok(
                    "write",
                    format!("wrote {} bytes to {}", content.len(), path),
                )
            }
            Err(e) => ToolResult::err("write", format!("{e}")),
        }
    })
}

fn exec_edit(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let path = match parse_str_field(&args, "path") {
            Some(p) => p,
            None => return ToolResult::err("edit", "path required"),
        };
        let old_string = match parse_str_field(&args, "old_string") {
            Some(s) => s,
            None => return ToolResult::err("edit", "old_string required"),
        };
        let new_string = match parse_str_field(&args, "new_string") {
            Some(s) => s,
            None => return ToolResult::err("edit", "new_string required"),
        };
        let full = match resolve_path(&ctx, &path, true) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("edit", e),
        };
        let content = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(e) => return ToolResult::err("edit", format!("read failed: {e}")),
        };
        let occurrences = content.matches(&old_string).count();
        if occurrences == 0 {
            return ToolResult::err("edit", "old_string not found in file");
        }
        if occurrences > 1 {
            return ToolResult::err(
                "edit",
                format!("old_string found {occurrences} times — must be unique"),
            );
        }
        let new_content = content.replacen(&old_string, &new_string, 1);
        match std::fs::write(&full, &new_content) {
            Ok(_) => ToolResult::ok("edit", format!("edited {}", path)),
            Err(e) => ToolResult::err("edit", format!("write failed: {e}")),
        }
    })
}

fn exec_bash(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let command = match parse_str_field(&args, "command") {
            Some(c) => c,
            None => return ToolResult::err("bash", "command required"),
        };
        let cwd = parse_str_field(&args, "cwd");
        let timeout_secs = parse_num_field(&args, "timeout").unwrap_or(120);

        if let Some(sb) = ctx.sandbox.as_ref() {
            if let Err(e) = sb.validate_command(&command) {
                return ToolResult::err("bash", e.to_string());
            }
        }

        let working_dir = if let Some(cwd) = cwd {
            match resolve_path(&ctx, &cwd, false) {
                Ok(p) => p,
                Err(e) => return ToolResult::err("bash", e),
            }
        } else if let Some(sb) = ctx.sandbox.as_ref() {
            if let Err(e) = sb.validate_path(&ctx.workspace_root, false) {
                return ToolResult::err("bash", e.to_string());
            }
            ctx.workspace_root.clone()
        } else {
            ctx.workspace_root.clone()
        };

        let mut cmd = if let Some(os) = ctx.os_sandbox.as_ref() {
            // Wrap bash -c under seatbelt/bwrap when OS sandbox is configured.
            match os.command("bash", &["-c", &command]) {
                Ok(mut c) => {
                    c.current_dir(&working_dir);
                    c
                }
                Err(e) => return ToolResult::err("bash", e.to_string()),
            }
        } else if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&command);
            c.current_dir(&working_dir);
            c
        } else {
            let mut c = Command::new("bash");
            c.arg("-c").arg(&command);
            c.current_dir(&working_dir);
            c
        };

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolResult::err("bash", format!("failed to execute: {e}")),
        };

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            #[cfg(feature = "ipc")]
            if ctx.cancellation.is_canceled() {
                let _ = child.kill();
                let _ = child.wait();
                return ToolResult::err("bash", "command cancelled");
            }
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return ToolResult::err(
                            "bash",
                            format!("command timed out after {timeout_secs}s"),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return ToolResult::err("bash", format!("wait failed: {e}")),
            }
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut out) = child.stdout.take() {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            stdout = String::from_utf8_lossy(&buf).to_string();
        }
        if let Some(mut err) = child.stderr.take() {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
            stderr = String::from_utf8_lossy(&buf).to_string();
        }
        let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push_str("\n--- stderr ---\n");
            }
            result.push_str(&stderr);
        }
        if exit_code != 0 {
            result.push_str(&format!("\n(exit code: {exit_code})"));
        }
        if result.is_empty() {
            result = "(no output)".to_string();
        }
        ToolResult::ok("bash", result)
    })
}

fn exec_grep(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let pattern = match parse_str_field(&args, "pattern") {
            Some(p) => p,
            None => return ToolResult::err("grep", "pattern required"),
        };
        let path = parse_str_field(&args, "path").unwrap_or_else(|| ".".to_string());
        let context = parse_num_field(&args, "context").unwrap_or(0) as usize;

        let full = match resolve_path(&ctx, &path, false) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("grep", e),
        };

        #[cfg(feature = "builtin-tools")]
        {
            let regex = match regex::Regex::new(&pattern) {
                Ok(r) => r,
                Err(e) => return ToolResult::err("grep", format!("invalid regex: {e}")),
            };

            let files: Vec<std::path::PathBuf> = if full.is_file() {
                vec![full.clone()]
            } else {
                collect_files(&full)
            };

            let results: Vec<String> = files
                .par_iter()
                .filter_map(|file| {
                    let content = std::fs::read_to_string(file).ok()?;
                    let lines: Vec<&str> = content.lines().collect();
                    let mut matches = Vec::new();
                    for (i, line) in lines.iter().enumerate() {
                        if regex.is_match(line) {
                            let start = i.saturating_sub(context);
                            let end = (i + context + 1).min(lines.len());
                            for (j, ctx_line) in lines[start..end].iter().enumerate() {
                                let line_num = start + j + 1;
                                let marker = if start + j == i { ">" } else { " " };
                                matches.push(format!(
                                    "{marker} {:>6}\t{}:{line_num}\t{ctx_line}",
                                    "",
                                    file.file_name()?.to_string_lossy(),
                                    line_num = line_num
                                ));
                            }
                            if context > 0 && end < lines.len() {
                                matches.push("  ---".to_string());
                            }
                        }
                    }
                    if matches.is_empty() {
                        None
                    } else {
                        Some(matches.join("\n"))
                    }
                })
                .collect();

            if results.is_empty() {
                ToolResult::ok("grep", "(no matches)")
            } else {
                ToolResult::ok("grep", results.join("\n"))
            }
        }

        #[cfg(not(feature = "builtin-tools"))]
        {
            let _ = (pattern, context);
            ToolResult::err("grep", "builtin-tools feature not enabled")
        }
    })
}

fn exec_find(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let pattern = match parse_str_field(&args, "pattern") {
            Some(p) => p,
            None => return ToolResult::err("find", "pattern required"),
        };
        let path = parse_str_field(&args, "path").unwrap_or_else(|| ".".to_string());
        let full = match resolve_path(&ctx, &path, false) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("find", e),
        };

        #[cfg(feature = "builtin-tools")]
        {
            let files = collect_files(&full);
            let glob_pattern = match glob::Pattern::new(&pattern) {
                Ok(p) => p,
                Err(e) => return ToolResult::err("find", format!("invalid glob: {e}")),
            };

            let matched: Vec<String> = files
                .par_iter()
                .filter_map(|f| {
                    let name = f.file_name()?.to_string_lossy().to_string();
                    let path_str = f.to_string_lossy().to_string();
                    if glob_pattern.matches(&name) || glob_pattern.matches(&path_str) {
                        Some(path_str)
                    } else {
                        None
                    }
                })
                .collect();

            if matched.is_empty() {
                ToolResult::ok("find", "(no files found)")
            } else {
                ToolResult::ok("find", matched.join("\n"))
            }
        }

        #[cfg(not(feature = "builtin-tools"))]
        {
            let _ = pattern;
            ToolResult::err("find", "builtin-tools feature not enabled")
        }
    })
}

fn exec_ls(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let path = match parse_str_field(&args, "path") {
            Some(p) => p,
            None => return ToolResult::err("ls", "path required"),
        };
        let full = match resolve_path(&ctx, &path, false) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("ls", e),
        };
        match std::fs::read_dir(&full) {
            Ok(entries) => {
                let mut items: Vec<(String, bool)> = entries
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        let is_dir = e.file_type().ok()?.is_dir();
                        Some((name, is_dir))
                    })
                    .collect();
                items.sort_by(|a, b| a.0.cmp(&b.0));
                let out: Vec<String> = items
                    .iter()
                    .map(|(name, is_dir)| {
                        if *is_dir {
                            format!("{name}/")
                        } else {
                            name.clone()
                        }
                    })
                    .collect();
                if out.is_empty() {
                    ToolResult::ok("ls", "(empty directory)")
                } else {
                    ToolResult::ok("ls", out.join("\n"))
                }
            }
            Err(e) => ToolResult::err("ls", format!("{e}")),
        }
    })
}

#[cfg(feature = "builtin-tools")]
fn collect_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    use rayon::prelude::*;
    let mut files = Vec::new();

    if root.is_file() {
        files.push(root.to_path_buf());
        return files;
    }

    #[cfg(feature = "builtin-tools")]
    {
        let walker = ignore::WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .build();

        let entries: Vec<_> = walker.filter_map(|e| e.ok()).collect();
        files = entries
            .par_iter()
            .filter_map(|e| {
                if e.file_type()?.is_file() {
                    Some(e.path().to_path_buf())
                } else {
                    None
                }
            })
            .collect();
    }

    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_and_read() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));

        let write_result = exec_write(
            ctx.clone(),
            r#"{"path":"test.txt","content":"hello world"}"#.to_string(),
        )
        .await;
        assert!(!write_result.is_error);

        let read_result = exec_read(ctx, r#"{"path":"test.txt"}"#.to_string()).await;
        assert!(!read_result.is_error);
        assert!(read_result.content.contains("hello world"));
    }

    #[tokio::test]
    async fn test_edit() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));

        exec_write(
            ctx.clone(),
            r#"{"path":"edit.txt","content":"foo bar baz"}"#.to_string(),
        )
        .await;
        let edit_result = exec_edit(
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
        let result = exec_bash(ctx, r#"{"command":"echo hello"}"#.to_string()).await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_bash_timeout() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));
        let result = exec_bash(ctx, r#"{"command":"sleep 2","timeout":1}"#.to_string()).await;
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
        let result = exec_bash(Arc::new(ctx), r#"{"command":"sleep 5"}"#.to_string()).await;
        assert!(result.is_error);
        assert_eq!(result.content, "command cancelled");
    }

    #[tokio::test]
    async fn test_todo_add_list_complete() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));
        let add = exec_todo(
            ctx.clone(),
            r#"{"action":"add","items":[{"id":"1","content":"ship tools"}]}"#.to_string(),
        )
        .await;
        assert!(!add.is_error, "{}", add.content);
        assert!(add.content.contains("ship tools"));

        let listed = exec_todo(ctx.clone(), r#"{"action":"list"}"#.to_string()).await;
        assert!(!listed.is_error);
        assert!(listed.content.contains("ship tools"));

        let done = exec_todo(
            ctx.clone(),
            r#"{"action":"complete","items":[{"id":"1"}]}"#.to_string(),
        )
        .await;
        assert!(!done.is_error);
        assert!(done.content.contains("completed"));

        let _ = exec_todo(ctx, r#"{"action":"clear"}"#.to_string()).await;
    }

    #[tokio::test]
    async fn test_enter_plan_mode_sets_pending_scope() {
        let pending = Arc::new(parking_lot::Mutex::new(None));
        let mut ctx = ToolContext::new(".");
        ctx.pending_scope = Some(Arc::clone(&pending));
        let result = exec_enter_plan_mode(Arc::new(ctx), "{}".to_string()).await;
        assert!(!result.is_error);
        assert_eq!(*pending.lock(), Some(Scope::Plan));
    }

    #[tokio::test]
    async fn test_web_fetch_without_providers_or_offline() {
        let ctx = Arc::new(ToolContext::new("."));
        let result = exec_web_fetch(ctx, r#"{"url":"https://example.invalid/"}"#.to_string()).await;
        assert!(result.is_error);
        assert!(
            result.content.contains("providers feature required")
                || result.content.contains("request failed")
                || result.content.contains("error")
        );
    }
}
