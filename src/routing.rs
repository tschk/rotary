//! Smart routing: classify each user turn as simple or strong and route to
//! different models (OpenClaude pattern), plus per-agent model routing.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Classification of a single user turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnComplexity {
    /// Short, no code, no reasoning keywords — cheap model is sufficient.
    Simple,
    /// Long, code blocks, or reasoning keywords — strong model required.
    Strong,
}

impl TurnComplexity {
    pub fn as_str(&self) -> &'static str {
        match self {
            TurnComplexity::Simple => "simple",
            TurnComplexity::Strong => "strong",
        }
    }
}

/// Configuration for smart routing between a simple and a strong model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConfig {
    /// Model used for simple turns (e.g., "gpt-4o-mini").
    pub simple_model: Option<String>,
    /// Model used for strong turns (e.g., "gpt-4o").
    pub strong_model: Option<String>,
    /// Whether smart routing is active.
    pub enabled: bool,
    /// Weight applied to cached context when estimating savings.
    pub cache_weight: f64,
    /// Weight applied to fresh input when estimating savings.
    pub fresh_weight: f64,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            simple_model: None,
            strong_model: None,
            enabled: false,
            cache_weight: 0.4,
            fresh_weight: 0.6,
        }
    }
}

const REASONING_KEYWORDS: &[&str] = &[
    "analyze",
    "design",
    "refactor",
    "debug",
    "architecture",
    "implement",
    "optimize",
    "review",
];

/// Heuristic classifier: simple turns are short, code-free, and lack
/// reasoning keywords; strong turns are long, contain code blocks, or
/// mention reasoning keywords.
pub fn classify_turn(prompt: &str) -> TurnComplexity {
    let has_code = prompt.contains("```");
    let has_reasoning = REASONING_KEYWORDS
        .iter()
        .any(|kw| prompt.to_lowercase().contains(kw));
    let is_long = prompt.chars().count() >= 200;

    if has_code || has_reasoning || is_long {
        TurnComplexity::Strong
    } else {
        TurnComplexity::Simple
    }
}

/// Aggregate statistics produced by a [`SmartRouter`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingStats {
    pub total_turns: usize,
    pub simple_turns: usize,
    pub strong_turns: usize,
    pub escalations: usize,
    pub estimated_savings: f64,
}

/// Routes prompts to a simple or strong model based on turn complexity.
pub struct SmartRouter {
    config: RoutingConfig,
    stats: parking_lot::Mutex<RoutingStats>,
}

impl SmartRouter {
    /// Create a new router with the given configuration.
    pub fn new(config: RoutingConfig) -> Self {
        Self {
            config,
            stats: parking_lot::Mutex::new(RoutingStats::default()),
        }
    }

    /// Route a single prompt, returning the model to use. When routing is
    /// disabled or the target model is unset, the `current_model` is
    /// returned unchanged.
    pub fn route(&self, prompt: &str, current_model: &str) -> String {
        let complexity = classify_turn(prompt);
        let mut stats = self.stats.lock();
        stats.total_turns += 1;
        match complexity {
            TurnComplexity::Simple => stats.simple_turns += 1,
            TurnComplexity::Strong => stats.strong_turns += 1,
        }

        if !self.config.enabled {
            return current_model.to_string();
        }

        let target = match complexity {
            TurnComplexity::Simple => self.config.simple_model.as_deref(),
            TurnComplexity::Strong => self.config.strong_model.as_deref(),
        };

        match target {
            Some(model) => {
                if matches!(complexity, TurnComplexity::Strong)
                    && self
                        .config
                        .simple_model
                        .as_deref()
                        .map(|s| s == current_model)
                        .unwrap_or(false)
                {
                    stats.escalations += 1;
                }
                model.to_string()
            }
            None => current_model.to_string(),
        }
    }

    /// Route a batch of prompts, returning the model to use for each.
    pub fn route_batch(&self, prompts: &[&str], current_model: &str) -> Vec<String> {
        prompts
            .iter()
            .map(|p| self.route(p, current_model))
            .collect()
    }

    /// Return a snapshot of the router's aggregate statistics.
    pub fn stats(&self) -> RoutingStats {
        self.stats.lock().clone()
    }
}

/// A per-agent model/provider override (OpenClaude agentModels pattern).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRoute {
    pub agent_name: String,
    pub model: Option<String>,
    pub provider: Option<String>,
}

/// Routes agent names to configured models, falling back to a default.
pub struct AgentRouter {
    routes: HashMap<String, AgentRoute>,
    default_model: String,
}

impl AgentRouter {
    /// Create a new agent router with the given default model.
    pub fn new(default_model: String) -> Self {
        Self {
            routes: HashMap::new(),
            default_model,
        }
    }

    /// Register a route for an agent.
    pub fn register(&mut self, route: AgentRoute) {
        self.routes.insert(route.agent_name.clone(), route);
    }

