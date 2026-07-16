//! QuickJS-backed runtime for pi extensions.
//!
//! Uses rquickjs to evaluate extension JavaScript in-process. Arguments and
//! results are marshaled via serde_json through JSON strings. TypeScript type
//! annotations are stripped with a simple regex-based pass before evaluation.

use crate::pi::extension::{ExtensionError, PiExtension, PiExtensionRuntime};
use crate::pi::policy::{PiCapability, PolicyDecision};
use dashmap::DashMap;
use rquickjs::{Context, Function, Runtime};

/// Per-extension QuickJS state.
struct ExtensionState {
    rt: Runtime,
    ctx: Context,
}

/// QuickJS-backed implementation of [`PiExtensionRuntime`].
///
/// Holds one QuickJS runtime/context per initialized extension, keyed by
/// extension id. Capability enforcement is applied before each hostcall using
/// the extension's [`PiCapabilityPolicy`].
pub struct QuickjsRuntime {
    states: DashMap<String, ExtensionState>,
}

impl QuickjsRuntime {
    /// Create a new empty QuickJS runtime manager.
    pub fn new() -> Self {
        Self {
            states: DashMap::new(),
        }
    }
}

impl Default for QuickjsRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl PiExtensionRuntime for QuickjsRuntime {
    async fn init(&self, ext: &PiExtension) -> Result<(), ExtensionError> {
        if self.states.contains_key(&ext.manifest.id) {
            return Ok(());
        }
        let entry_path = ext.dir.join(&ext.manifest.entry);
        let source = std::fs::read_to_string(&entry_path)
            .map_err(|e| ExtensionError::LoadFailed(e.to_string()))?;
        let source = strip_typescript(&source);

        let rt = Runtime::new().map_err(|e| ExtensionError::RuntimeUnavailable(e.to_string()))?;
        let limit = (ext.policy.max_memory_mb as usize) * 1024 * 1024;
        rt.set_memory_limit(limit);
        rt.set_max_stack_size(1024 * 1024);

        let ctx =
            Context::full(&rt).map_err(|e| ExtensionError::RuntimeUnavailable(e.to_string()))?;

        ctx.with(|ctx| ctx.eval::<(), _>(source.as_str()))
            .map_err(|e| ExtensionError::ExecutionError(e.to_string()))?;
        drain_jobs(&rt);

        self.states
            .insert(ext.manifest.id.clone(), ExtensionState { rt, ctx });
        Ok(())
    }

    async fn hostcall(
        &self,
        ext: &PiExtension,
        cap: PiCapability,
        method: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, ExtensionError> {
        match ext.check_capability(cap) {
            PolicyDecision::Allow => {}
            PolicyDecision::Deny | PolicyDecision::Ask => {
                return Err(ExtensionError::ExecutionError(format!(
                    "capability {} denied for extension {}",
                    cap.as_str(),
                    ext.manifest.id
                )));
            }
        }

        let args_str = serde_json::to_string(&args)
            .map_err(|e| ExtensionError::ExecutionError(e.to_string()))?;

        let id = ext.manifest.id.clone();
        let state = self.states.get(&id).ok_or_else(|| {
            ExtensionError::RuntimeUnavailable(format!("extension {id} not initialized"))
        })?;

        let result: String = state.ctx.with(|ctx| {
            let globals = ctx.globals();
            let func: Function = globals
                .get("__hostcall")
                .map_err(|e| ExtensionError::ExecutionError(e.to_string()))?;
            func.call((method.to_string(), args_str))
                .map_err(|e| ExtensionError::ExecutionError(e.to_string()))
        })?;
        drain_jobs(&state.rt);

        let value: serde_json::Value = serde_json::from_str(&result)
            .map_err(|e| ExtensionError::ExecutionError(e.to_string()))?;
        Ok(value)
    }

    async fn execute_tool(
        &self,
        ext: &PiExtension,
        tool_name: &str,
        args: &str,
    ) -> Result<String, ExtensionError> {
        let id = ext.manifest.id.clone();
        let state = self.states.get(&id).ok_or_else(|| {
            ExtensionError::RuntimeUnavailable(format!("extension {id} not initialized"))
        })?;

        let result: String = state.ctx.with(|ctx| {
            let globals = ctx.globals();
            let func: Function = globals
                .get("__execute_tool")
                .map_err(|e| ExtensionError::ExecutionError(e.to_string()))?;
            func.call((tool_name.to_string(), args.to_string()))
                .map_err(|e| ExtensionError::ExecutionError(e.to_string()))
        })?;
        drain_jobs(&state.rt);
        Ok(result)
    }

    async fn shutdown(&self, ext: &PiExtension) -> Result<(), ExtensionError> {
        if let Some((_, state)) = self.states.remove(&ext.manifest.id) {
            state.rt.run_gc();
        }
        Ok(())
    }
}

/// Drain pending QuickJS microtasks until the job queue is empty or an
/// exception is thrown.
fn drain_jobs(rt: &Runtime) {
    loop {
        match rt.execute_pending_job() {
            Ok(true) => {}
            Ok(false) => break,
            Err(_) => break,
        }
    }
}

/// Strip TypeScript type annotations and interface blocks from a JS source.
///
/// This is a best-effort, regex-based pass sufficient for common extension
/// TypeScript. It removes `interface { ... }` blocks and `: Type` annotations
/// for recognized TypeScript types (primitive keywords and PascalCase types)
/// preceding `=`, `,`, `)`, `;`, `}` or a newline. Lowercase identifiers are
/// left untouched so plain JavaScript object literals are preserved.
fn strip_typescript(src: &str) -> String {
    let interface_re =
        regex::Regex::new(r"interface\s+\w+\s*(?:extends\s+[\w,\s]+)?\s*\{[^}]*\}").unwrap();
    let mut out = interface_re.replace_all(src, "").to_string();

    let anno_re = regex::Regex::new(
        r":\s*(?:string|number|boolean|symbol|bigint|any|void|unknown|never|[A-Z][\w$]*(?:<[^>]*>|\[\])*)\s*([=,);}{\n])",
    )
    .unwrap();
    out = anno_re.replace_all(&out, "$1").to_string();

    out
}

#[cfg(test)]
#[cfg(feature = "pi-extensions")]
mod tests {
    use super::*;
    use crate::pi::extension::PiExtensionManifest;
    use crate::pi::policy::PiCapabilityPolicy;
    use tempfile::TempDir;

