//! Model registry: built-in model metadata, user overrides, capability detection,
//! and per-provider compatibility configuration.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

/// Metadata describing a single model's capabilities and limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default)]
    pub supports_reasoning: bool,
}

/// Per-provider compatibility overrides for request field naming and role handling.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompatConfig {
    /// Field name to use for the maximum output tokens parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<String>,
    /// Whether the provider accepts a top-level `system` role message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_role: Option<SystemRoleHandling>,
    /// Whether the provider supports native tool-call function definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_field: Option<String>,
}

/// How a provider expects system instructions to be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemRoleHandling {
    /// Top-level `system` role message (OpenAI-compatible).
    TopLevel,
    /// Prepended into the first user message (Anthropic-style).
    PrependUser,
    /// Dedicated `system` parameter outside the messages array.
    SystemParam,
}

/// Global model registry with built-in defaults merged against user overrides.
#[derive(Debug, Clone)]
pub struct ModelRegistry {
    models: HashMap<String, ModelInfo>,
    compat: HashMap<String, CompatConfig>,
}

impl ModelRegistry {
    /// Build a registry from the built-in model list, optionally merged with
    /// `~/.agents/models.json` and `./models.json` (user overrides take precedence).
    pub fn load() -> Self {
        let mut models = HashMap::new();
        for model in builtin_models() {
            models.insert(model.id.clone(), model);
        }

        let mut compat = HashMap::new();
        for (provider, cfg) in builtin_compat() {
            compat.insert(provider.to_string(), cfg);
        }

        if let Some(overrides) = load_user_overrides() {
            for model in overrides.models {
                models.insert(model.id.clone(), model);
            }
            for (provider, cfg) in overrides.compat {
                compat.insert(provider, cfg);
            }
        }

        Self { models, compat }
    }

    /// Look up a model by id (e.g. `gpt-4o`, `claude-3-5-sonnet`).
    pub fn get(&self, id: &str) -> Option<&ModelInfo> {
        self.models.get(id)
    }

    /// Iterate over all registered models.
    pub fn models(&self) -> impl Iterator<Item = &ModelInfo> {
        self.models.values()
    }

    /// Look up compatibility configuration for a provider id.
    pub fn compat(&self, provider: &str) -> Option<&CompatConfig> {
        self.compat.get(provider)
    }

    /// True for models that support high-effort reasoning (o1, o3, thinking Claude).
    pub fn supports_xhigh(&self, id: &str) -> bool {
        match self.models.get(id) {
            Some(m) if m.supports_reasoning => is_xhigh_model(&m.id),
            _ => false,
        }
    }

    /// Predicate for any reasoning-capable model.
    pub fn is_reasoning_model(&self, id: &str) -> bool {
        self.models
            .get(id)
            .map(|m| m.supports_reasoning)
            .unwrap_or(false)
    }

    /// Clamp a requested thinking level to the model's supported range.
    ///
    /// Returns one of `"low"`, `"medium"`, `"high"`, `"xhigh"`. Non-reasoning
    /// models clamp to `"low"`; reasoning models without xhigh support clamp to
    /// `"high"`.
    pub fn thinking_level_clamp(&self, id: &str, requested: &str) -> String {
        if !self.is_reasoning_model(id) {
            return "low".into();
        }
        let xhigh = self.supports_xhigh(id);
        match requested {
            "xhigh" if xhigh => "xhigh".into(),
            "xhigh" => "high".into(),
            "high" => "high".into(),
            "medium" => "medium".into(),
            _ => "low".into(),
        }
    }
}

fn is_xhigh_model(id: &str) -> bool {
    if id.starts_with("gpt-5.6-sol")
        || id.starts_with("gpt-5.5")
        || id.starts_with("gpt-5.4")
        || id.starts_with("gpt-5.2")
    {
        return true;
    }
    if id.starts_with("o1") && !id.contains("mini") {
        return true;
    }
    if id.starts_with("o3") {
        return true;
    }
    id == "claude-3-5-sonnet" || id == "claude-3-7-sonnet" || id == "claude-sonnet-4"
}

