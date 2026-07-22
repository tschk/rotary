//! Marketplace: plugin discovery, installation, and lifecycle management.
//!
//! A marketplace is a JSON index of plugin manifests. Each manifest describes
//! a plugin's metadata, MCP servers, skills, hooks, and dependencies. The
//! [`PluginInstaller`] clones plugins from git URLs (or copies from local
//! paths) into `~/.agents/plugins/{name}/` and records an [`InstalledPlugin`]
//! entry in a local registry.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors raised by marketplace operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MarketplaceError {
    #[error("marketplace fetch failed: {0}")]
    Fetch(String),
    #[error("plugin not found: {0}")]
    NotFound(String),
    #[error("plugin installation failed: {0}")]
    InstallFailed(String),
    #[error("plugin uninstall failed: {0}")]
    UninstallFailed(String),
    #[error("plugin manifest error: {0}")]
    ManifestError(String),
    #[error("plugin already installed: {0}")]
    AlreadyInstalled(String),
    #[error("integrity check failed: expected {expected}, got {actual}")]
    IntegrityCheckFailed { expected: String, actual: String },
}

/// MCP server transport kind for plugin manifests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum McpTransportKind {
    #[default]
    Stdio,
    Http,
    Sse,
}

/// Configuration for a single MCP server declared by a plugin manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub transport: McpTransportKind,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

/// Describes a marketplace plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// The index of available plugins in a marketplace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceIndex {
    #[serde(default)]
    pub plugins: Vec<PluginManifest>,
}

impl MarketplaceIndex {
    /// Fetches an index from a URL. Requires the `providers` feature.
    #[cfg(feature = "providers")]
    pub fn fetch(url: &str) -> Result<Self, MarketplaceError> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("rx4/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| MarketplaceError::Fetch(e.to_string()))?;
        let resp = client
            .get(url)
            .send()
            .map_err(|e| MarketplaceError::Fetch(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(MarketplaceError::Fetch(format!(
                "HTTP {} for {}",
                resp.status(),
                url
            )));
        }
        let body = resp
            .text()
            .map_err(|e| MarketplaceError::Fetch(e.to_string()))?;
        serde_json::from_str(&body).map_err(|e| MarketplaceError::Fetch(e.to_string()))
    }

    /// Loads an index from a local file.
    pub fn from_file(path: &Path) -> Result<Self, MarketplaceError> {
        let contents =
            std::fs::read_to_string(path).map_err(|e| MarketplaceError::Fetch(e.to_string()))?;
        serde_json::from_str(&contents).map_err(|e| MarketplaceError::Fetch(e.to_string()))
    }

    /// Returns the slice of plugins in this index.
    pub fn plugins(&self) -> &[PluginManifest] {
        &self.plugins
    }

    /// Simple keyword search across name, description, keywords, and categories.
    pub fn search(&self, query: &str) -> Vec<&PluginManifest> {
        let q = query.to_lowercase();
        if q.is_empty() {
            return self.plugins.iter().collect();
        }
        let terms: Vec<&str> = q.split_whitespace().collect();
        self.plugins
            .iter()
            .filter(|p| {
                let name = p.name.to_lowercase();
                let desc = p.description.to_lowercase();
                let keywords: Vec<String> = p.keywords.iter().map(|k| k.to_lowercase()).collect();
                let cats: Vec<String> = p.categories.iter().map(|c| c.to_lowercase()).collect();
                terms.iter().all(|term| {
                    name.contains(term)
                        || desc.contains(term)
                        || keywords.iter().any(|k| k.contains(term))
                        || cats.iter().any(|c| c.contains(term))
                })
            })
            .collect()
    }

    /// Returns plugins that declare the given category (case-insensitive).
    pub fn by_category(&self, category: &str) -> Vec<&PluginManifest> {
        let c = category.to_lowercase();
        self.plugins
            .iter()
            .filter(|p| p.categories.iter().any(|cat| cat.to_lowercase() == c))
            .collect()
    }
}

/// A plugin that has been installed on disk, with its manifest and metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub name: String,
    pub version: String,
    pub path: PathBuf,
    pub installed_at: DateTime<Utc>,
    pub manifest: PluginManifest,
}

