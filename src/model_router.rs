//! Intelligent model routing: map task tiers to model preferences, with
//! prompt heuristics, fallback resolution, and low-power proactive
//! monitoring inspired by Hermes/omi.
//!
//! [`ModelRouter`] selects a [`ModelTier`] for a given [`TaskType`] or prompt.
//! [`ProactiveMonitor`] runs lightweight background classification and
//! learning extraction on the Lite tier.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Errors raised by model routing operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModelRouterError {
    #[error("no model available for tier: {0}")]
    NoModelAvailable(String),
    #[error("invalid tier: {0}")]
    InvalidTier(String),
}

/// Power level for a task type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskTier {
    /// Low-power tasks: skill creation, monitoring, consolidation, classification.
    Lite,
    /// Normal tasks: code generation, tool execution, conversation.
    Standard,
    /// Complex tasks: multi-step reasoning, architecture, review, debugging.
    Heavy,
    /// Delegated tasks: model chosen by primary model.
    Subagent,
}

impl TaskTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskTier::Lite => "lite",
            TaskTier::Standard => "standard",
            TaskTier::Heavy => "heavy",
            TaskTier::Subagent => "subagent",
        }
    }
}

/// Specific task category that maps to a [`TaskTier`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskType {
    SkillExtraction,
    MemoryConsolidation,
    ProactiveMonitor,
    KeywordExtraction,
    SimpleClassification,
    CodeGeneration,
    ToolExecution,
    Conversation,
    FileEdit,
    ShellCommand,
    ArchitectureDesign,
    CodeReview,
    Debugging,
    MultiStepReasoning,
    ComplexRefactor,
    /// Delegated task; the inner string is the task description.
    SubagentTask(String),
}

impl TaskType {
    /// Returns the [`TaskTier`] this task type belongs to.
    pub fn tier(&self) -> TaskTier {
        match self {
            TaskType::SkillExtraction
            | TaskType::MemoryConsolidation
            | TaskType::ProactiveMonitor
            | TaskType::KeywordExtraction
            | TaskType::SimpleClassification => TaskTier::Lite,
            TaskType::CodeGeneration
            | TaskType::ToolExecution
            | TaskType::Conversation
            | TaskType::FileEdit
            | TaskType::ShellCommand => TaskTier::Standard,
            TaskType::ArchitectureDesign
            | TaskType::CodeReview
            | TaskType::Debugging
            | TaskType::MultiStepReasoning
            | TaskType::ComplexRefactor => TaskTier::Heavy,
            TaskType::SubagentTask(_) => TaskTier::Subagent,
        }
    }
}

/// Model configuration for a single tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelTier {
    /// Preferred model for this tier.
    pub model: String,
    /// Fallback model if the preferred model is unavailable.
    pub fallback: Option<String>,
    /// Maximum output tokens.
    pub max_tokens: u32,
    /// Sampling temperature.
    pub temperature: f32,
    /// Reasoning effort for thinking models ("low", "medium", "high", "xhigh").
    pub thinking_level: Option<String>,
}

impl ModelTier {
    /// Builds a [`ModelTier`] with the given preferred model.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            fallback: None,
            max_tokens: 4096,
            temperature: 0.7,
            thinking_level: None,
        }
    }

    fn lite() -> Self {
        Self {
            model: "gpt-4o-mini".to_string(),
            fallback: Some("claude-3.5-haiku".to_string()),
            max_tokens: 2048,
            temperature: 0.3,
            thinking_level: None,
        }
    }

    fn standard() -> Self {
        Self {
            model: "gpt-4o".to_string(),
            fallback: Some("claude-3.5-sonnet".to_string()),
            max_tokens: 8192,
            temperature: 0.7,
            thinking_level: None,
        }
    }

    fn heavy() -> Self {
        Self {
            model: "claude-3.5-sonnet".to_string(),
            fallback: Some("o1".to_string()),
            max_tokens: 16384,
            temperature: 0.5,
            thinking_level: Some("high".to_string()),
        }
    }

    fn subagent() -> Self {
        Self {
            model: "gpt-4o-mini".to_string(),
            fallback: Some("claude-3.5-haiku".to_string()),
            max_tokens: 4096,
            temperature: 0.5,
            thinking_level: None,
        }
    }
}

