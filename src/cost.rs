//! Cost tracking per-model and per-session (Crush pattern).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Pricing for a single model, expressed as cost per 1M tokens.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ModelPricing {
    /// Cost per 1M input tokens.
    pub input_per_1m: f64,
    /// Cost per 1M output tokens.
    pub output_per_1m: f64,
    /// Cost per 1M cached input tokens, when supported.
    pub cache_read_per_1m: Option<f64>,
    /// Cost per 1M cache write tokens, when supported.
    pub cache_write_per_1m: Option<f64>,
}

impl ModelPricing {
    /// Create a pricing entry with only input/output rates.
    pub fn new(input_per_1m: f64, output_per_1m: f64) -> Self {
        Self {
            input_per_1m,
            output_per_1m,
            cache_read_per_1m: None,
            cache_write_per_1m: None,
        }
    }

    /// Builder method to set cache read pricing.
    pub fn with_cache_read(mut self, per_1m: f64) -> Self {
        self.cache_read_per_1m = Some(per_1m);
        self
    }

    /// Builder method to set cache write pricing.
    pub fn with_cache_write(mut self, per_1m: f64) -> Self {
        self.cache_write_per_1m = Some(per_1m);
        self
    }
}

/// Registry of model pricing with built-in entries for common models.
#[derive(Debug, Clone, Default)]
pub struct PricingRegistry {
    pricing: HashMap<String, ModelPricing>,
}

impl PricingRegistry {
    /// Create a registry pre-populated with pricing for common models.
    pub fn new() -> Self {
        let mut registry = Self::default();
        registry.register("gpt-4o", ModelPricing::new(2.50, 10.00));
        registry.register("gpt-4o-mini", ModelPricing::new(0.15, 0.60));
        registry.register(
            "claude-3.5-sonnet",
            ModelPricing::new(3.00, 15.00)
                .with_cache_read(0.30)
                .with_cache_write(3.75),
        );
        registry.register("claude-3.5-haiku", ModelPricing::new(0.25, 1.25));
        registry.register("o1", ModelPricing::new(15.00, 60.00));
        registry.register("o3-mini", ModelPricing::new(1.10, 4.40));
        registry.register("gemini-2.0-flash", ModelPricing::new(0.10, 0.40));
        registry.register("grok-3", ModelPricing::new(5.00, 15.00));
        registry
    }

    /// Look up pricing for a model.
    pub fn get(&self, model: &str) -> Option<ModelPricing> {
        self.pricing.get(model).copied()
    }

    /// Register or overwrite pricing for a model.
    pub fn register(&mut self, model: &str, pricing: ModelPricing) {
        self.pricing.insert(model.to_string(), pricing);
    }

    /// Estimate the cost of a single model call. Returns 0.0 for unknown
    /// models.
    pub fn estimate_cost(&self, model: &str, input_tokens: usize, output_tokens: usize) -> f64 {
        match self.pricing.get(model) {
            Some(p) => {
                let input = (input_tokens as f64 / 1_000_000.0) * p.input_per_1m;
                let output = (output_tokens as f64 / 1_000_000.0) * p.output_per_1m;
                input + output
            }
            None => 0.0,
        }
    }

    /// Estimate the cost of a single model call including cache tokens.
    pub fn estimate_cost_detailed(&self, model: &str, usage: &TokenUsage) -> f64 {
        match self.pricing.get(model) {
            Some(p) => {
                let input = (usage.input_tokens as f64 / 1_000_000.0) * p.input_per_1m;
                let output = (usage.output_tokens as f64 / 1_000_000.0) * p.output_per_1m;
                let cache_read = p
                    .cache_read_per_1m
                    .map(|r| (usage.cache_read_tokens as f64 / 1_000_000.0) * r)
                    .unwrap_or(0.0);
                let cache_write = p
                    .cache_write_per_1m
                    .map(|w| (usage.cache_write_tokens as f64 / 1_000_000.0) * w)
                    .unwrap_or(0.0);
                input + output + cache_read + cache_write
            }
            None => 0.0,
        }
    }
}

/// Token usage for a single model call.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub cache_read_tokens: usize,
    pub cache_write_tokens: usize,
}

/// A single recorded cost entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEntry {
    pub model: String,
    pub usage: TokenUsage,
    pub cost: f64,
    pub timestamp: DateTime<Utc>,
}

/// Tracks the accumulated cost of a session.
#[derive(Debug, Clone, Default)]
pub struct SessionCost {
    entries: Vec<CostEntry>,
    by_model: HashMap<String, f64>,
    total: f64,
    total_input: usize,
    total_output: usize,
}

impl SessionCost {
    /// Create a new empty session cost tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a model call, updating aggregate totals.
    pub fn record(&mut self, model: &str, usage: TokenUsage, registry: &PricingRegistry) {
        let cost = registry.estimate_cost_detailed(model, &usage);
        let entry = CostEntry {
            model: model.to_string(),
            usage,
            cost,
            timestamp: Utc::now(),
        };
        *self.by_model.entry(model.to_string()).or_insert(0.0) += cost;
        self.total += cost;
        self.total_input += usage.input_tokens;
        self.total_output += usage.output_tokens;
        self.entries.push(entry);
    }