/// Simple blocklist of plugin names that should not be installed.
#[derive(Debug, Clone, Default)]
pub struct PluginBlocklist {
    names: HashSet<String>,
}

impl PluginBlocklist {
    /// Creates an empty blocklist.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a plugin name to the blocklist.
    pub fn add(&mut self, name: &str) {
        self.names.insert(name.to_string());
    }

    /// Returns true if the given plugin name is blocked.
    pub fn is_blocked(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Loads a blocklist from a JSON array of plugin names.
    pub fn from_file(path: &Path) -> Result<Self, MarketplaceError> {
        let contents =
            std::fs::read_to_string(path).map_err(|e| MarketplaceError::Fetch(e.to_string()))?;
        let names: Vec<String> =
            serde_json::from_str(&contents).map_err(|e| MarketplaceError::Fetch(e.to_string()))?;
        let mut blocklist = Self::new();
        for name in names {
            blocklist.add(&name);
        }
        Ok(blocklist)
    }
}

/// Manages plugin installation, uninstallation, and updates.
#[derive(Debug, Clone)]
pub struct PluginInstaller {
    install_dir: PathBuf,
    blocklist: PluginBlocklist,
}

/// Accepts only `[a-zA-Z0-9._-]`. Rejects empty names, `..`, absolute paths,
/// and path separators.
pub fn sanitize_plugin_name(name: &str) -> Result<&str, MarketplaceError> {
    if name.is_empty() {
        return Err(MarketplaceError::InstallFailed(
            "plugin name is empty".to_string(),
        ));
    }
    if name == ".." || name.contains("..") {
        return Err(MarketplaceError::InstallFailed(format!(
            "invalid plugin name: {name}"
        )));
    }
    if name.starts_with('/') || name.starts_with('\\') || name.contains('/') || name.contains('\\')
    {
        return Err(MarketplaceError::InstallFailed(format!(
            "invalid plugin name: {name}"
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(MarketplaceError::InstallFailed(format!(
            "invalid plugin name: {name}"
        )));
    }
    Ok(name)
}

impl PluginInstaller {
    /// Creates a new installer targeting the given install directory.
    pub fn new(install_dir: PathBuf) -> Self {
        Self {
            install_dir,
            blocklist: PluginBlocklist::new(),
        }
    }

    /// Replaces the installer's plugin blocklist.
    pub fn with_blocklist(mut self, blocklist: PluginBlocklist) -> Self {
        self.blocklist = blocklist;
        self
    }

    /// Returns the default install directory (`~/.rx4/plugins/`).
    pub fn default_install_dir() -> PathBuf {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        home.join(".agents").join("plugins")
    }

    /// Returns the path to the installed-plugins registry JSON file.
    fn registry_path(&self) -> PathBuf {
        self.install_dir.join("installed.json")
    }

    /// Reads the registry of installed plugins from disk.
    fn read_registry(&self) -> Vec<InstalledPlugin> {
        let path = self.registry_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// Writes the registry of installed plugins to disk.
    fn write_registry(&self, plugins: &[InstalledPlugin]) -> Result<(), MarketplaceError> {
        let path = self.registry_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| MarketplaceError::InstallFailed(e.to_string()))?;
        }
        let json = serde_json::to_string_pretty(plugins)
            .map_err(|e| MarketplaceError::InstallFailed(e.to_string()))?;
        std::fs::write(&path, json).map_err(|e| MarketplaceError::InstallFailed(e.to_string()))
    }

    /// Installs a plugin from a git URL or local path.
    ///
    /// For git sources (`git+<url>` or URLs ending in `.git`), the plugin is
    /// cloned into `install_dir/{name}/`. For local paths, the directory is
    /// copied into `install_dir/{name}/`. After the source is materialized,
    /// the manifest is read from `{path}/.rx4-plugin/plugin.json` (falling
    /// back to `{path}/plugin.json`), validated, and recorded in the registry.
    pub fn install(
        &self,
        manifest: &PluginManifest,
        source: &str,
    ) -> Result<InstalledPlugin, MarketplaceError> {
        let name = sanitize_plugin_name(&manifest.name)?;
        if self.blocklist.is_blocked(name) {
            return Err(MarketplaceError::InstallFailed(format!(
                "plugin {name} is blocklisted"
            )));
        }
        if self.is_installed(name) {
            return Err(MarketplaceError::AlreadyInstalled(name.to_string()));
        }
        std::fs::create_dir_all(&self.install_dir)
            .map_err(|e| MarketplaceError::InstallFailed(e.to_string()))?;
        let target = self.install_dir.join(name);
        if target.exists() {
            std::fs::remove_dir_all(&target)
                .map_err(|e| MarketplaceError::InstallFailed(e.to_string()))?;
        }
        if source.starts_with("git+") || source.ends_with(".git") {
            let url = source.strip_prefix("git+").unwrap_or(source);
            std::process::Command::new("git")
                .arg("clone")
                .arg(url)
                .arg(&target)
                .output()
                .map_err(|e| MarketplaceError::InstallFailed(e.to_string()))
                .and_then(|out| {
                    if out.status.success() {
                        Ok(())
                    } else {
                        Err(MarketplaceError::InstallFailed(format!(
                            "git clone failed: {}",
                            String::from_utf8_lossy(&out.stderr)
                        )))
                    }
                })?;
        } else {
            let src = Path::new(source);
            if !src.exists() {
                return Err(MarketplaceError::InstallFailed(format!(
                    "source path does not exist: {source}"
                )));
            }
            copy_dir(src, &target).map_err(|e| MarketplaceError::InstallFailed(e.to_string()))?;
        }

        let on_disk_manifest = read_manifest(&target).unwrap_or_else(|_| manifest.clone());
        validate_manifest(&on_disk_manifest)?;

        // Re-check blocklist against the final on-disk identity (alias mismatch).
        let final_name = sanitize_plugin_name(&on_disk_manifest.name)?;
        if self.blocklist.is_blocked(final_name) {
            let _ = std::fs::remove_dir_all(&target);
            return Err(MarketplaceError::InstallFailed(format!(
                "plugin {final_name} (on-disk identity) is blocklisted"
            )));
        }

        if let Some(ref expected) = on_disk_manifest.sha256 {
            verify_plugin_integrity(&target, expected)?;
        } else if let Some(ref expected) = manifest.sha256 {
            verify_plugin_integrity(&target, expected)?;
        } else {
            return Err(MarketplaceError::InstallFailed(
                "sha256 required".to_string(),
            ));
        }

        if target.join("package.json").exists() {
            // npm install would run here; recorded but not executed.
        }

        let installed = InstalledPlugin {
            name: on_disk_manifest.name.clone(),
            version: on_disk_manifest.version.clone(),
            path: target.clone(),
            installed_at: Utc::now(),
            manifest: on_disk_manifest,
        };

        let mut registry = self.read_registry();
        registry.push(installed.clone());
        self.write_registry(&registry)?;
        Ok(installed)
    }

    /// Removes an installed plugin from disk and the registry.
    pub fn uninstall(&self, name: &str) -> Result<(), MarketplaceError> {
        let mut registry = self.read_registry();
        let idx = registry
            .iter()
            .position(|p| p.name == name)
            .ok_or_else(|| MarketplaceError::NotFound(name.to_string()))?;
        let removed = registry.remove(idx);
        if removed.path.exists() {
            std::fs::remove_dir_all(&removed.path)
                .map_err(|e| MarketplaceError::UninstallFailed(e.to_string()))?;
        }
        self.write_registry(&registry)
    }

    /// Lists all installed plugins.
    pub fn list_installed(&self) -> Vec<InstalledPlugin> {
        self.read_registry()
    }

    /// Returns true if a plugin with the given name is installed.
    pub fn is_installed(&self, name: &str) -> bool {
        self.read_registry().iter().any(|p| p.name == name)
    }

    /// Updates a plugin by re-installing it from its repository URL.
    pub fn update(&self, name: &str) -> Result<InstalledPlugin, MarketplaceError> {
        let registry = self.read_registry();
        let existing = registry
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| MarketplaceError::NotFound(name.to_string()))?;
        let source = existing.manifest.repository.clone().ok_or_else(|| {
            MarketplaceError::InstallFailed(format!("no repository URL for plugin {name}"))
        })?;
        let manifest = existing.manifest.clone();
        self.uninstall(name)?;
        self.install(&manifest, &source)
    }
}

impl Default for PluginInstaller {
    fn default() -> Self {
        Self::new(Self::default_install_dir())
    }
}

/// Reads a plugin manifest from `{path}/.rx4-plugin/plugin.json`, falling back
/// to `{path}/plugin.json`.
fn read_manifest(path: &Path) -> Result<PluginManifest, MarketplaceError> {
    let nested = path.join(".rx4-plugin").join("plugin.json");
    let flat = path.join("plugin.json");
    let manifest_path = if nested.exists() {
        nested
    } else if flat.exists() {
        flat
    } else {
        return Err(MarketplaceError::ManifestError(format!(
            "no plugin.json found under {}",
            path.display()
        )));
    };
    let contents = std::fs::read_to_string(&manifest_path)
        .map_err(|e| MarketplaceError::ManifestError(e.to_string()))?;
    serde_json::from_str(&contents).map_err(|e| MarketplaceError::ManifestError(e.to_string()))
}

/// Validates that a manifest has the required non-empty fields.
fn validate_manifest(manifest: &PluginManifest) -> Result<(), MarketplaceError> {
    if manifest.name.is_empty() {
        return Err(MarketplaceError::ManifestError("name is empty".to_string()));
    }
    if manifest.version.is_empty() {
        return Err(MarketplaceError::ManifestError(
            "version is empty".to_string(),
        ));
    }
    if manifest.author.is_empty() {
        return Err(MarketplaceError::ManifestError(
            "author is empty".to_string(),
        ));
    }
    Ok(())
}

/// Computes a deterministic SHA-256 digest over every file under `path`
/// (recursively, entries sorted by relative path) and compares it against
/// `expected_sha256`. Returns `Ok(())` on match or
/// [`MarketplaceError::IntegrityCheckFailed`] on mismatch.
pub fn verify_plugin_integrity(path: &Path, expected_sha256: &str) -> Result<(), MarketplaceError> {
    let actual = compute_dir_sha256(path)?;
    if actual.eq_ignore_ascii_case(expected_sha256) {
        Ok(())
    } else {
        Err(MarketplaceError::IntegrityCheckFailed {
            expected: expected_sha256.to_string(),
            actual,
        })
    }
}

/// Recursively hashes the contents of every file under `path` into a single
/// SHA-256 digest. Directory entries are visited in sorted order so the
/// result is deterministic. Subdirectories named `.git` are skipped so that
/// a cloned repository's metadata does not affect the content hash.
/// Compute a stable directory hash used for plugin integrity checks.
pub fn compute_dir_sha256(path: &Path) -> Result<String, MarketplaceError> {
    let mut hasher = Sha256::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
        .map_err(|e| MarketplaceError::InstallFailed(e.to_string()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    hash_entries(&mut hasher, path, &entries)?;
    let digest = hasher.finalize();
    Ok(format!("{:x}", digest))
}

fn hash_entries(
    hasher: &mut Sha256,
    base: &Path,
    entries: &[PathBuf],
) -> Result<(), MarketplaceError> {
    for entry in entries {
        let file_name = entry
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if entry.is_dir() {
            if file_name == ".git" {
                continue;
            }
            let mut child_entries: Vec<PathBuf> = std::fs::read_dir(entry)
                .map_err(|e| MarketplaceError::InstallFailed(e.to_string()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .collect();
            child_entries.sort();
            hash_entries(hasher, base, &child_entries)?;
        } else {
            let rel = entry
                .strip_prefix(base)
                .unwrap_or(entry)
                .to_string_lossy()
                .to_string();
            hasher.update(rel.as_bytes());
            hasher.update(b"\0");
            let data =
                std::fs::read(entry).map_err(|e| MarketplaceError::InstallFailed(e.to_string()))?;
            hasher.update(&data);
            hasher.update(b"\0");
        }
    }
    Ok(())
}

/// Recursively copies a directory tree from `src` to `dst`.
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &dest)?;
        } else {
            std::fs::copy(&path, &dest)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn sample_manifest() -> PluginManifest {
        PluginManifest {
            name: "demo-plugin".to_string(),
            version: "0.1.0".to_string(),
            description: "A demo plugin for testing.".to_string(),
            author: "tester".to_string(),
            homepage: None,
            repository: Some("https://example.com/repo.git".to_string()),
            license: Some("MIT".to_string()),
            categories: vec!["testing".to_string(), "utils".to_string()],
            keywords: vec!["demo".to_string(), "test".to_string()],
            dependencies: Vec::new(),
            mcp_servers: Vec::new(),
            skills: Vec::new(),
            hooks: Vec::new(),
            sha256: None,
        }
    }

    fn write_plugin_dir(dir: &Path, manifest: &PluginManifest) {
        fs::create_dir_all(dir).unwrap();
        let json = serde_json::to_string_pretty(manifest).unwrap();
        fs::write(dir.join("plugin.json"), json).unwrap();
    }

    /// Manifest with sha256 of an already-written source dir (hash not stored on disk).
    fn manifest_with_hash(source_dir: &Path) -> PluginManifest {
        let mut m = sample_manifest();
        m.sha256 = Some(compute_dir_sha256(source_dir).unwrap());
        m
    }

    #[test]
    fn deserializes_plugin_manifest_from_json() {
        let json = r#"{
            "name": "foo",
            "version": "1.2.3",
            "description": "does foo",
            "author": "alice",
            "homepage": "https://foo.example",
            "repository": "https://github.com/alice/foo",
            "license": "Apache-2.0",
            "categories": ["a", "b"],
            "keywords": ["foo", "bar"],
            "dependencies": ["dep1"],
            "mcp_servers": [
                {"name": "srv", "command": "node", "args": ["s.js"], "env": {"X": "1"}}
            ],
            "skills": ["s1"],
            "hooks": ["h1"]
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.name, "foo");
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.author, "alice");
        assert_eq!(m.categories, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(m.keywords, vec!["foo".to_string(), "bar".to_string()]);
        assert_eq!(m.mcp_servers.len(), 1);
        assert_eq!(m.mcp_servers[0].command, "node");
        assert_eq!(m.mcp_servers[0].env.get("X").unwrap(), "1");
        assert_eq!(m.mcp_servers[0].transport, McpTransportKind::Stdio);
        assert!(m.mcp_servers[0].url.is_none());
    }

    #[test]
    fn deserializes_remote_mcp_server_config() {
        let json = r#"{
            "name": "remote",
            "transport": "http",
            "url": "https://example.com/mcp",
            "headers": {"Authorization": "Bearer t"}
        }"#;
        let cfg: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.name, "remote");
        assert_eq!(cfg.command, "");
        assert_eq!(cfg.transport, McpTransportKind::Http);
        assert_eq!(cfg.url.as_deref(), Some("https://example.com/mcp"));
        assert_eq!(cfg.headers.get("Authorization").unwrap(), "Bearer t");
    }

    #[test]
    fn deserializes_sse_transport_kind() {
        let json = r#"{"name":"s","transport":"sse","url":"https://x/sse"}"#;
        let cfg: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.transport, McpTransportKind::Sse);
    }

