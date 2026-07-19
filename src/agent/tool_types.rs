//! Tool call/result types, registry, and execution context.

use crate::mode::Scope;
use crate::provider::Provider;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tracing::info;

#[cfg(feature = "ipc")]
use cancellation_token::{CancellationToken, CancellationTokenSource};
#[cfg(feature = "ipc")]
use parking_lot::RwLock;

pub fn normalize_tool_name(name: &str) -> &str {
    match name {
        "read_file" | "read" => "read",
        "write_file" | "write" => "write",
        "list_dir" | "ls" => "ls",
        "run_command" | "bash" => "bash",
        "find_files" | "find" => "find",
        "code_intel" | "grep" => "grep",
        "hashline_edit" | "search_replace" | "apply_patch" | "edit" => "edit",
        "spawn_agent" | "agent" => "spawn_agent",
        "web_fetch" | "fetch" | "fetch_url" => "web_fetch",
        "todo" | "todo_write" | "todo_list" => "todo",
        "enter_plan_mode" | "plan_mode" => "enter_plan_mode",
        "exit_plan_mode" => "exit_plan_mode",
        "lsp_diagnostics" | "diagnostics" => "lsp_diagnostics",
        "lsp_definition" | "definition" | "go_to_definition" => "lsp_definition",
        "lsp_references" | "references" | "find_references" => "lsp_references",
        _ => name,
    }
}

pub type ToolFuture = Pin<Box<dyn Future<Output = ToolResult> + Send>>;

#[cfg(feature = "ipc")]
#[derive(Clone)]
pub struct CancellationHandle {
    source: Arc<RwLock<CancellationTokenSource>>,
}

#[cfg(feature = "ipc")]
impl CancellationHandle {
    pub(crate) fn new() -> Self {
        Self {
            source: Arc::new(RwLock::new(CancellationTokenSource::new())),
        }
    }

    pub fn cancel(&self) {
        self.source.read().cancel();
    }

    pub(crate) fn reset(&self) -> CancellationToken {
        let source = CancellationTokenSource::new();
        let token = source.token();
        *self.source.write() = source;
        token
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub id: String,
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            is_error: false,
        }
    }
    pub fn err(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

/// Context passed to tool execution — provides workspace root, cancellation, etc.
pub struct ToolContext {
    pub workspace_root: std::path::PathBuf,
    #[cfg(feature = "ipc")]
    pub cancellation: CancellationToken,
    pub sandbox: Option<std::sync::Arc<crate::sandbox::SandboxManager>>,
    pub os_sandbox: Option<std::sync::Arc<crate::sandbox::OsSandboxRunner>>,
    /// Optional provider so nested tools (e.g. spawn_agent) can run an agent loop.
    pub provider: Option<Arc<dyn Provider>>,
    /// Optional tool registry for nested agent runs.
    pub tools: Option<Arc<ToolRegistry>>,
    /// Tools may request a scope switch; Agent applies after the tool batch.
    pub pending_scope: Option<Arc<parking_lot::Mutex<Option<Scope>>>>,
    /// Optional LSP manager for diagnostics / navigation tools.
    #[cfg(feature = "ipc")]
    pub lsp: Option<Arc<crate::lsp::LspManager>>,
}

impl ToolContext {
    pub fn new(workspace_root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            #[cfg(feature = "ipc")]
            cancellation: CancellationToken::new(false),
            sandbox: None,
            os_sandbox: None,
            provider: None,
            tools: None,
            pending_scope: None,
            #[cfg(feature = "ipc")]
            lsp: None,
        }
    }

    pub fn with_sandbox(mut self, sb: Arc<crate::sandbox::SandboxManager>) -> Self {
        self.sandbox = Some(sb);
        self
    }

    pub fn with_os_sandbox(mut self, os: Arc<crate::sandbox::OsSandboxRunner>) -> Self {
        self.os_sandbox = Some(os);
        self
    }
}

/// Function-pointer tool (for simple builtins).
pub type ToolExecuteFn = fn(Arc<ToolContext>, String) -> ToolFuture;

/// Boxed-closure tool (for stateful tools that capture external state).
pub type ToolExecuteBox = Box<dyn Fn(Arc<ToolContext>, String) -> ToolFuture + Send + Sync>;

/// Tool executor — either a function pointer or a boxed closure.
pub enum ToolExecutor {
    Fn(ToolExecuteFn),
    Boxed(ToolExecuteBox),
}

impl ToolExecutor {
    /// Execute the tool, dispatching to the appropriate variant.
    pub fn call(&self, ctx: Arc<ToolContext>, args: String) -> ToolFuture {
        match self {
            ToolExecutor::Fn(f) => f(ctx, args),
            ToolExecutor::Boxed(b) => b(ctx, args),
        }
    }
}

/// Tool effect class — determines parallel execution eligibility (codex-rs pattern).
/// Read-only tools can run in parallel; write/process tools are serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    Read,
    Write,
    Network,
    Process,
}

impl ToolEffect {
    /// Returns true if this tool can run in parallel with other read tools.
    pub fn supports_parallel(self) -> bool {
        matches!(self, ToolEffect::Read | ToolEffect::Network)
    }
}

pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters_json: String,
    pub execute: ToolExecutor,
    pub effect: ToolEffect,
}

impl ToolDefinition {
    pub fn new_fn(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_json: impl Into<String>,
        execute: ToolExecuteFn,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters_json: parameters_json.into(),
            execute: ToolExecutor::Fn(execute),
            effect: ToolEffect::Read,
        }
    }

    /// Create a tool definition with a boxed closure executor (for stateful tools).
    pub fn new_boxed(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_json: impl Into<String>,
        execute: ToolExecuteBox,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters_json: parameters_json.into(),
            execute: ToolExecutor::Boxed(execute),
            effect: ToolEffect::Read,
        }
    }

    pub fn with_effect(mut self, effect: ToolEffect) -> Self {
        self.effect = effect;
        self
    }
}

/// Concurrent tool registry using dashmap (grok pattern).
pub struct ToolRegistry {
    tools: dashmap::DashMap<String, ToolDefinition>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: dashmap::DashMap::new(),
        }
    }

    pub fn register(&mut self, tool: ToolDefinition) {
        info!("registered tool: {}", tool.name);
        self.tools.insert(tool.name.clone(), tool);
    }

    pub fn count(&self) -> usize {
        self.tools.len()
    }

    pub fn definitions(&self) -> Vec<serde_json::Value> {
        self.tools.iter().map(|t| serde_json::json!({
            "name": t.name,
            "description": t.description,
            "parameters": serde_json::from_str::<serde_json::Value>(&t.parameters_json).unwrap_or(serde_json::Value::Null),
        })).collect()
    }

    pub async fn execute(
        &self,
        name: &str,
        ctx: &Arc<ToolContext>,
        arguments: &str,
    ) -> Option<ToolResult> {
        let entry = self.tools.get(name)?;
        Some(entry.execute.call(ctx.clone(), arguments.to_string()).await)
    }

    /// Get the effect class for a tool.
    /// Unknown tools default to Process (serial, no cache) — safer than Read.
    pub fn effect_of(&self, name: &str) -> ToolEffect {
        self.tools
            .get(name)
            .map(|e| e.effect)
            .unwrap_or(ToolEffect::Process)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
