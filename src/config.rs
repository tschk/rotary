//! Configuration: load config from file + env.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default = "default_max_iterations")]
    pub max_tool_iterations: usize,
    #[serde(default = "default_compact_after")]
    pub auto_compact_after: usize,
}

fn default_model() -> String {
    "gpt-4o".into()
}
fn default_scope() -> String {
    "coding".into()
}
fn default_max_iterations() -> usize {
    20
}
fn default_compact_after() -> usize {
    80
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: default_model(),
            scope: default_scope(),
            provider: None,
            api_key_env: None,
            max_tool_iterations: default_max_iterations(),
            auto_compact_after: default_compact_after(),
        }
    }
}

impl Config {
    pub fn load(path: &std::path::Path) -> Self {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(config) = serde_json::from_str(&content) {
                return config;
            }
        }
        Self::default()
    }
}