    #[test]
    fn marketplace_search_matches_keywords() {
        let index = MarketplaceIndex {
            plugins: vec![
                sample_manifest(),
                PluginManifest {
                    name: "other".to_string(),
                    version: "0.0.1".to_string(),
                    description: "unrelated".to_string(),
                    author: "bob".to_string(),
                    categories: vec!["misc".to_string()],
                    keywords: vec!["other".to_string()],
                    ..sample_manifest()
                },
            ],
        };
        let results = index.search("demo");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "demo-plugin");
    }

    #[test]
    fn marketplace_by_category_filters() {
        let index = MarketplaceIndex {
            plugins: vec![sample_manifest()],
        };
        let results = index.by_category("Testing");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "demo-plugin");
        assert!(index.by_category("nonexistent").is_empty());
    }

    #[test]
    fn installs_plugin_from_local_path() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("plugins");
        let source_dir = tmp.path().join("source");
        write_plugin_dir(&source_dir, &sample_manifest());
        let manifest = manifest_with_hash(&source_dir);

        let installer = PluginInstaller::new(install_dir.clone());
        let installed = installer
            .install(&manifest, source_dir.to_str().unwrap())
            .unwrap();
        assert_eq!(installed.name, "demo-plugin");
        assert_eq!(installed.version, "0.1.0");
        assert!(installed.path.join("plugin.json").exists());
        assert!(installer.is_installed("demo-plugin"));
    }

    #[test]
    fn install_rejects_already_installed() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("source");
        write_plugin_dir(&source_dir, &sample_manifest());
        let manifest = manifest_with_hash(&source_dir);
        let installer = PluginInstaller::new(tmp.path().join("plugins"));
        installer
            .install(&manifest, source_dir.to_str().unwrap())
            .unwrap();
        let err = installer
            .install(&manifest, source_dir.to_str().unwrap())
            .unwrap_err();
        assert!(matches!(err, MarketplaceError::AlreadyInstalled(_)));
    }

    #[test]
    fn uninstalls_plugin() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("source");
        write_plugin_dir(&source_dir, &sample_manifest());
        let manifest = manifest_with_hash(&source_dir);
        let installer = PluginInstaller::new(tmp.path().join("plugins"));
        installer
            .install(&manifest, source_dir.to_str().unwrap())
            .unwrap();
        assert!(installer.is_installed("demo-plugin"));
        installer.uninstall("demo-plugin").unwrap();
        assert!(!installer.is_installed("demo-plugin"));
    }

    #[test]
    fn uninstall_errors_on_unknown_plugin() {
        let tmp = TempDir::new().unwrap();
        let installer = PluginInstaller::new(tmp.path().join("plugins"));
        let err = installer.uninstall("nope").unwrap_err();
        assert!(matches!(err, MarketplaceError::NotFound(_)));
    }

    #[test]
    fn list_installed_returns_plugins() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("source");
        write_plugin_dir(&source_dir, &sample_manifest());
        let manifest = manifest_with_hash(&source_dir);
        let installer = PluginInstaller::new(tmp.path().join("plugins"));
        installer
            .install(&manifest, source_dir.to_str().unwrap())
            .unwrap();
        let list = installer.list_installed();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "demo-plugin");
    }

    #[test]
    fn is_installed_returns_false_when_empty() {
        let tmp = TempDir::new().unwrap();
        let installer = PluginInstaller::new(tmp.path().join("plugins"));
        assert!(!installer.is_installed("anything"));
    }

    #[test]
    fn blocklist_blocks_added_names() {
        let mut bl = PluginBlocklist::new();
        assert!(!bl.is_blocked("evil"));
        bl.add("evil");
        assert!(bl.is_blocked("evil"));
        assert!(!bl.is_blocked("good"));
    }

    #[test]
    fn blocklist_loads_from_json_array() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("blocklist.json");
        fs::write(&path, r#"["a","b","c"]"#).unwrap();
        let bl = PluginBlocklist::from_file(&path).unwrap();
        assert!(bl.is_blocked("a"));
        assert!(bl.is_blocked("b"));
        assert!(bl.is_blocked("c"));
        assert!(!bl.is_blocked("d"));
    }

    #[test]
    fn install_errors_on_missing_source() {
        let tmp = TempDir::new().unwrap();
        let installer = PluginInstaller::new(tmp.path().join("plugins"));
        let mut manifest = sample_manifest();
        manifest.sha256 = Some("deadbeef".to_string());
        let err = installer
            .install(&manifest, tmp.path().join("missing").to_str().unwrap())
            .unwrap_err();
        assert!(matches!(err, MarketplaceError::InstallFailed(_)));
    }

    #[test]
    fn install_requires_sha256() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("source");
        write_plugin_dir(&source_dir, &sample_manifest());
        let installer = PluginInstaller::new(tmp.path().join("plugins"));
        let err = installer
            .install(&sample_manifest(), source_dir.to_str().unwrap())
            .unwrap_err();
        assert!(matches!(err, MarketplaceError::InstallFailed(msg) if msg.contains("sha256")));
    }

    #[test]
    fn install_rejects_blocklisted_name() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("source");
        write_plugin_dir(&source_dir, &sample_manifest());
        let manifest = manifest_with_hash(&source_dir);
        let mut bl = PluginBlocklist::new();
        bl.add("demo-plugin");
        let installer = PluginInstaller::new(tmp.path().join("plugins")).with_blocklist(bl);
        let err = installer
            .install(&manifest, source_dir.to_str().unwrap())
            .unwrap_err();
        assert!(matches!(err, MarketplaceError::InstallFailed(msg) if msg.contains("blocklisted")));
    }

    #[test]
    fn sanitize_plugin_name_rejects_path_chars() {
        assert!(sanitize_plugin_name("demo-plugin").is_ok());
        assert!(sanitize_plugin_name("").is_err());
        assert!(sanitize_plugin_name("..").is_err());
        assert!(sanitize_plugin_name("a/b").is_err());
        assert!(sanitize_plugin_name("/abs").is_err());
        assert!(sanitize_plugin_name("bad name").is_err());
    }

    #[test]
    fn marketplace_index_loads_from_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("index.json");
        let index = MarketplaceIndex {
            plugins: vec![sample_manifest()],
        };
        fs::write(&path, serde_json::to_string(&index).unwrap()).unwrap();
        let loaded = MarketplaceIndex::from_file(&path).unwrap();
        assert_eq!(loaded.plugins().len(), 1);
        assert_eq!(loaded.plugins()[0].name, "demo-plugin");
    }

    #[test]
    fn test_verify_integrity_success() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("source");
        write_plugin_dir(&source_dir, &sample_manifest());
        let expected = compute_dir_sha256(&source_dir).unwrap();
        assert!(verify_plugin_integrity(&source_dir, &expected).is_ok());
    }

    #[test]
    fn test_verify_integrity_failure() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("source");
        write_plugin_dir(&source_dir, &sample_manifest());
        let wrong_hash =
            "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        let err = verify_plugin_integrity(&source_dir, &wrong_hash).unwrap_err();
        match err {
            MarketplaceError::IntegrityCheckFailed { expected, actual } => {
                assert_eq!(expected, wrong_hash);
                assert_ne!(actual, wrong_hash);
            }
            other => panic!("expected IntegrityCheckFailed, got {other:?}"),
        }
    }

    #[test]
    fn test_install_aborts_on_mismatch() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("plugins");
        let source_dir = tmp.path().join("source");
        let mut manifest = sample_manifest();
        manifest.sha256 =
            Some("0000000000000000000000000000000000000000000000000000000000000000".to_string());
        write_plugin_dir(&source_dir, &manifest);

        let installer = PluginInstaller::new(install_dir.clone());
        let err = installer
            .install(&manifest, source_dir.to_str().unwrap())
            .unwrap_err();
        match err {
            MarketplaceError::IntegrityCheckFailed { expected, .. } => {
                assert!(expected.starts_with("0000"));
            }
            other => panic!("expected IntegrityCheckFailed, got {other:?}"),
        }
        assert!(!installer.is_installed("demo-plugin"));
    }
}