/// Configuration for a [`ModelRouter`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    /// Tier to model configuration mapping.
    pub tiers: HashMap<TaskTier, ModelTier>,
    /// Keyword to tier mapping used by prompt heuristics.
    pub prompt_heuristics: HashMap<String, TaskTier>,
    /// Primary model to default subagent model mapping.
    pub subagent_defaults: HashMap<String, String>,
}

impl Default for RouterConfig {
    fn default() -> Self {
        let mut tiers = HashMap::new();
        tiers.insert(TaskTier::Lite, ModelTier::lite());
        tiers.insert(TaskTier::Standard, ModelTier::standard());
        tiers.insert(TaskTier::Heavy, ModelTier::heavy());
        tiers.insert(TaskTier::Subagent, ModelTier::subagent());

        let mut prompt_heuristics = HashMap::new();
        for kw in ["architecture", "design", "review", "debug"] {
            prompt_heuristics.insert(kw.to_string(), TaskTier::Heavy);
        }
        for kw in ["skill", "memory", "consolidate", "monitor", "classify"] {
            prompt_heuristics.insert(kw.to_string(), TaskTier::Lite);
        }

        let mut subagent_defaults = HashMap::new();
        subagent_defaults.insert("gpt-4o".to_string(), "gpt-4o-mini".to_string());
        subagent_defaults.insert(
            "claude-3.5-sonnet".to_string(),
            "claude-3.5-haiku".to_string(),
        );

        Self {
            tiers,
            prompt_heuristics,
            subagent_defaults,
        }
    }
}

/// Routes tasks to appropriate models based on tier heuristics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRouter {
    config: RouterConfig,
}

impl Default for ModelRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelRouter {
    /// Creates a router with sensible defaults.
    pub fn new() -> Self {
        Self {
            config: RouterConfig::default(),
        }
    }

    /// Creates a router from a custom [`RouterConfig`].
    pub fn with_config(config: RouterConfig) -> Self {
        Self { config }
    }

    /// Returns the [`ModelTier`] for a given [`TaskType`].
    pub fn route(&self, task: &TaskType) -> &ModelTier {
        let tier = task.tier();
        self.config
            .tiers
            .get(&tier)
            .expect("tier config must be present")
    }

    /// Heuristic routing based on prompt content.
    ///
    /// Keywords are matched case-insensitively against the configured
    /// [`RouterConfig::prompt_heuristics`]. The first matching keyword wins;
    /// if none match, [`TaskTier::Standard`] is used.
    pub fn route_prompt(&self, prompt: &str) -> &ModelTier {
        let lower = prompt.to_lowercase();
        for (keyword, tier) in &self.config.prompt_heuristics {
            if lower.contains(keyword) {
                if let Some(model_tier) = self.config.tiers.get(tier) {
                    return model_tier;
                }
            }
        }
        self.config
            .tiers
            .get(&TaskTier::Standard)
            .expect("standard tier config must be present")
    }

    /// Overrides the preferred model for a tier.
    pub fn set_model(&mut self, tier: TaskTier, model: String) {
        if let Some(t) = self.config.tiers.get_mut(&tier) {
            t.model = model;
        }
    }

    /// Sets the fallback model for a tier.
    pub fn set_fallback(&mut self, tier: TaskTier, fallback: String) {
        if let Some(t) = self.config.tiers.get_mut(&tier) {
            t.fallback = Some(fallback);
        }
    }

    /// Resolves a task to an available model, falling back if the preferred
    /// model is unavailable.
    ///
    /// Returns [`ModelRouterError::NoModelAvailable`] if neither the preferred
    /// nor the fallback model is in `available_models`.
    pub fn resolve(
        &self,
        task: &TaskType,
        available_models: &[String],
    ) -> Result<String, ModelRouterError> {
        let tier = self.route(task);
        if available_models.iter().any(|m| m == &tier.model) {
            return Ok(tier.model.clone());
        }
        if let Some(ref fallback) = tier.fallback {
            if available_models.iter().any(|m| m == fallback) {
                return Ok(fallback.clone());
            }
        }
        Err(ModelRouterError::NoModelAvailable(format!(
            "no available model for tier {} (preferred: {}, fallback: {:?})",
            task.tier().as_str(),
            tier.model,
            tier.fallback
        )))
    }

