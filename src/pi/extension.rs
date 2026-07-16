//! Pi extension protocol — manifest, manager, runtime trait.
//!
//! Compatible with pi_agent_rust extension system:
//! - Extensions declare capabilities in a manifest
//! - Capability policy gates each hostcall
//! - Runtime can be QuickJS (in-process) or subprocess
//! - Trust-state lifecycle: pending → acknowledged → trusted → killed

use crate::pi::policy::{PiCapability, PiCapabilityPolicy, PolicyDecision};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

/// Extension manifest — describes a pi extension's metadata and capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiExtensionManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub entry: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
}

impl PiExtensionManifest {
    /// Load from a manifest.json file.
    pub fn load(path: &std::path::Path) -> Result<Self, ExtensionError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| ExtensionError::LoadFailed(e.to_string()))?;
        let manifest: Self = serde_json::from_str(&content)
            .map_err(|e| ExtensionError::ParseFailed(e.to_string()))?;
        Ok(manifest)
    }

    /// Load from a directory (looks for manifest.json or package.json).
    pub fn load_from_dir(dir: &std::path::Path) -> Result<Self, ExtensionError> {
        let manifest_path = dir.join("manifest.json");
        if manifest_path.exists() {
            return Self::load(&manifest_path);
        }
        let pkg_path = dir.join("package.json");
        if pkg_path.exists() {
            let content = std::fs::read_to_string(&pkg_path)
                .map_err(|e| ExtensionError::LoadFailed(e.to_string()))?;
            let pkg: serde_json::Value = serde_json::from_str(&content)
                .map_err(|e| ExtensionError::ParseFailed(e.to_string()))?;
            let id = pkg
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let version = pkg
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("0.0.0")
                .to_string();
            let description = pkg
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let entry = pkg
                .get("main")
                .and_then(|v| v.as_str())
                .unwrap_or("index.js")
                .to_string();
            let caps = pkg
                .get("capabilities")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let tools = pkg
                .get("tools")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let name = id.clone();
            return Ok(Self {
                id,
                name,
                version,
                description,
                author: None,
                capabilities: caps,
                tools,
                providers: vec![],
                entry,
                homepage: None,
                license: None,
            });
        }
        Err(ExtensionError::NotFound(
            "no manifest.json or package.json".into(),
        ))
    }

    pub fn has_capability(&self, cap: PiCapability) -> bool {
        self.capabilities.iter().any(|c| c == cap.as_str())
    }
}

/// Trust state for an extension (pi lifecycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustState {
    Pending,
    Acknowledged,
    Trusted,
    Killed,
}

/// A loaded pi extension.
pub struct PiExtension {
    pub manifest: PiExtensionManifest,
    pub dir: PathBuf,
    pub trust: TrustState,
    pub policy: PiCapabilityPolicy,
}

impl PiExtension {
    pub fn new(manifest: PiExtensionManifest, dir: PathBuf) -> Self {
        Self {
            manifest,
            dir,
            trust: TrustState::Pending,
            policy: PiCapabilityPolicy::default(),
        }
    }

    pub fn check_capability(&self, cap: PiCapability) -> PolicyDecision {
        self.policy.check(&self.manifest.id, cap)
    }

    pub fn acknowledge(&mut self) {
        self.trust = TrustState::Acknowledged;
        info!("extension {} acknowledged", self.manifest.id);
    }

    pub fn trust(&mut self) {
        self.trust = TrustState::Trusted;
        info!("extension {} trusted", self.manifest.id);
    }

    pub fn kill(&mut self) {
        self.trust = TrustState::Killed;
        warn!("extension {} killed", self.manifest.id);
    }

    pub fn is_alive(&self) -> bool {
        matches!(self.trust, TrustState::Acknowledged | TrustState::Trusted)
    }
}

/// Runtime trait for executing extension code.
/// Implementations can be QuickJS (in-process) or subprocess-based.
#[async_trait::async_trait]
pub trait PiExtensionRuntime: Send + Sync {
    /// Initialize the extension runtime.
    async fn init(&self, ext: &PiExtension) -> Result<(), ExtensionError>;

