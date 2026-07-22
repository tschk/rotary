use super::common::{parse_num_field, parse_str_field};
use crate::agent::{ToolContext, ToolFuture, ToolResult};
use crate::mode::Scope;
use crate::subagent::{SubagentConfig, SubagentManager};
use dashmap::DashMap;
use std::sync::{Arc, OnceLock};

/// Validate a URL for web_fetch: reject loopback, link-local, private IPs,
/// and cloud metadata endpoints. Returns Ok(()) if safe, Err(msg) if blocked.
#[allow(dead_code)] // used when `providers` feature is enabled
fn validate_fetch_url(url: &str) -> Result<(), String> {
    // Extract host from URL without external crate.
    let after_scheme = url.find("://").map(|i| &url[i + 3..]).unwrap_or(url);
    let host = after_scheme
        .split('@')
        .next_back()
        .unwrap_or(after_scheme)
        .split('/')
        .next()
        .unwrap_or(after_scheme)
        .split(':')
        .next()
        .unwrap_or(after_scheme)
        .trim_start_matches('[')
        .trim_end_matches(']');

    if host.is_empty() {
        return Err("URL has no host".into());
    }

    // Reject metadata endpoints.
    if host == "169.254.169.254" || host == "metadata.google.internal" {
        return Err("cloud metadata endpoint blocked".into());
    }

    // Reject localhost variants.
    if host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "0.0.0.0" {
        return Err(format!("loopback host blocked: {host}"));
    }

    // Reject link-local.
    if host.starts_with("169.254.") || host.starts_with("fe80:") {
        return Err(format!("link-local address blocked: {host}"));
    }

    // Reject private networks.
    if host.starts_with("10.") || host.starts_with("192.168.") {
        return Err(format!("private network address blocked: {host}"));
    }
    if host.starts_with("172.") {
        if let Some(octet) = host.split('.').nth(1) {
            if let Ok(n) = octet.parse::<u8>() {
                if (16..=31).contains(&n) {
                    return Err(format!("private network address blocked: {host}"));
                }
            }
        }
    }

    Ok(())
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

pub(crate) fn exec_web_fetch(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let url = match parse_str_field(&args, "url") {
            Some(u) => u,
            None => return ToolResult::err("web_fetch", "url required"),
        };
        let max_bytes = parse_num_field(&args, "max_bytes").unwrap_or(100_000) as usize;
        let max_bytes = max_bytes.min(100_000);

        if let Some(sb) = ctx.sandbox.as_ref() {
            if let Err(e) = sb.validate_network() {
                return ToolResult::err("web_fetch", e.to_string());
            }
        }

        #[cfg(feature = "providers")]
        {
            // Reject destinations that should not be fetched: loopback,
            // link-local, private, and cloud-metadata addresses.
            if let Err(e) = validate_fetch_url(&url) {
                return ToolResult::err("web_fetch", e);
            }
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

pub(crate) fn exec_todo(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
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

pub(crate) fn exec_spawn_agent(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
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

pub(crate) fn exec_enter_plan_mode(ctx: Arc<ToolContext>, _args: String) -> ToolFuture {
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

pub(crate) fn exec_exit_plan_mode(ctx: Arc<ToolContext>, _args: String) -> ToolFuture {
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

pub(crate) fn exec_lsp_diagnostics(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
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

pub(crate) fn exec_lsp_definition(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
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

pub(crate) fn exec_lsp_references(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
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