    /// Lets the primary model decide the subagent model based on task
    /// complexity.
    ///
    /// Uses [`SubagentModelSelector`] heuristics. If the primary model has a
    /// configured default and no heuristic matches, that default is used.
    pub fn subagent_model(&self, task_description: &str, primary_model: &str) -> String {
        let selector = SubagentModelSelector::new();
        let available: Vec<String> = self
            .config
            .tiers
            .values()
            .map(|t| t.model.clone())
            .collect();
        let selected = selector.select(task_description, primary_model, &available);
        if selected != primary_model {
            return selected;
        }
        if let Some(default) = self.config.subagent_defaults.get(primary_model) {
            return default.clone();
        }
        self.config
            .tiers
            .get(&TaskTier::Subagent)
            .map(|t| t.model.clone())
            .unwrap_or_else(|| "gpt-4o-mini".to_string())
    }

    /// Returns a reference to the underlying [`RouterConfig`].
    pub fn config(&self) -> &RouterConfig {
        &self.config
    }
}

/// Lets the primary model choose subagent models based on task complexity
/// heuristics.
#[derive(Debug, Clone)]
pub struct SubagentModelSelector {
    lite_model: String,
    standard_model: String,
    heavy_model: String,
}

impl Default for SubagentModelSelector {
    fn default() -> Self {
        Self::new()
    }
}

impl SubagentModelSelector {
    /// Creates a selector with default model preferences.
    pub fn new() -> Self {
        Self {
            lite_model: "gpt-4o-mini".to_string(),
            standard_model: "gpt-4o".to_string(),
            heavy_model: "claude-3.5-sonnet".to_string(),
        }
    }

    /// Selects a subagent model for the given task description.
    ///
    /// Heuristics:
    /// - "explore", "search", "find" -> lite model
    /// - "implement", "write", "create" -> standard model
    /// - "review", "analyze", "design" -> heavy model
    /// - default -> subagent tier model (first available)
    pub fn select(
        &self,
        task_description: &str,
        primary_model: &str,
        available_models: &[String],
    ) -> String {
        let lower = task_description.to_lowercase();
        let pick = |model: &str| -> String {
            if available_models.iter().any(|m| m == model) {
                model.to_string()
            } else {
                primary_model.to_string()
            }
        };

        if ["explore", "search", "find"]
            .iter()
            .any(|k| lower.contains(k))
        {
            return pick(&self.lite_model);
        }
        if ["implement", "write", "create"]
            .iter()
            .any(|k| lower.contains(k))
        {
            return pick(&self.standard_model);
        }
        if ["review", "analyze", "design"]
            .iter()
            .any(|k| lower.contains(k))
        {
            return pick(&self.heavy_model);
        }
        pick(&self.lite_model)
    }

    /// Builds a prompt for the primary model to select a subagent model.
    pub fn build_prompt(&self, task_description: &str, available_models: &[String]) -> String {
        let models = available_models.join(", ");
        format!(
            "You are selecting a subagent model for the following task.\n\n\
             Task: {task_description}\n\n\
             Available models: {models}\n\n\
             Choose the most appropriate model for this task based on its \
             complexity. Reply with only the model name from the available \
             list. Use a lighter model for exploration/search, a standard \
             model for implementation, and a heavier model for review, \
             analysis, or design."
        )
    }
}

/// Classification of a completed conversation turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnClassification {
    /// Estimated complexity of the turn.
    pub complexity: TaskTier,
    /// Tools invoked during the turn.
    pub tools_used: Vec<String>,
    /// Whether the turn completed successfully.
    pub success: bool,
    /// Wall-clock duration in seconds.
    pub duration_secs: f64,
}

/// A learning extracted from conversation history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Learning {
    /// Short summary of the learning.
    pub summary: String,
    /// Category tag (e.g., "tooling", "debugging").
    pub category: String,
    /// Confidence in the learning, in `[0.0, 1.0]`.
    pub confidence: f64,
    /// When the learning was recorded.
    pub timestamp: DateTime<Utc>,
}

/// A suggestion to create a new skill from observed conversation patterns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillSuggestion {
    /// Proposed skill name.
    pub name: String,
    /// Human-readable description of what the skill does.
    pub description: String,
    /// Prompt patterns that should trigger the skill.
    pub trigger_patterns: Vec<String>,
    /// Instructions the skill should follow when triggered.
    pub instructions: String,
    /// Confidence in the suggestion, in `[0.0, 1.0]`.
    pub confidence: f64,
}

/// Low-power background monitoring inspired by Hermes/omi.
///
/// Runs on the Lite tier and classifies turns, extracts learnings, and
/// suggests skills without blocking the main agent loop.
#[derive(Debug, Clone)]
pub struct ProactiveMonitor {
    router: ModelRouter,
}