    /// Total estimated cost across all recorded calls.
    pub fn total_cost(&self) -> f64 {
        self.total
    }

    /// Total input tokens across all recorded calls.
    pub fn total_input_tokens(&self) -> usize {
        self.total_input
    }

    /// Total output tokens across all recorded calls.
    pub fn total_output_tokens(&self) -> usize {
        self.total_output
    }

    /// Cost breakdown by model, sorted by descending cost.
    pub fn by_model(&self) -> Vec<(String, f64)> {
        let mut entries: Vec<(String, f64)> =
            self.by_model.iter().map(|(k, v)| (k.clone(), *v)).collect();
        entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        entries
    }

    /// Number of recorded calls.
    pub fn turn_count(&self) -> usize {
        self.entries.len()
    }

    /// All recorded cost entries.
    pub fn entries(&self) -> &[CostEntry] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_known_models() {
        let registry = PricingRegistry::new();
        assert!(registry.get("gpt-4o").is_some());
        assert!(registry.get("gpt-4o-mini").is_some());
        assert!(registry.get("claude-3.5-sonnet").is_some());
        assert!(registry.get("claude-3.5-haiku").is_some());
        assert!(registry.get("o1").is_some());
        assert!(registry.get("o3-mini").is_some());
        assert!(registry.get("gemini-2.0-flash").is_some());
        assert!(registry.get("grok-3").is_some());
    }

    #[test]
    fn registry_returns_none_for_unknown() {
        let registry = PricingRegistry::new();
        assert!(registry.get("unknown-model").is_none());
    }

    #[test]
    fn registry_can_register_custom() {
        let mut registry = PricingRegistry::new();
        registry.register("custom", ModelPricing::new(1.0, 2.0));
        let pricing = registry.get("custom").unwrap();
        assert_eq!(pricing.input_per_1m, 1.0);
        assert_eq!(pricing.output_per_1m, 2.0);
    }

    #[test]
    fn estimate_cost_known_model() {
        let registry = PricingRegistry::new();
        let cost = registry.estimate_cost("gpt-4o", 1_000_000, 500_000);
        let expected = 2.50 + (0.5 * 10.00);
        assert!((cost - expected).abs() < 1e-9);
    }

    #[test]
    fn estimate_cost_unknown_model_returns_zero() {
        let registry = PricingRegistry::new();
        let cost = registry.estimate_cost("unknown-model", 1_000_000, 500_000);
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn estimate_cost_detailed_includes_cache() {
        let registry = PricingRegistry::new();
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 0,
        };
        let cost = registry.estimate_cost_detailed("claude-3.5-sonnet", &usage);
        let expected = 3.00 + 0.30;
        assert!((cost - expected).abs() < 1e-9);
    }

    #[test]
    fn session_cost_tracks_total() {
        let registry = PricingRegistry::new();
        let mut session = SessionCost::new();
        session.record(
            "gpt-4o",
            TokenUsage {
                input_tokens: 1_000_000,
                output_tokens: 500_000,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            &registry,
        );
        let expected = 2.50 + 5.00;
        assert!((session.total_cost() - expected).abs() < 1e-9);
    }

    #[test]
    fn session_cost_tracks_tokens() {
        let registry = PricingRegistry::new();
        let mut session = SessionCost::new();
        session.record(
            "gpt-4o",
            TokenUsage {
                input_tokens: 100,
                output_tokens: 200,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            &registry,
        );
        session.record(
            "gpt-4o",
            TokenUsage {
                input_tokens: 300,
                output_tokens: 400,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            &registry,
        );
        assert_eq!(session.total_input_tokens(), 400);
        assert_eq!(session.total_output_tokens(), 600);
        assert_eq!(session.turn_count(), 2);
    }

    #[test]
    fn session_cost_by_model_breakdown() {
        let registry = PricingRegistry::new();
        let mut session = SessionCost::new();
        session.record(
            "gpt-4o",
            TokenUsage {
                input_tokens: 1_000_000,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            &registry,
        );
        session.record(
            "gpt-4o-mini",
            TokenUsage {
                input_tokens: 1_000_000,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            &registry,
        );
        let by_model = session.by_model();
        assert_eq!(by_model.len(), 2);
        assert_eq!(by_model[0].0, "gpt-4o");
        assert!(by_model[0].1 > by_model[1].1);
    }

    #[test]
    fn session_cost_unknown_model_zero() {
        let registry = PricingRegistry::new();
        let mut session = SessionCost::new();
        session.record(
            "unknown-model",
            TokenUsage {
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            &registry,
        );
        assert_eq!(session.total_cost(), 0.0);
        assert_eq!(session.turn_count(), 1);
    }
}