fn builtin_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "gpt-5.6-sol".into(),
            provider: "openai".into(),
            context_window: 200_000,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: true,
        },
        ModelInfo {
            id: "gpt-5.6-terra".into(),
            provider: "openai".into(),
            context_window: 200_000,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: true,
        },
        ModelInfo {
            id: "gpt-5.6-luna".into(),
            provider: "openai".into(),
            context_window: 200_000,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: false,
        },
        ModelInfo {
            id: "gpt-5.4".into(),
            provider: "openai".into(),
            context_window: 200_000,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: true,
        },
        ModelInfo {
            id: "gpt-5.4-mini".into(),
            provider: "openai".into(),
            context_window: 200_000,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: false,
        },
        ModelInfo {
            id: "gpt-5.2".into(),
            provider: "openai".into(),
            context_window: 200_000,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: true,
        },
        ModelInfo {
            id: "claude-3-5-sonnet".into(),
            provider: "anthropic".into(),
            context_window: 200_000,
            max_output_tokens: 8_192,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: true,
        },
        ModelInfo {
            id: "claude-3-5-haiku".into(),
            provider: "anthropic".into(),
            context_window: 200_000,
            max_output_tokens: 8_192,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: false,
        },
        ModelInfo {
            id: "claude-3-opus".into(),
            provider: "anthropic".into(),
            context_window: 200_000,
            max_output_tokens: 4_096,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: false,
        },
        ModelInfo {
            id: "gemini-2.0-flash".into(),
            provider: "google".into(),
            context_window: 1_048_576,
            max_output_tokens: 8_192,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: false,
        },
        ModelInfo {
            id: "gemini-1.5-pro".into(),
            provider: "google".into(),
            context_window: 2_097_152,
            max_output_tokens: 8_192,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: false,
        },
        ModelInfo {
            id: "grok-3".into(),
            provider: "xai".into(),
            context_window: 131_072,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: false,
            supports_reasoning: false,
        },
        ModelInfo {
            id: "grok-3-mini".into(),
            provider: "xai".into(),
            context_window: 131_072,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: false,
            supports_reasoning: true,
        },
        ModelInfo {
            id: "grok-4.5".into(),
            provider: "xai".into(),
            context_window: 256_000,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: true,
        },
        ModelInfo {
            id: "grok-4.3".into(),
            provider: "xai".into(),
            context_window: 256_000,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: true,
        },
        ModelInfo {
            id: "grok-build-0.1".into(),
            provider: "xai".into(),
            context_window: 256_000,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: true,
        },
        ModelInfo {
            id: "grok-4.20-0309-reasoning".into(),
            provider: "xai".into(),
            context_window: 256_000,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: true,
        },
        ModelInfo {
            id: "grok-4.20-0309-non-reasoning".into(),
            provider: "xai".into(),
            context_window: 256_000,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: false,
        },
        ModelInfo {
            id: "grok-4.20-multi-agent-0309".into(),
            provider: "xai".into(),
            context_window: 256_000,
            max_output_tokens: 16_384,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: true,
        },
        ModelInfo {
            id: "llama3.2".into(),
            provider: "ollama".into(),
            context_window: 128_000,
            max_output_tokens: 4_096,
            supports_tools: true,
            supports_vision: false,
            supports_reasoning: false,
        },
        ModelInfo {
            id: "qwen2.5".into(),
            provider: "ollama".into(),
            context_window: 131_072,
            max_output_tokens: 4_096,
            supports_tools: true,
            supports_vision: false,
            supports_reasoning: false,
        },
        ModelInfo {
            id: "deepseek-r1".into(),
            provider: "ollama".into(),
            context_window: 128_000,
            max_output_tokens: 8_192,
            supports_tools: false,
            supports_vision: false,
            supports_reasoning: true,
        },
    ]
}

fn builtin_compat() -> Vec<(&'static str, CompatConfig)> {
    vec![
        (
            "openai",
            CompatConfig {
                max_tokens_field: Some("max_completion_tokens".into()),
                system_role: Some(SystemRoleHandling::TopLevel),
                tools_field: Some("tools".into()),
            },
        ),
        (
            "anthropic",
            CompatConfig {
                max_tokens_field: Some("max_tokens".into()),
                system_role: Some(SystemRoleHandling::SystemParam),
                tools_field: Some("tools".into()),
            },
        ),
        (
            "google",
            CompatConfig {
                max_tokens_field: Some("max_output_tokens".into()),
                system_role: Some(SystemRoleHandling::TopLevel),
                tools_field: Some("tools".into()),
            },
        ),
        (
            "xai",
            CompatConfig {
                max_tokens_field: Some("max_completion_tokens".into()),
                system_role: Some(SystemRoleHandling::TopLevel),
                tools_field: Some("tools".into()),
            },
        ),
        (
            "ollama",
            CompatConfig {
                max_tokens_field: Some("max_tokens".into()),
                system_role: Some(SystemRoleHandling::TopLevel),
                tools_field: Some("tools".into()),
            },
        ),
    ]
}