impl ProactiveMonitor {
    /// Creates a new monitor backed by the given router.
    pub fn new(router: ModelRouter) -> Self {
        Self { router }
    }

    /// Returns `true` if enough time has elapsed since `last_run` for the
    /// monitor to run again.
    pub fn should_run(&self, last_run: DateTime<Utc>, interval_minutes: u32) -> bool {
        let elapsed = Utc::now().signed_duration_since(last_run);
        elapsed.num_minutes() >= i64::from(interval_minutes)
    }

    /// Classifies a completed turn based on its prompt and response.
    ///
    /// Heuristics mirror [`ModelRouter::route_prompt`] for complexity, and
    /// inspect the response for tool-call markers and success indicators.
    pub fn classify_turn(&self, prompt: &str, response: &str) -> TurnClassification {
        let tier = self.router.route_prompt(prompt);
        let complexity = match tier.model.as_str() {
            "gpt-4o-mini" => TaskTier::Lite,
            "gpt-4o" => TaskTier::Standard,
            "claude-3.5-sonnet" => TaskTier::Heavy,
            _ => TaskTier::Standard,
        };

        let mut tools_used = Vec::new();
        let lower = response.to_lowercase();
        for tool in [
            "read", "write", "edit", "exec", "grep", "search", "bash", "shell",
        ] {
            if lower.contains(tool) {
                tools_used.push(tool.to_string());
            }
        }

        let success = !response.trim().is_empty()
            && !lower.contains("error")
            && !lower.contains("failed")
            && !lower.contains("traceback");

        let duration_secs = if tools_used.is_empty() { 0.5 } else { 2.0 } * tools_used.len() as f64;

        TurnClassification {
            complexity,
            tools_used,
            success,
            duration_secs,
        }
    }

    /// Extracts a [`Learning`] from a conversation transcript, if one is
    /// detectable.
    ///
    /// A learning is detected when a turn contains an error followed by a
    /// resolution, or when a tool usage pattern repeats.
    pub fn extract_learning(&self, conversation: &[String]) -> Option<Learning> {
        if conversation.len() < 2 {
            return None;
        }
        let combined = conversation.join("\n");
        let lower = combined.to_lowercase();

        let (category, summary, confidence) = if lower.contains("error") || lower.contains("failed")
        {
            (
                "debugging".to_string(),
                "Errors were encountered and resolved during the turn.".to_string(),
                0.7,
            )
        } else if lower.contains("refactor") {
            (
                "refactoring".to_string(),
                "A refactoring pattern was applied successfully.".to_string(),
                0.6,
            )
        } else if lower.contains("test") {
            (
                "testing".to_string(),
                "Tests were run and informed the workflow.".to_string(),
                0.5,
            )
        } else {
            return None;
        };

        Some(Learning {
            summary,
            category,
            confidence,
            timestamp: Utc::now(),
        })
    }

    /// Suggests creating a skill when a prompt pattern repeats across the
    /// conversation.
    pub fn suggest_skill(&self, conversation: &[String]) -> Option<SkillSuggestion> {
        if conversation.len() < 3 {
            return None;
        }
        let lower: Vec<String> = conversation.iter().map(|s| s.to_lowercase()).collect();

        let (name, description, trigger_patterns, instructions, confidence) =
            if lower.iter().filter(|s| s.contains("test")).count() >= 2 {
                (
                    "run-tests".to_string(),
                    "Run the project test suite and report failures.".to_string(),
                    vec!["run tests".to_string(), "check tests".to_string()],
                    "Execute the test command, capture output, and summarize failures.".to_string(),
                    0.8,
                )
            } else if lower.iter().filter(|s| s.contains("lint")).count() >= 2 {
                (
                    "lint".to_string(),
                    "Run the linter and report issues.".to_string(),
                    vec!["lint".to_string(), "check style".to_string()],
                    "Run the configured linter and summarize violations.".to_string(),
                    0.7,
                )
            } else if lower.iter().filter(|s| s.contains("build")).count() >= 2 {
                (
                    "build".to_string(),
                    "Build the project and report errors.".to_string(),
                    vec!["build".to_string(), "compile".to_string()],
                    "Run the build command and report any compilation errors.".to_string(),
                    0.6,
                )
            } else {
                return None;
            };

        Some(SkillSuggestion {
            name,
            description,
            trigger_patterns,
            instructions,
            confidence,
        })
    }

