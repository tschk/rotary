//! Plugin registry: pi-compatible extensions (stub for now).

pub struct PluginRegistry {
    pub plugins: Vec<Plugin>,
}

pub struct Plugin {
    pub id: String,
    pub name: String,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }
    pub fn add(&mut self, plugin: Plugin) {
        self.plugins.push(plugin);
    }
    pub fn count(&self) -> usize {
        self.plugins.len()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}