    fn make_ext(
        dir: &std::path::Path,
        js: &str,
        tools: Vec<String>,
        policy: PiCapabilityPolicy,
    ) -> PiExtension {
        std::fs::write(dir.join("index.js"), js).unwrap();
        let manifest = PiExtensionManifest {
            id: "test-ext".into(),
            name: "Test".into(),
            version: "1.0.0".into(),
            description: "".into(),
            author: None,
            capabilities: vec![],
            tools,
            providers: vec![],
            entry: "index.js".into(),
            homepage: None,
            license: None,
        };
        let mut ext = PiExtension::new(manifest, dir.to_path_buf());
        ext.policy = policy;
        ext.acknowledge();
        ext.trust();
        ext
    }

    #[tokio::test]
    async fn init_simple_extension() {
        let tmp = TempDir::new().unwrap();
        let js = "var __hostcall = function(m, a) { return a; }; var __execute_tool = function(n, a) { return 'ok'; };";
        let ext = make_ext(tmp.path(), js, vec![], PiCapabilityPolicy::permissive());
        let rt = QuickjsRuntime::new();
        rt.init(&ext).await.unwrap();
        rt.shutdown(&ext).await.unwrap();
    }

    #[tokio::test]
    async fn execute_tool_basic() {
        let tmp = TempDir::new().unwrap();
        let js = "var __execute_tool = function(name, args) { return name + ':' + args; }; var __hostcall = function(m, a) { return a; };";
        let ext = make_ext(
            tmp.path(),
            js,
            vec!["greet".into()],
            PiCapabilityPolicy::permissive(),
        );
        let rt = QuickjsRuntime::new();
        rt.init(&ext).await.unwrap();
        let result = rt.execute_tool(&ext, "greet", "world").await.unwrap();
        assert_eq!(result, "greet:world");
        rt.shutdown(&ext).await.unwrap();
    }

    #[tokio::test]
    async fn hostcall_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let js = "var __hostcall = function(method, args) { var parsed = JSON.parse(args); return JSON.stringify({ method: method, x: parsed.x }); };";
        let ext = make_ext(tmp.path(), js, vec![], PiCapabilityPolicy::permissive());
        let rt = QuickjsRuntime::new();
        rt.init(&ext).await.unwrap();
        let result = rt
            .hostcall(
                &ext,
                PiCapability::Read,
                "ping",
                serde_json::json!({"x": 1}),
            )
            .await
            .unwrap();
        assert_eq!(result["method"], "ping");
        assert_eq!(result["x"], 1);
        rt.shutdown(&ext).await.unwrap();
    }

    #[tokio::test]
    async fn capability_denial() {
        let tmp = TempDir::new().unwrap();
        let js = "var __hostcall = function(m, a) { return a; };";
        let ext = make_ext(tmp.path(), js, vec![], PiCapabilityPolicy::strict());
        let rt = QuickjsRuntime::new();
        rt.init(&ext).await.unwrap();
        let result = rt
            .hostcall(&ext, PiCapability::Exec, "run", serde_json::Value::Null)
            .await;
        assert!(result.is_err());
        rt.shutdown(&ext).await.unwrap();
    }

    #[tokio::test]
    async fn typescript_stripping() {
        let tmp = TempDir::new().unwrap();
        let js = "interface Foo { a: number; }\nvar __execute_tool = function(name: string, args: string): string { return name + ':' + args; };\nvar __hostcall = function(m: string, a: string): string { return a; };";
        let ext = make_ext(
            tmp.path(),
            js,
            vec!["greet".into()],
            PiCapabilityPolicy::permissive(),
        );
        let rt = QuickjsRuntime::new();
        rt.init(&ext).await.unwrap();
        let result = rt.execute_tool(&ext, "greet", "world").await.unwrap();
        assert_eq!(result, "greet:world");
        rt.shutdown(&ext).await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let ext = make_ext(
            tmp.path(),
            "var __hostcall = function(m,a){return a;};",
            vec![],
            PiCapabilityPolicy::permissive(),
        );
        let rt = QuickjsRuntime::new();
        rt.init(&ext).await.unwrap();
        rt.shutdown(&ext).await.unwrap();
        rt.shutdown(&ext).await.unwrap();
    }

    #[test]
    fn strip_typescript_removes_interface() {
        let src = "interface Foo { a: number; b: string; }\nvar x = 1;";
        let out = strip_typescript(src);
        assert!(!out.contains("interface"));
        assert!(out.contains("var x = 1"));
    }

    #[test]
    fn strip_typescript_passes_plain_js() {
        let src = "var f = function(a, b) { return a + b; };";
        assert_eq!(strip_typescript(src), src);
    }
}