    /// Returns a reference to the monitor's router.
    pub fn router(&self) -> &ModelRouter {
        &self.router
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lite_task_types_map_to_lite_tier() {
        assert_eq!(TaskType::SkillExtraction.tier(), TaskTier::Lite);
        assert_eq!(TaskType::MemoryConsolidation.tier(), TaskTier::Lite);
        assert_eq!(TaskType::ProactiveMonitor.tier(), TaskTier::Lite);
        assert_eq!(TaskType::KeywordExtraction.tier(), TaskTier::Lite);
        assert_eq!(TaskType::SimpleClassification.tier(), TaskTier::Lite);
    }

    #[test]
    fn standard_task_types_map_to_standard_tier() {
        assert_eq!(TaskType::CodeGeneration.tier(), TaskTier::Standard);
        assert_eq!(TaskType::ToolExecution.tier(), TaskTier::Standard);
        assert_eq!(TaskType::Conversation.tier(), TaskTier::Standard);
        assert_eq!(TaskType::FileEdit.tier(), TaskTier::Standard);
        assert_eq!(TaskType::ShellCommand.tier(), TaskTier::Standard);
    }

    #[test]
    fn heavy_task_types_map_to_heavy_tier() {
        assert_eq!(TaskType::ArchitectureDesign.tier(), TaskTier::Heavy);
        assert_eq!(TaskType::CodeReview.tier(), TaskTier::Heavy);
        assert_eq!(TaskType::Debugging.tier(), TaskTier::Heavy);
        assert_eq!(TaskType::MultiStepReasoning.tier(), TaskTier::Heavy);
        assert_eq!(TaskType::ComplexRefactor.tier(), TaskTier::Heavy);
    }

    #[test]
    fn subagent_task_maps_to_subagent_tier() {
        assert_eq!(
            TaskType::SubagentTask("explore the repo".to_string()).tier(),
            TaskTier::Subagent
        );
    }

    #[test]
    fn route_returns_correct_model_tier() {
        let router = ModelRouter::new();
        let lite = router.route(&TaskType::SkillExtraction);
        assert_eq!(lite.model, "gpt-4o-mini");
        assert_eq!(lite.max_tokens, 2048);

        let standard = router.route(&TaskType::CodeGeneration);
        assert_eq!(standard.model, "gpt-4o");
        assert_eq!(standard.max_tokens, 8192);

        let heavy = router.route(&TaskType::ArchitectureDesign);
        assert_eq!(heavy.model, "claude-3.5-sonnet");
        assert_eq!(heavy.thinking_level, Some("high".to_string()));

        let subagent = router.route(&TaskType::SubagentTask("do thing".to_string()));
        assert_eq!(subagent.model, "gpt-4o-mini");
        assert_eq!(subagent.max_tokens, 4096);
    }

    #[test]
    fn route_prompt_architecture_is_heavy() {
        let router = ModelRouter::new();
        let tier = router.route_prompt("design the architecture for the new module");
        assert_eq!(tier.model, "claude-3.5-sonnet");
    }

    #[test]
    fn route_prompt_skill_is_lite() {
        let router = ModelRouter::new();
        let tier = router.route_prompt("extract a skill from this conversation");
        assert_eq!(tier.model, "gpt-4o-mini");
    }

    #[test]
    fn route_prompt_default_is_standard() {
        let router = ModelRouter::new();
        let tier = router.route_prompt("write a function that adds two numbers");
        assert_eq!(tier.model, "gpt-4o");
    }

    #[test]
    fn set_model_overrides_tier_model() {
        let mut router = ModelRouter::new();
        router.set_model(TaskTier::Lite, "custom-lite".to_string());
        let tier = router.route(&TaskType::SkillExtraction);
        assert_eq!(tier.model, "custom-lite");
    }

    #[test]
    fn set_fallback_overrides_tier_fallback() {
        let mut router = ModelRouter::new();
        router.set_fallback(TaskTier::Standard, "custom-fallback".to_string());
        let tier = router.route(&TaskType::CodeGeneration);
        assert_eq!(tier.fallback, Some("custom-fallback".to_string()));
    }

    #[test]
    fn resolve_returns_preferred_when_available() {
        let router = ModelRouter::new();
        let available = vec![
            "gpt-4o-mini".to_string(),
            "gpt-4o".to_string(),
            "claude-3.5-sonnet".to_string(),
        ];
        let model = router
            .resolve(&TaskType::CodeGeneration, &available)
            .expect("should resolve");
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn resolve_falls_back_when_preferred_unavailable() {
        let router = ModelRouter::new();
        let available = vec!["claude-3.5-sonnet".to_string()];
        let model = router
            .resolve(&TaskType::CodeGeneration, &available)
            .expect("should resolve via fallback");
        assert_eq!(model, "claude-3.5-sonnet");
    }

    #[test]
    fn resolve_errors_when_nothing_available() {
        let router = ModelRouter::new();
        let available = vec!["some-other-model".to_string()];
        let result = router.resolve(&TaskType::CodeGeneration, &available);
        assert!(matches!(result, Err(ModelRouterError::NoModelAvailable(_))));
    }

    #[test]
    fn subagent_model_selection_explore_is_lite() {
        let selector = SubagentModelSelector::new();
        let available = vec![
            "gpt-4o-mini".to_string(),
            "gpt-4o".to_string(),
            "claude-3.5-sonnet".to_string(),
        ];
        let model = selector.select("explore the repository structure", "gpt-4o", &available);
        assert_eq!(model, "gpt-4o-mini");
    }

    #[test]
    fn subagent_model_selection_search_is_lite() {
        let selector = SubagentModelSelector::new();
        let available = vec![
            "gpt-4o-mini".to_string(),
            "gpt-4o".to_string(),
            "claude-3.5-sonnet".to_string(),
        ];
        let model = selector.select("search for usages of foo", "gpt-4o", &available);
        assert_eq!(model, "gpt-4o-mini");
    }

    #[test]
    fn subagent_model_selection_implement_is_standard() {
        let selector = SubagentModelSelector::new();
        let available = vec![
            "gpt-4o-mini".to_string(),
            "gpt-4o".to_string(),
            "claude-3.5-sonnet".to_string(),
        ];
        let model = selector.select("implement the new endpoint", "gpt-4o", &available);
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn subagent_model_selection_write_is_standard() {
        let selector = SubagentModelSelector::new();
        let available = vec![
            "gpt-4o-mini".to_string(),
            "gpt-4o".to_string(),
            "claude-3.5-sonnet".to_string(),
        ];
        let model = selector.select("write the documentation", "gpt-4o", &available);
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn subagent_model_selection_review_is_heavy() {
        let selector = SubagentModelSelector::new();
        let available = vec![
            "gpt-4o-mini".to_string(),
            "gpt-4o".to_string(),
            "claude-3.5-sonnet".to_string(),
        ];
        let model = selector.select("review the pull request", "gpt-4o", &available);
        assert_eq!(model, "claude-3.5-sonnet");
    }

    #[test]
    fn subagent_model_selection_analyze_is_heavy() {
        let selector = SubagentModelSelector::new();
        let available = vec![
            "gpt-4o-mini".to_string(),
            "gpt-4o".to_string(),
            "claude-3.5-sonnet".to_string(),
        ];
        let model = selector.select("analyze the performance bottleneck", "gpt-4o", &available);
        assert_eq!(model, "claude-3.5-sonnet");
    }

    #[test]
    fn subagent_model_selection_default_is_lite() {
        let selector = SubagentModelSelector::new();
        let available = vec![
            "gpt-4o-mini".to_string(),
            "gpt-4o".to_string(),
            "claude-3.5-sonnet".to_string(),
        ];
        let model = selector.select("do something generic", "gpt-4o", &available);
        assert_eq!(model, "gpt-4o-mini");
    }

    #[test]
    fn subagent_model_selection_falls_back_to_primary_when_unavailable() {
        let selector = SubagentModelSelector::new();
        let available = vec!["gpt-4o".to_string()];
        let model = selector.select("explore the repo", "gpt-4o", &available);
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn subagent_model_prompt_contains_task_and_models() {
        let selector = SubagentModelSelector::new();
        let available = vec!["gpt-4o-mini".to_string(), "gpt-4o".to_string()];
        let prompt = selector.build_prompt("explore the repo", &available);
        assert!(prompt.contains("explore the repo"));
        assert!(prompt.contains("gpt-4o-mini"));
        assert!(prompt.contains("gpt-4o"));
    }

    #[test]
    fn router_subagent_model_uses_heuristics() {
        let router = ModelRouter::new();
        let model = router.subagent_model("review the architecture", "gpt-4o");
        assert_eq!(model, "claude-3.5-sonnet");
    }

    #[test]
    fn router_subagent_model_uses_default_for_unknown_pattern() {
        let router = ModelRouter::new();
        let model = router.subagent_model("do something generic", "gpt-4o");
        assert_eq!(model, "gpt-4o-mini");
    }

    #[test]
    fn config_based_router_construction() {
        let mut config = RouterConfig::default();
        config.tiers.insert(
            TaskTier::Lite,
            ModelTier {
                model: "my-lite".to_string(),
                fallback: None,
                max_tokens: 1024,
                temperature: 0.1,
                thinking_level: None,
            },
        );
        let router = ModelRouter::with_config(config);
        let tier = router.route(&TaskType::SkillExtraction);
        assert_eq!(tier.model, "my-lite");
        assert_eq!(tier.max_tokens, 1024);
    }

    #[test]
    fn proactive_monitor_should_run_when_interval_elapsed() {
        let router = ModelRouter::new();
        let monitor = ProactiveMonitor::new(router);
        let last_run = Utc::now() - chrono::Duration::minutes(10);
        assert!(monitor.should_run(last_run, 5));
    }

    #[test]
    fn proactive_monitor_should_not_run_within_interval() {
        let router = ModelRouter::new();
        let monitor = ProactiveMonitor::new(router);
        let last_run = Utc::now() - chrono::Duration::minutes(1);
        assert!(!monitor.should_run(last_run, 5));
    }

    #[test]
    fn proactive_monitor_classifies_heavy_turn() {
        let router = ModelRouter::new();
        let monitor = ProactiveMonitor::new(router);
        let classification = monitor.classify_turn(
            "design the architecture for the system",
            "Created module structure with traits.",
        );
        assert_eq!(classification.complexity, TaskTier::Heavy);
        assert!(classification.success);
    }

    #[test]
    fn proactive_monitor_classifies_lite_turn() {
        let router = ModelRouter::new();
        let monitor = ProactiveMonitor::new(router);
        let classification = monitor.classify_turn("extract a skill from this", "Done.");
        assert_eq!(classification.complexity, TaskTier::Lite);
        assert!(classification.success);
    }

    #[test]
    fn proactive_monitor_detects_failure_in_response() {
        let router = ModelRouter::new();
        let monitor = ProactiveMonitor::new(router);
        let classification = monitor.classify_turn("write a function", "error: compilation failed");
        assert!(!classification.success);
    }

    #[test]
    fn proactive_monitor_extracts_debugging_learning() {
        let router = ModelRouter::new();
        let monitor = ProactiveMonitor::new(router);
        let conversation = vec![
            "run the build".to_string(),
            "error: undefined variable".to_string(),
            "fixed the variable reference".to_string(),
        ];
        let learning = monitor
            .extract_learning(&conversation)
            .expect("should extract");
        assert_eq!(learning.category, "debugging");
        assert!(learning.confidence > 0.0);
    }

    #[test]
    fn proactive_monitor_extracts_nothing_from_short_conversation() {
        let router = ModelRouter::new();
        let monitor = ProactiveMonitor::new(router);
        let conversation = vec!["hello".to_string()];
        assert!(monitor.extract_learning(&conversation).is_none());
    }

    #[test]
    fn proactive_monitor_suggests_test_skill() {
        let router = ModelRouter::new();
        let monitor = ProactiveMonitor::new(router);
        let conversation = vec![
            "run the tests".to_string(),
            "tests passed".to_string(),
            "run the tests again".to_string(),
        ];
        let suggestion = monitor
            .suggest_skill(&conversation)
            .expect("should suggest");
        assert_eq!(suggestion.name, "run-tests");
        assert!(!suggestion.trigger_patterns.is_empty());
    }

    #[test]
    fn proactive_monitor_suggests_nothing_for_short_conversation() {
        let router = ModelRouter::new();
        let monitor = ProactiveMonitor::new(router);
        let conversation = vec!["hello".to_string(), "world".to_string()];
        assert!(monitor.suggest_skill(&conversation).is_none());
    }

    #[test]
    fn task_tier_as_str_round_trips() {
        assert_eq!(TaskTier::Lite.as_str(), "lite");
        assert_eq!(TaskTier::Standard.as_str(), "standard");
        assert_eq!(TaskTier::Heavy.as_str(), "heavy");
        assert_eq!(TaskTier::Subagent.as_str(), "subagent");
    }
}
