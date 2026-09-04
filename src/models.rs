//! Consumer-supplied model metadata and provider compatibility.
//!
//! Rotary owns the registry data structure and lookup rules, but it does not
//! own a catalog of current models. Hosts should populate a [`ModelRegistry`]
//! from their provider SDK, discovery endpoint, or configuration and pass it
//! to [`crate::Agent::set_model_registry`].

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Metadata describing a model selected by the host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelInfo {
    /// Provider-local model identifier, for example `gpt-4o` or
    /// `anthropic/claude-sonnet-4`.
    pub id: String,
    /// Provider identifier used by [`crate::provider::Provider::id`].
    pub provider: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default)]
    pub supports_reasoning: bool,
    #[serde(default)]
    pub supports_reasoning_effort: bool,
}

impl ModelInfo {
    /// Construct metadata with conservative capability defaults.
    pub fn new(
        provider: impl Into<String>,
        id: impl Into<String>,
        context_window: usize,
        max_output_tokens: usize,
    ) -> Self {
        Self {
            id: id.into(),
            provider: provider.into(),
            context_window,
            max_output_tokens,
            supports_tools: false,
            supports_vision: false,
            supports_reasoning: false,
            supports_reasoning_effort: false,
        }
    }
}

/// Per-provider compatibility overrides for request field naming and role handling.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
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
    TopLevel,
    PrependUser,
    SystemParam,
}