#[derive(Debug, Deserialize)]
struct OverridesFile {
    #[serde(default)]
    models: Vec<ModelInfo>,
    #[serde(default)]
    compat: HashMap<String, CompatConfig>,
}

fn load_user_overrides() -> Option<OverridesFile> {
    let home = std::env::var("HOME").ok();
    let candidates: Vec<std::path::PathBuf> =
        std::iter::once(std::path::PathBuf::from("./models.json"))
            .chain(home.map(|h| std::path::Path::new(&h).join(".agents").join("models.json")))
            .collect();

    for path in candidates {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(file) = serde_json::from_str::<OverridesFile>(&content) {
                return Some(file);
            }
        }
    }
    None
}

static REGISTRY: OnceLock<ModelRegistry> = OnceLock::new();

/// Access the process-wide registry, initializing it on first use.
pub fn registry() -> &'static ModelRegistry {
    REGISTRY.get_or_init(ModelRegistry::load)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> ModelRegistry {
        ModelRegistry::load()
    }

    #[test]
    fn builtin_model_lookup() {
        let reg = fresh();
        let gpt = reg
            .get("gpt-5.6-sol")
            .expect("gpt-5.6-sol should be registered");
        assert_eq!(gpt.provider, "openai");
        assert_eq!(gpt.context_window, 200_000);
        assert!(gpt.supports_tools);
        assert!(gpt.supports_vision);
        assert!(gpt.supports_reasoning);

        let sonnet = reg
            .get("claude-3-5-sonnet")
            .expect("claude-3-5-sonnet should be registered");
        assert_eq!(sonnet.provider, "anthropic");
        assert!(sonnet.supports_reasoning);
    }

    #[test]
    fn unknown_model_returns_none() {
        let reg = fresh();
        assert!(reg.get("does-not-exist-xyz").is_none());
    }

    #[test]
    fn reasoning_model_detection() {
        let reg = fresh();
        assert!(reg.is_reasoning_model("gpt-5.6-sol"));
        assert!(reg.is_reasoning_model("gpt-5.4"));
        assert!(reg.is_reasoning_model("claude-3-5-sonnet"));
        assert!(!reg.is_reasoning_model("gpt-5.6-luna"));
        assert!(!reg.is_reasoning_model("gpt-5.4-mini"));
        assert!(!reg.is_reasoning_model("claude-3-5-haiku"));
    }

    #[test]
    fn xhigh_support() {
        let reg = fresh();
        assert!(reg.supports_xhigh("gpt-5.6-sol"));
        assert!(reg.supports_xhigh("gpt-5.4"));
        assert!(reg.supports_xhigh("claude-3-5-sonnet"));
        assert!(!reg.supports_xhigh("gpt-5.6-luna"));
        assert!(!reg.supports_xhigh("gpt-5.4-mini"));
    }

    #[test]
    fn thinking_level_clamping() {
        let reg = fresh();
        assert_eq!(reg.thinking_level_clamp("gpt-5.6-luna", "xhigh"), "low");
        assert_eq!(reg.thinking_level_clamp("gpt-5.4", "xhigh"), "xhigh");
        assert_eq!(reg.thinking_level_clamp("gpt-5.6-sol", "xhigh"), "xhigh");
        assert_eq!(reg.thinking_level_clamp("gpt-5.6-sol", "low"), "low");
        assert_eq!(reg.thinking_level_clamp("gpt-5.4", "medium"), "medium");
        assert_eq!(
            reg.thinking_level_clamp("claude-3-5-sonnet", "xhigh"),
            "xhigh"
        );
    }

    #[test]
    fn compat_config_lookup() {
        let reg = fresh();
        let anthropic = reg
            .compat("anthropic")
            .expect("anthropic compat should be registered");
        assert_eq!(anthropic.max_tokens_field.as_deref(), Some("max_tokens"));
        assert_eq!(anthropic.system_role, Some(SystemRoleHandling::SystemParam));

        let openai = reg
            .compat("openai")
            .expect("openai compat should be registered");
        assert_eq!(
            openai.max_tokens_field.as_deref(),
            Some("max_completion_tokens")
        );
        assert_eq!(openai.system_role, Some(SystemRoleHandling::TopLevel));

        assert!(reg.compat("unknown-provider").is_none());
    }

    #[test]
    fn registry_is_initialized_once() {
        let a = registry();
        let b = registry();
        assert!(std::ptr::eq(a, b));
    }
}