    /// Execute a hostcall from the extension.
    async fn hostcall(
        &self,
        ext: &PiExtension,
        cap: PiCapability,
        method: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, ExtensionError>;

    /// Execute a tool defined by the extension.
    async fn execute_tool(
        &self,
        ext: &PiExtension,
        tool_name: &str,
        args: &str,
    ) -> Result<String, ExtensionError>;

    /// Shutdown the runtime for this extension.
    async fn shutdown(&self, ext: &PiExtension) -> Result<(), ExtensionError>;
}

/// Stub runtime — does nothing, returns errors. Used when pi-extensions feature is off.
pub struct StubRuntime;

#[async_trait::async_trait]
impl PiExtensionRuntime for StubRuntime {
    async fn init(&self, _ext: &PiExtension) -> Result<(), ExtensionError> {
        Err(ExtensionError::RuntimeUnavailable(
            "pi-extensions feature not enabled".into(),
        ))
    }
    async fn hostcall(
        &self,
        _ext: &PiExtension,
        _cap: PiCapability,
        _method: &str,
        _args: serde_json::Value,
    ) -> Result<serde_json::Value, ExtensionError> {
        Err(ExtensionError::RuntimeUnavailable(
            "pi-extensions feature not enabled".into(),
        ))
    }
    async fn execute_tool(
        &self,
        _ext: &PiExtension,
        _tool: &str,
        _args: &str,
    ) -> Result<String, ExtensionError> {
        Err(ExtensionError::RuntimeUnavailable(
            "pi-extensions feature not enabled".into(),
        ))
    }
    async fn shutdown(&self, _ext: &PiExtension) -> Result<(), ExtensionError> {
        Ok(())
    }
}

/// Extension manager — discovers, loads, and manages pi extensions.
pub struct PiExtensionManager {
    extensions: DashMap<String, PiExtension>,
    runtime: Arc<dyn PiExtensionRuntime>,
    extensions_dir: PathBuf,
}

impl PiExtensionManager {
    pub fn new(extensions_dir: PathBuf) -> Self {
        Self {
            extensions: DashMap::new(),
            runtime: Arc::new(StubRuntime),
            extensions_dir,
        }
    }

    pub fn with_runtime(mut self, runtime: Arc<dyn PiExtensionRuntime>) -> Self {
        self.runtime = runtime;
        self
    }

