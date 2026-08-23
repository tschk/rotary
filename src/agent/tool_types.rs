//! Tool call/result types, registry, and execution context.

use crate::mode::Scope;
use crate::provider::Provider;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tracing::info;

use cancellation_token::{CancellationToken, CancellationTokenSource};
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
        "web_search" | "darash" | "darash_search" => "web_search",
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

#[derive(Clone)]
pub struct CancellationHandle {
    source: Arc<RwLock<CancellationTokenSource>>,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Structured classification for errors that require host approval.
    /// This avoids making the agent loop infer control flow from text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<ToolErrorKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolErrorKind {
    ApprovalRequired,
}

impl ToolResult {
    pub fn ok(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            is_error: false,
            error_kind: None,
        }
    }
    pub fn err(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            is_error: true,
            error_kind: None,
        }
    }

    pub fn approval_required(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content: "approval required".to_string(),
            is_error: true,
            error_kind: Some(ToolErrorKind::ApprovalRequired),
        }
    }

    pub fn requires_approval(&self) -> bool {
        self.error_kind == Some(ToolErrorKind::ApprovalRequired)
    }
}

/// Context passed to tool execution — provides workspace root, cancellation, etc.
pub struct ToolContext {
    pub workspace_root: std::path::PathBuf,
    pub cancellation: CancellationToken,
    pub sandbox: Option<std::sync::Arc<crate::sandbox::SandboxManager>>,
    pub os_sandbox: Option<std::sync::Arc<crate::sandbox::OsSandboxRunner>>,
    /// When true, policy requested OS sandboxing but the runner was unavailable.
    /// Shell tools must refuse execution rather than falling through to bare bash.
    pub os_sandbox_required: bool,
    /// Optional provider so nested tools (e.g. spawn_agent) can run an agent loop.
    pub provider: Option<Arc<dyn Provider>>,
    /// Optional tool registry for nested agent runs.
    pub tools: Option<Arc<ToolRegistry>>,
    /// Tools may request a scope switch; Agent applies after the tool batch.
    pub pending_scope: Option<Arc<parking_lot::Mutex<Option<Scope>>>>,
    /// Opt-in engine-owned todo state shared with the builtin todo executor.
    pub todo_state: Option<Arc<parking_lot::RwLock<crate::todo::TodoState>>>,
    /// Enables confidence-gated todo mutations when present.
    pub todo_config: Option<crate::todo::TodoConfig>,
    /// Todo snapshots accumulated during a tool batch for event emission.
    pub todo_updates: Option<Arc<parking_lot::Mutex<Vec<crate::todo::TodoState>>>>,
    /// Optional LSP manager for diagnostics / navigation tools.
    #[cfg(feature = "ipc")]
    pub lsp: Option<Arc<crate::lsp::LspManager>>,
    /// Last hashline read per path. `hashline_edit` fail-closes without a match.
    pub hashline_sight: Arc<parking_lot::RwLock<crate::hashline::HashlineSight>>,
}

impl ToolContext {
    pub fn new(workspace_root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            cancellation: CancellationToken::new(false),
            sandbox: None,
            os_sandbox: None,
            os_sandbox_required: false,
            provider: None,
            tools: None,
            pending_scope: None,
            todo_state: None,
            todo_config: None,
            todo_updates: None,
            #[cfg(feature = "ipc")]
            lsp: None,
            hashline_sight: Arc::new(parking_lot::RwLock::new(
                crate::hashline::HashlineSight::new(),
            )),
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
        let mut definitions: Vec<serde_json::Value> = self
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": serde_json::from_str::<serde_json::Value>(&t.parameters_json).unwrap_or(serde_json::Value::Null),
                })
            })
            .collect();
        // DashMap iteration order is intentionally unspecified. Stable tool
        // ordering keeps the serialized prompt prefix stable for providers
        // that cache it automatically.
        definitions.sort_by(|a, b| {
            a.get("name")
                .and_then(serde_json::Value::as_str)
                .cmp(&b.get("name").and_then(serde_json::Value::as_str))
        });
        definitions
    }

    /// Stable digest of the tool loadout, useful for host cache diagnostics.
    pub fn definitions_fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};

        let bytes = serde_json::to_vec(&self.definitions()).unwrap_or_default();
        let digest = Sha256::digest(bytes);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn noop(_ctx: Arc<ToolContext>, _args: String) -> ToolFuture {
        Box::pin(async { ToolResult::ok("noop", "ok") })
    }

    #[test]
    fn normalizes_web_search_aliases() {
        assert_eq!(normalize_tool_name("web_search"), "web_search");
        assert_eq!(normalize_tool_name("darash"), "web_search");
        assert_eq!(normalize_tool_name("darash_search"), "web_search");
    }

    #[test]
    fn definitions_are_stable_and_fingerprinted() {
        let mut registry = ToolRegistry::new();
        registry.register(ToolDefinition::new_fn("zeta", "z", "{}", noop));
        registry.register(ToolDefinition::new_fn("alpha", "a", "{}", noop));
        let definitions = registry.definitions();
        let names: Vec<_> = definitions
            .iter()
            .map(|definition| definition["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
        assert_eq!(registry.definitions_fingerprint().len(), 64);
    }
}
