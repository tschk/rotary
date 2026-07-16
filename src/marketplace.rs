//! Marketplace: plugin discovery, installation, and lifecycle management.
//!
//! A marketplace is a JSON index of plugin manifests. Each manifest describes
//! a plugin's metadata, MCP servers, skills, hooks, and dependencies. The
//! [`PluginInstaller`] clones plugins from git URLs (or copies from local
//! paths) into `~/.rx4/plugins/{name}/` and records an [`InstalledPlugin`]
//! entry in a local registry.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
}

/// Configuration for a single MCP server declared by a plugin manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
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
}

impl PluginInstaller {
    /// Creates a new installer targeting the given install directory.
    pub fn new(install_dir: PathBuf) -> Self {
        Self { install_dir }
    }

    /// Returns the default install directory (`~/.rx4/plugins/`).
    pub fn default_install_dir() -> PathBuf {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        home.join(".rx4").join("plugins")
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
        if self.is_installed(&manifest.name) {
            return Err(MarketplaceError::AlreadyInstalled(manifest.name.clone()));
        }
        std::fs::create_dir_all(&self.install_dir)
            .map_err(|e| MarketplaceError::InstallFailed(e.to_string()))?;
        let target = self.install_dir.join(&manifest.name);
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
        }
    }

    fn write_plugin_dir(dir: &Path, manifest: &PluginManifest) {
        fs::create_dir_all(dir).unwrap();
        let json = serde_json::to_string_pretty(manifest).unwrap();
        fs::write(dir.join("plugin.json"), json).unwrap();
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

        let installer = PluginInstaller::new(install_dir.clone());
        let installed = installer
            .install(&sample_manifest(), source_dir.to_str().unwrap())
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
        let installer = PluginInstaller::new(tmp.path().join("plugins"));
        installer
            .install(&sample_manifest(), source_dir.to_str().unwrap())
            .unwrap();
        let err = installer
            .install(&sample_manifest(), source_dir.to_str().unwrap())
            .unwrap_err();
        assert!(matches!(err, MarketplaceError::AlreadyInstalled(_)));
    }

    #[test]
    fn uninstalls_plugin() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("source");
        write_plugin_dir(&source_dir, &sample_manifest());
        let installer = PluginInstaller::new(tmp.path().join("plugins"));
        installer
            .install(&sample_manifest(), source_dir.to_str().unwrap())
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
        let installer = PluginInstaller::new(tmp.path().join("plugins"));
        installer
            .install(&sample_manifest(), source_dir.to_str().unwrap())
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
        let err = installer
            .install(
                &sample_manifest(),
                tmp.path().join("missing").to_str().unwrap(),
            )
            .unwrap_err();
        assert!(matches!(err, MarketplaceError::InstallFailed(_)));
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
}