    /// Return the model for the named agent, falling back to the default.
    pub fn route_for_agent(&self, agent_name: &str) -> &str {
        match self.routes.get(agent_name) {
            Some(route) => route.model.as_deref().unwrap_or(&self.default_model),
            None => &self.default_model,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_simple_turn() {
        assert_eq!(classify_turn("hello"), TurnComplexity::Simple);
        assert_eq!(classify_turn("what is 2+2?"), TurnComplexity::Simple);
    }

    #[test]
    fn classifies_long_turn_as_strong() {
        let prompt = "x".repeat(200);
        assert_eq!(classify_turn(&prompt), TurnComplexity::Strong);
    }

    #[test]
    fn classifies_code_block_as_strong() {
        assert_eq!(
            classify_turn("fix this:\n```rust\nfn main() {}\n```"),
            TurnComplexity::Strong
        );
    }

    #[test]
    fn classifies_reasoning_keyword_as_strong() {
        assert_eq!(classify_turn("analyze the data"), TurnComplexity::Strong);
        assert_eq!(
            classify_turn("refactor this module"),
            TurnComplexity::Strong
        );
        assert_eq!(
            classify_turn("design the architecture"),
            TurnComplexity::Strong
        );
    }

    #[test]
    fn router_returns_current_model_when_disabled() {
        let router = SmartRouter::new(RoutingConfig::default());
        let model = router.route("analyze this", "gpt-4o");
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn router_routes_simple_to_simple_model() {
        let config = RoutingConfig {
            enabled: true,
            simple_model: Some("gpt-4o-mini".to_string()),
            strong_model: Some("gpt-4o".to_string()),
            ..Default::default()
        };
        let router = SmartRouter::new(config);
        assert_eq!(router.route("hello", "gpt-4o"), "gpt-4o-mini");
    }

    #[test]
    fn router_routes_strong_to_strong_model() {
        let config = RoutingConfig {
            enabled: true,
            simple_model: Some("gpt-4o-mini".to_string()),
            strong_model: Some("gpt-4o".to_string()),
            ..Default::default()
        };
        let router = SmartRouter::new(config);
        assert_eq!(
            router.route("analyze the architecture", "gpt-4o-mini"),
            "gpt-4o"
        );
    }

    #[test]
    fn router_falls_back_when_target_unset() {
        let config = RoutingConfig {
            enabled: true,
            simple_model: None,
            ..Default::default()
        };
        let router = SmartRouter::new(config);
        assert_eq!(router.route("hello", "gpt-4o"), "gpt-4o");
    }

    #[test]
    fn router_tracks_stats() {
        let config = RoutingConfig {
            enabled: true,
            simple_model: Some("gpt-4o-mini".to_string()),
            strong_model: Some("gpt-4o".to_string()),
            ..Default::default()
        };
        let router = SmartRouter::new(config);
        router.route("hello", "gpt-4o");
        router.route("analyze this", "gpt-4o");
        router.route("refactor that", "gpt-4o");
        let stats = router.stats();
        assert_eq!(stats.total_turns, 3);
        assert_eq!(stats.simple_turns, 1);
        assert_eq!(stats.strong_turns, 2);
    }

    #[test]
    fn router_route_batch() {
        let config = RoutingConfig {
            enabled: true,
            simple_model: Some("gpt-4o-mini".to_string()),
            strong_model: Some("gpt-4o".to_string()),
            ..Default::default()
        };
        let router = SmartRouter::new(config);
        let models = router.route_batch(&["hello", "analyze this"], "gpt-4o");
        assert_eq!(models, vec!["gpt-4o-mini", "gpt-4o"]);
    }

    #[test]
    fn agent_router_falls_back_to_default() {
        let router = AgentRouter::new("gpt-4o".to_string());
        assert_eq!(router.route_for_agent("unknown"), "gpt-4o");
    }

    #[test]
    fn agent_router_returns_registered_model() {
        let mut router = AgentRouter::new("gpt-4o".to_string());
        router.register(AgentRoute {
            agent_name: "researcher".to_string(),
            model: Some("o1".to_string()),
            provider: Some("openai".to_string()),
        });
        assert_eq!(router.route_for_agent("researcher"), "o1");
    }

    #[test]
    fn agent_router_falls_back_when_model_unset() {
        let mut router = AgentRouter::new("gpt-4o".to_string());
        router.register(AgentRoute {
            agent_name: "researcher".to_string(),
            model: None,
            provider: None,
        });
        assert_eq!(router.route_for_agent("researcher"), "gpt-4o");
    }

    #[test]
    fn turn_complexity_as_str() {
        assert_eq!(TurnComplexity::Simple.as_str(), "simple");
        assert_eq!(TurnComplexity::Strong.as_str(), "strong");
    }
}
