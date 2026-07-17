//! Plugin registry: host extension points loaded from marketplace installs.
//!
//! Plugins can declare skills, hooks, and MCP servers via
//! [`crate::marketplace::PluginManifest`]. The registry tracks installed
//! plugins, loads them from disk, and exposes their declared capabilities.

use crate::marketplace::{InstalledPlugin, PluginInstaller, PluginManifest};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A loaded plugin entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub path: PathBuf,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
}

impl Plugin {
    /// Build from a marketplace installed record.
    pub fn from_installed(installed: &InstalledPlugin) -> Self {
        Self {
            id: installed.name.clone(),
            name: installed.name.clone(),
            version: installed.version.clone(),
            path: installed.path.clone(),
            skills: installed.manifest.skills.clone(),
            hooks: installed.manifest.hooks.clone(),
            mcp_servers: installed
                .manifest
                .mcp_servers
                .iter()
                .map(|m| m.name.clone())
                .collect(),
            enabled: true,
        }
    }

    /// Build from a raw manifest + install path.
    pub fn from_manifest(manifest: &PluginManifest, path: PathBuf) -> Self {
        Self {
            id: manifest.name.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            path,
            skills: manifest.skills.clone(),
            hooks: manifest.hooks.clone(),
            mcp_servers: manifest
                .mcp_servers
                .iter()
                .map(|m| m.name.clone())
                .collect(),
            enabled: true,
        }
    }
}

/// Registry of loaded plugins.
#[derive(Debug, Default, Clone)]
pub struct PluginRegistry {
    pub plugins: Vec<Plugin>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    /// Add or replace a plugin by id.
    pub fn add(&mut self, plugin: Plugin) {
        if let Some(existing) = self.plugins.iter_mut().find(|p| p.id == plugin.id) {
            *existing = plugin;
        } else {
            self.plugins.push(plugin);
        }
    }

    pub fn count(&self) -> usize {
        self.plugins.len()
    }

    pub fn get(&self, id: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.id == id)
    }

    pub fn enabled(&self) -> impl Iterator<Item = &Plugin> {
        self.plugins.iter().filter(|p| p.enabled)
    }

    /// Enable or disable a plugin.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> bool {
        if let Some(p) = self.plugins.iter_mut().find(|p| p.id == id) {
            p.enabled = enabled;
            true
        } else {
            false
        }
    }

    /// Remove a plugin from the registry (does not uninstall from disk).
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.plugins.len();
        self.plugins.retain(|p| p.id != id);
        self.plugins.len() != before
    }

    /// Load all plugins recorded by a [`PluginInstaller`] registry file.
    pub fn load_from_installer(&mut self, installer: &PluginInstaller) -> usize {
        let mut n = 0;
        for installed in installer.list_installed() {
            self.add(Plugin::from_installed(&installed));
            n += 1;
        }
        n
    }

    /// Load a single plugin directory that contains `plugin.json` or
    /// `.rx4-plugin/plugin.json`.
    pub fn load_dir(&mut self, dir: &Path) -> Result<Plugin, String> {
        let manifest_path = if dir.join(".rx4-plugin/plugin.json").is_file() {
            dir.join(".rx4-plugin/plugin.json")
        } else if dir.join("plugin.json").is_file() {
            dir.join("plugin.json")
        } else {
            return Err(format!("no plugin.json under {}", dir.display()));
        };
        let text = std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
        let manifest: PluginManifest =
            serde_json::from_str(&text).map_err(|e| format!("manifest parse: {e}"))?;
        let plugin = Plugin::from_manifest(&manifest, dir.to_path_buf());
        self.add(plugin.clone());
        Ok(plugin)
    }

    /// All skill paths declared by enabled plugins (relative to plugin root).
    pub fn skill_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for p in self.enabled() {
            for skill in &p.skills {
                out.push(p.path.join(skill));
            }
        }
        out
    }

    /// All hook identifiers declared by enabled plugins.
    pub fn hook_ids(&self) -> Vec<String> {
        self.enabled()
            .flat_map(|p| p.hooks.iter().cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::{compute_dir_sha256, PluginInstaller, PluginManifest};
    use std::fs;

    #[test]
    fn add_get_enable() {
        let mut reg = PluginRegistry::new();
        reg.add(Plugin {
            id: "demo".into(),
            name: "demo".into(),
            version: "0.1.0".into(),
            path: PathBuf::from("/tmp/demo"),
            skills: vec!["SKILL.md".into()],
            hooks: vec!["on_start".into()],
            mcp_servers: vec![],
            enabled: true,
        });
        assert_eq!(reg.count(), 1);
        assert!(reg.get("demo").is_some());
        assert_eq!(reg.skill_paths().len(), 1);
        assert!(reg.set_enabled("demo", false));
        assert_eq!(reg.skill_paths().len(), 0);
    }

    #[test]
    fn load_dir_reads_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("myplug");
        fs::create_dir_all(dir.join(".rx4-plugin")).unwrap();
        let manifest = PluginManifest {
            name: "myplug".into(),
            version: "1.0.0".into(),
            description: "d".into(),
            author: "a".into(),
            homepage: None,
            repository: None,
            license: None,
            categories: vec![],
            keywords: vec![],
            dependencies: vec![],
            mcp_servers: vec![],
            skills: vec!["skills/a.md".into()],
            hooks: vec!["h1".into()],
            sha256: None,
        };
        fs::write(
            dir.join(".rx4-plugin/plugin.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        let mut reg = PluginRegistry::new();
        let p = reg.load_dir(&dir).expect("load");
        assert_eq!(p.name, "myplug");
        assert_eq!(p.skills, vec!["skills/a.md"]);
        assert_eq!(reg.count(), 1);
    }

    #[test]
    fn load_from_installer() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("srcplug");
        fs::create_dir_all(source.join(".rx4-plugin")).unwrap();
        // Write on-disk plugin without sha256 field; pass expected hash via install arg.
        let on_disk = PluginManifest {
            name: "srcplug".into(),
            version: "0.2.0".into(),
            description: "d".into(),
            author: "a".into(),
            homepage: None,
            repository: None,
            license: None,
            categories: vec![],
            keywords: vec![],
            dependencies: vec![],
            mcp_servers: vec![],
            skills: vec![],
            hooks: vec![],
            sha256: None,
        };
        fs::write(
            source.join(".rx4-plugin/plugin.json"),
            serde_json::to_string(&on_disk).unwrap(),
        )
        .unwrap();
        let hash = compute_dir_sha256(&source).unwrap();
        let mut install_manifest = on_disk;
        install_manifest.sha256 = Some(hash);

        let install_dir = tmp.path().join("plugins");
        let installer = PluginInstaller::new(install_dir);
        installer
            .install(&install_manifest, source.to_str().unwrap())
            .unwrap();

        let mut reg = PluginRegistry::new();
        assert_eq!(reg.load_from_installer(&installer), 1);
        assert!(reg.get("srcplug").is_some());
    }
}