/// Model metadata supplied by a host or consumer.
///
/// The registry starts empty. It never silently adds Rotary's own model list,
/// reads a global singleton, or overrides a host's selected provider. A model
/// is addressed by `(provider, id)` internally; [`Self::get`] is retained as a
/// convenience for IDs that are unique across the supplied catalog.
#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    models: HashMap<String, ModelInfo>,
    compat: HashMap<String, CompatConfig>,
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry from host-supplied model metadata.
    pub fn from_models<I>(models: I) -> Self
    where
        I: IntoIterator<Item = ModelInfo>,
    {
        let mut registry = Self::new();
        registry.extend(models);
        registry
    }

    /// Add or replace metadata for one provider/model pair.
    pub fn register(&mut self, model: ModelInfo) -> Option<ModelInfo> {
        self.models
            .insert(model_key(&model.provider, &model.id), model)
    }

    pub fn extend<I>(&mut self, models: I)
    where
        I: IntoIterator<Item = ModelInfo>,
    {
        for model in models {
            self.register(model);
        }
    }

    pub fn register_compat(
        &mut self,
        provider: impl Into<String>,
        config: CompatConfig,
    ) -> Option<CompatConfig> {
        self.compat.insert(provider.into(), config)
    }

    /// Parse a consumer-owned JSON registry. The file format is an object with
    /// `models` and optional `compat` fields; file location and refresh policy
    /// remain entirely the host's responsibility.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let file = serde_json::from_str::<OverridesFile>(json)?;
        let mut registry = ModelRegistry::from_models(file.models);
        registry.compat = file.compat;
        Ok(registry)
    }

    /// Look up a model by a fully qualified `provider/model` key or by a
    /// unique provider-local model ID.
    pub fn get(&self, id: &str) -> Option<&ModelInfo> {
        if let Some(model) = self.models.get(id) {
            return Some(model);
        }

        let mut matches = self.models.values().filter(|model| model.id == id);
        let first = matches.next()?;
        if matches.next().is_none() {
            Some(first)
        } else {
            None
        }
    }

    /// Look up a model using the provider identity selected by the host.
    pub fn get_for_provider(&self, provider: &str, id: &str) -> Option<&ModelInfo> {
        self.models.get(&model_key(provider, id))
    }

    pub fn models(&self) -> impl Iterator<Item = &ModelInfo> {
        self.models.values()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    pub fn compat(&self, provider: &str) -> Option<&CompatConfig> {
        self.compat.get(provider)
    }

    pub fn supports_xhigh(&self, id: &str) -> bool {
        self.get(id)
            .is_some_and(|model| model.supports_reasoning && is_xhigh_model(&model.id))
    }

    pub fn is_reasoning_model(&self, id: &str) -> bool {
        self.get(id).is_some_and(|model| model.supports_reasoning)
    }

    pub fn supports_reasoning_effort(&self, id: &str) -> bool {
        self.get(id)
            .is_some_and(|model| model.supports_reasoning_effort)
    }

    pub fn supports_reasoning_effort_for(&self, provider: &str, id: &str) -> bool {
        self.get_for_provider(provider, id)
            .or_else(|| self.get(id))
            .is_some_and(|model| model.supports_reasoning_effort)
    }

    pub fn thinking_level_clamp(&self, id: &str, requested: &str) -> String {
        if !self.is_reasoning_model(id) {
            return "low".into();
        }
        match requested {
            "xhigh" if self.supports_xhigh(id) => "xhigh".into(),
            "xhigh" => "high".into(),
            "high" => "high".into(),
            "medium" => "medium".into(),
            _ => "low".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelBinding {
    pub credential_id: String,
    pub model_id: String,
    pub context_window: usize,
}

impl ModelBinding {
    pub fn new(
        credential_id: impl Into<String>,
        model_id: impl Into<String>,
        context_window: usize,
    ) -> Self {
        Self {
            credential_id: credential_id.into(),
            model_id: model_id.into(),
            context_window,
        }
    }
}

fn model_key(provider: &str, id: &str) -> String {
    format!("{provider}/{id}")
}

fn is_xhigh_model(id: &str) -> bool {
    id.starts_with("gpt-5")
        || (id.starts_with("o1") && !id.contains("mini"))
        || id.starts_with("o3")
        || matches!(
            id,
            "claude-3-5-sonnet" | "claude-3-7-sonnet" | "claude-sonnet-4"
        )
}

#[derive(Debug, Deserialize, Default)]
struct OverridesFile {
    #[serde(default)]
    models: Vec<ModelInfo>,
    #[serde(default)]
    compat: HashMap<String, CompatConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(provider: &str, id: &str) -> ModelInfo {
        let mut model = ModelInfo::new(provider, id, 128_000, 8_192);
        model.supports_tools = true;
        model.supports_reasoning = true;
        model.supports_reasoning_effort = true;
        model
    }

    #[test]
    fn registry_starts_without_rotary_owned_models() {
        assert!(ModelRegistry::new().is_empty());
    }

    #[test]
    fn consumer_metadata_controls_lookup_and_capabilities() {
        let registry = ModelRegistry::from_models([model("openrouter", "openai/gpt-4o")]);
        assert_eq!(
            registry
                .get_for_provider("openrouter", "openai/gpt-4o")
                .unwrap()
                .context_window,
            128_000
        );
        assert!(registry.supports_reasoning_effort_for("openrouter", "openai/gpt-4o"));
    }

    #[test]
    fn duplicate_model_ids_require_provider_qualification() {
        let registry = ModelRegistry::from_models([
            ModelInfo::new("openai", "shared", 1, 1),
            ModelInfo::new("anthropic", "shared", 2, 2),
        ]);
        assert!(registry.get("shared").is_none());
        assert_eq!(
            registry
                .get_for_provider("anthropic", "shared")
                .unwrap()
                .context_window,
            2
        );
    }

    #[test]
    fn compatibility_is_consumer_supplied() {
        let mut registry = ModelRegistry::new();
        registry.register_compat(
            "custom",
            CompatConfig {
                max_tokens_field: Some("output_limit".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            registry
                .compat("custom")
                .unwrap()
                .max_tokens_field
                .as_deref(),
            Some("output_limit")
        );
        assert!(registry.compat("openai").is_none());
    }

    #[test]
    fn model_binding_is_per_request_not_global() {
        let a = ModelBinding::new("cred-a", "gpt-4o", 128_000);
        let b = ModelBinding::new("cred-b", "claude", 200_000);
        assert_ne!(a.credential_id, b.credential_id);
        assert_ne!(a.model_id, b.model_id);
        assert_ne!(a.context_window, b.context_window);
    }

    #[test]
    fn json_parsing_does_not_add_defaults() {
        let registry = ModelRegistry::from_json(
            r#"{"models":[{"id":"live-model","provider":"custom","context_window":42,"max_output_tokens":7}]}"#,
        )
        .unwrap();
        assert_eq!(registry.models().count(), 1);
        assert!(registry.get("gpt-4o").is_none());
    }
}