    /// Discover extensions in the extensions directory.
    pub fn discover(&self) -> Vec<PiExtensionManifest> {
        let mut found = Vec::new();
        if !self.extensions_dir.exists() {
            return found;
        }
        if let Ok(entries) = std::fs::read_dir(&self.extensions_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    match PiExtensionManifest::load_from_dir(&path) {
                        Ok(manifest) => {
                            info!(
                                "discovered extension: {} v{}",
                                manifest.id, manifest.version
                            );
                            found.push(manifest);
                        }
                        Err(e) => warn!("skipped extension at {}: {e}", path.display()),
                    }
                }
            }
        }
        found
    }

    /// Load and register an extension from a manifest.
    pub fn load(&self, manifest: PiExtensionManifest) -> Result<(), ExtensionError> {
        let dir = self.extensions_dir.join(&manifest.id);
        let ext = PiExtension::new(manifest, dir);
        let id = ext.manifest.id.clone();
        self.extensions.insert(id, ext);
        Ok(())
    }

    /// Get an extension by ID.
    pub fn get(&self, id: &str) -> Option<dashmap::mapref::one::Ref<'_, String, PiExtension>> {
        self.extensions.get(id)
    }

    /// Check if an extension is allowed a capability.
    pub fn check_capability(&self, ext_id: &str, cap: PiCapability) -> PolicyDecision {
        match self.extensions.get(ext_id) {
            Some(ext) => ext.check_capability(cap),
            None => PolicyDecision::Deny,
        }
    }

    /// Initialize an extension's runtime.
    pub async fn init_extension(&self, ext_id: &str) -> Result<(), ExtensionError> {
        let ext = self
            .extensions
            .get(ext_id)
            .ok_or_else(|| ExtensionError::NotFound(ext_id.into()))?;
        if !ext.is_alive() {
            return Err(ExtensionError::NotTrusted(ext_id.into()));
        }
        self.runtime.init(&ext).await
    }

    /// Execute a tool from an extension.
    pub async fn execute_tool(
        &self,
        ext_id: &str,
        tool_name: &str,
        args: &str,
    ) -> Result<String, ExtensionError> {
        let ext = self
            .extensions
            .get(ext_id)
            .ok_or_else(|| ExtensionError::NotFound(ext_id.into()))?;
        if !ext.is_alive() {
            return Err(ExtensionError::NotTrusted(ext_id.into()));
        }
        if !ext.manifest.tools.iter().any(|t| t == tool_name) {
            return Err(ExtensionError::ToolNotFound(tool_name.into()));
        }
        self.runtime.execute_tool(&ext, tool_name, args).await
    }

    /// List all registered extensions.
    pub fn list(&self) -> Vec<(String, String, TrustState)> {
        self.extensions
            .iter()
            .map(|e| (e.manifest.id.clone(), e.manifest.name.clone(), e.trust))
            .collect()
    }

    /// Count of registered extensions.
    pub fn count(&self) -> usize {
        self.extensions.len()
    }

    /// Acknowledge an extension (transition to acknowledged state).
    pub fn acknowledge(&self, ext_id: &str) -> Result<(), ExtensionError> {
        let mut ext = self
            .extensions
            .get_mut(ext_id)
            .ok_or_else(|| ExtensionError::NotFound(ext_id.into()))?;
        ext.acknowledge();
        Ok(())
    }

    /// Trust an extension (transition to trusted state).
    pub fn trust(&self, ext_id: &str) -> Result<(), ExtensionError> {
        let mut ext = self
            .extensions
            .get_mut(ext_id)
            .ok_or_else(|| ExtensionError::NotFound(ext_id.into()))?;
        ext.trust();
        Ok(())
    }

    /// Kill an extension (transition to killed state).
    pub fn kill(&self, ext_id: &str) -> Result<(), ExtensionError> {
        let mut ext = self
            .extensions
            .get_mut(ext_id)
            .ok_or_else(|| ExtensionError::NotFound(ext_id.into()))?;
        ext.kill();
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("extension not found: {0}")]
    NotFound(String),
    #[error("extension not trusted: {0}")]
    NotTrusted(String),
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("load failed: {0}")]
    LoadFailed(String),
    #[error("parse failed: {0}")]
    ParseFailed(String),
    #[error("runtime unavailable: {0}")]
    RuntimeUnavailable(String),
    #[error("execution error: {0}")]
    ExecutionError(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn manifest_from_json() {
        let json = r#"{
            "id": "test-ext",
            "name": "Test Extension",
            "version": "1.0.0",
            "description": "A test",
            "capabilities": ["read", "tool"],
            "tools": ["my_tool"],
            "entry": "index.js"
        }"#;
        let manifest: PiExtensionManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.id, "test-ext");
        assert!(manifest.has_capability(PiCapability::Read));
        assert!(!manifest.has_capability(PiCapability::Exec));
    }

    #[test]
    fn manager_discover_and_load() {
        let tmp = TempDir::new().unwrap();
        let ext_dir = tmp.path().join("extensions");
        let test_ext_dir = ext_dir.join("test-ext");
        std::fs::create_dir_all(&test_ext_dir).unwrap();
        std::fs::write(
            test_ext_dir.join("manifest.json"),
            r#"{"id":"test-ext","name":"Test","version":"1.0.0","capabilities":["read"],"tools":["foo"],"entry":"index.js"}"#,
        ).unwrap();

        let manager = PiExtensionManager::new(ext_dir);
        let mut found = manager.discover();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "test-ext");

        manager.load(found.remove(0)).unwrap();
        assert_eq!(manager.count(), 1);
        assert_eq!(
            manager.check_capability("test-ext", PiCapability::Read),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn trust_lifecycle() {
        let tmp = TempDir::new().unwrap();
        let manager = PiExtensionManager::new(tmp.path().into());
        let manifest = PiExtensionManifest {
            id: "ext1".into(),
            name: "Ext1".into(),
            version: "1.0".into(),
            description: "".into(),
            author: None,
            capabilities: vec![],
            tools: vec![],
            providers: vec![],
            entry: "index.js".into(),
            homepage: None,
            license: None,
        };
        manager.load(manifest).unwrap();

        assert!(manager.get("ext1").is_some());
        manager.acknowledge("ext1").unwrap();
        manager.trust("ext1").unwrap();
        assert!(manager.get("ext1").unwrap().is_alive());

        manager.kill("ext1").unwrap();
        assert!(!manager.get("ext1").unwrap().is_alive());
    }
}
