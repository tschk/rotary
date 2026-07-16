//! Prompt caching support for Anthropic and OpenAI providers.
//!
//! Anthropic supports explicit cache breakpoints via `cache_control` blocks
//! on message content. OpenAI performs automatic prefix caching with no
//! client-side markers. This module provides configuration, message
//! mutation, and statistics tracking for both strategies.

use serde::{Deserialize, Serialize};

/// Which provider's caching strategy is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheProvider {
    /// Anthropic: explicit `cache_control` blocks on content.
    Anthropic,
    /// OpenAI: automatic prefix caching (no client markers needed).
    OpenAI,
    /// No caching.
    #[default]
    None,
}

/// Cache lifetime. Anthropic supports a 1h TTL in addition to the default 5m.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheTtl {
    /// Default ephemeral cache (5 minutes).
    #[default]
    FiveMinutes,
    /// Extended ephemeral cache (1 hour). Anthropic only.
    OneHour,
}

/// Where to insert a cache breakpoint within the message sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CachePosition {
    /// Cache the system prompt.
    SystemPrompt,
    /// Cache up to the tool definitions.
    BeforeTools,
    /// Cache everything except the last user message.
    BeforeLastMessage,
    /// Cache at a specific message index.
    Custom { message_index: usize },
}

/// A single cache breakpoint placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePoint {
    pub position: CachePosition,
    pub ttl: Option<CacheTtl>,
}

impl CachePoint {
    pub fn new(position: CachePosition) -> Self {
        Self {
            position,
            ttl: None,
        }
    }

    pub fn with_ttl(mut self, ttl: CacheTtl) -> Self {
        self.ttl = Some(ttl);
        self
    }
}

/// Low-level cache control descriptor. This mirrors the fields users may
/// set directly when they want fine-grained control over breakpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheControl {
    pub enabled: bool,
    pub provider: CacheProvider,
    pub cache_points: Vec<CachePoint>,
}

impl Default for CacheControl {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: CacheProvider::None,
            cache_points: Vec::new(),
        }
    }
}

/// Configuration for prompt caching. Defaults are tuned for Anthropic, which
/// benefits most from explicit breakpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCacheConfig {
    pub enabled: bool,
    pub provider: CacheProvider,
    /// Cache the system prompt (default true).
    pub cache_system_prompt: bool,
    /// Cache up to the tool definitions (default true).
    pub cache_tools: bool,
    /// Cache conversation history (default true).
    pub cache_history: bool,
    /// Cache TTL (default 5 minutes).
    pub ttl: CacheTtl,
}

impl Default for PromptCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: CacheProvider::Anthropic,
            cache_system_prompt: true,
            cache_tools: true,
            cache_history: true,
            ttl: CacheTtl::FiveMinutes,
        }
    }
}

impl PromptCacheConfig {
    pub fn anthropic() -> Self {
        Self::default()
    }

    pub fn openai() -> Self {
        Self {
            enabled: true,
            provider: CacheProvider::OpenAI,
            cache_system_prompt: true,
            cache_tools: true,
            cache_history: true,
            ttl: CacheTtl::FiveMinutes,
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            provider: CacheProvider::None,
            cache_system_prompt: false,
            cache_tools: false,
            cache_history: false,
            ttl: CacheTtl::FiveMinutes,
        }
    }
}

/// Build the `cache_control` JSON object for an Anthropic content block.
fn cache_control_object(ttl: CacheTtl) -> serde_json::Value {
    match ttl {
        CacheTtl::FiveMinutes => serde_json::json!({"type": "ephemeral"}),
        CacheTtl::OneHour => serde_json::json!({"type": "ephemeral", "ttl": "1h"}),
    }
}

/// Ensure a message's `content` field is an array of content blocks,
/// converting a string content into a single text block. Returns a mutable
/// reference to the content array.
fn ensure_content_array(message: &mut serde_json::Value) -> Option<&mut Vec<serde_json::Value>> {
    if let Some(content) = message.get_mut("content") {
        if content.is_string() {
            let text = content.as_str().unwrap_or_default().to_string();
            *content = serde_json::json!([{"type": "text", "text": text}]);
        }
        if let Some(arr) = content.as_array_mut() {
            return Some(arr);
        }
    }
    None
}

/// Add a `cache_control` marker to the last content block of `message`.
fn mark_message(message: &mut serde_json::Value, ttl: CacheTtl) {
    if let Some(blocks) = ensure_content_array(message) {
        if let Some(last) = blocks.last_mut() {
            last["cache_control"] = cache_control_object(ttl);
        }
    }
}

/// Find the index of the first message with `role` == `role_str`.
fn find_role(messages: &[serde_json::Value], role_str: &str) -> Option<usize> {
    messages
        .iter()
        .position(|m| m.get("role").and_then(|r| r.as_str()) == Some(role_str))
}

/// Modify the message JSON to add `cache_control` markers according to
/// `config`.
///
/// For Anthropic, this adds `"cache_control": {"type": "ephemeral"}` (or
/// the 1h variant) to the last content block of cached messages. For
/// OpenAI and `None` providers this is a no-op: OpenAI performs automatic
/// prefix caching and needs no client-side markers.
pub fn apply_cache_control(messages: &mut [serde_json::Value], config: &PromptCacheConfig) {
    if !config.enabled {
        return;
    }
    if config.provider != CacheProvider::Anthropic {
        return;
    }
    if messages.is_empty() {
        return;
    }

    let ttl = config.ttl;
    let mut marked: Vec<usize> = Vec::new();

    if config.cache_system_prompt {
        if let Some(idx) = find_role(messages, "system") {
            marked.push(idx);
        }
    }

    if config.cache_tools {
        // Tools are injected after the message history; cache the last
        // non-tool-result message so the prefix up to the tool definitions
        // is cached.
        if let Some(idx) = last_non_tool_message(messages) {
            if !marked.contains(&idx) {
                marked.push(idx);
            }
        }
    }

    if config.cache_history {
        // Cache everything except the final user message by marking the
        // second-to-last message.
        if messages.len() > 1 {
            let idx = messages.len() - 2;
            if !marked.contains(&idx) {
                marked.push(idx);
            }
        }
    }

    for idx in marked {
        if idx < messages.len() {
            mark_message(&mut messages[idx], ttl);
        }
    }
}

/// Index of the last message whose role is not `tool`.
fn last_non_tool_message(messages: &[serde_json::Value]) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, m)| m.get("role").and_then(|r| r.as_str()) != Some("tool"))
        .map(|(i, _)| i)
}

/// Snapshot of cache statistics for a single reporting period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheStats {
    pub cache_read_tokens: usize,
    pub cache_write_tokens: usize,
    pub cache_hit_rate: f64,
    pub savings: f64,
}

impl Default for CacheStats {
    fn default() -> Self {
        Self {
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_hit_rate: 0.0,
            savings: 0.0,
        }
    }
}

/// Tracks cache statistics across multiple API calls.
#[derive(Debug, Clone, Default)]
pub struct CacheStatsTracker {
    total_input_tokens: usize,
    total_cache_read_tokens: usize,
    total_cache_write_tokens: usize,
    call_count: usize,
}

impl CacheStatsTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record cache token usage from an API response `usage` object.
    ///
    /// Supports both Anthropic (`cache_read_input_tokens` /
    /// `cache_creation_input_tokens`) and OpenAI
    /// (`prompt_tokens_details.cached_tokens`) field layouts.
    pub fn record(&mut self, usage: &serde_json::Value) {
        self.call_count += 1;

        let input_tokens = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| usage.get("prompt_tokens").and_then(|v| v.as_u64()))
            .unwrap_or(0) as usize;
        self.total_input_tokens += input_tokens;

        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(0) as usize;
        self.total_cache_read_tokens += cache_read;

        let cache_write = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        self.total_cache_write_tokens += cache_write;
    }

    /// Compute the current cache statistics snapshot.
    pub fn stats(&self) -> CacheStats {
        let cache_hit_rate = if self.total_input_tokens > 0 {
            self.total_cache_read_tokens as f64 / self.total_input_tokens as f64
        } else {
            0.0
        };

        // Savings: cached reads are ~10x cheaper than full input on
        // Anthropic. We approximate savings as the fraction of input
        // tokens served from cache, scaled by the 0.9 cost discount.
        let savings = if self.total_input_tokens > 0 {
            (self.total_cache_read_tokens as f64 / self.total_input_tokens as f64) * 0.9
        } else {
            0.0
        };

        CacheStats {
            cache_read_tokens: self.total_cache_read_tokens,
            cache_write_tokens: self.total_cache_write_tokens,
            cache_hit_rate,
            savings,
        }
    }

    /// Reset all accumulated statistics.
    pub fn reset(&mut self) {
        self.total_input_tokens = 0;
        self.total_cache_read_tokens = 0;
        self.total_cache_write_tokens = 0;
        self.call_count = 0;
    }

    /// Number of API calls recorded.
    pub fn call_count(&self) -> usize {
        self.call_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(content: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": content})
    }

    fn assistant_msg(content: &str) -> serde_json::Value {
        serde_json::json!({"role": "assistant", "content": content})
    }

    fn system_msg(content: &str) -> serde_json::Value {
        serde_json::json!({"role": "system", "content": content})
    }

    fn has_cache_control(message: &serde_json::Value) -> bool {
        if let Some(content) = message.get("content").and_then(|c| c.as_array()) {
            content
                .last()
                .and_then(|block| block.get("cache_control"))
                .is_some()
        } else {
            false
        }
    }

    #[test]
    fn test_apply_cache_control_anthropic_system_prompt() {
        let mut messages = vec![
            system_msg("You are a helpful assistant."),
            user_msg("Hello"),
        ];
        let config = PromptCacheConfig {
            enabled: true,
            provider: CacheProvider::Anthropic,
            cache_system_prompt: true,
            cache_tools: false,
            cache_history: false,
            ttl: CacheTtl::FiveMinutes,
        };
        apply_cache_control(&mut messages, &config);
        assert!(has_cache_control(&messages[0]));
        assert!(!has_cache_control(&messages[1]));
    }

    #[test]
    fn test_apply_cache_control_anthropic_before_tools() {
        let mut messages = vec![
            system_msg("System"),
            user_msg("First question"),
            assistant_msg("Answer"),
            user_msg("Second question"),
        ];
        let config = PromptCacheConfig {
            enabled: true,
            provider: CacheProvider::Anthropic,
            cache_system_prompt: false,
            cache_tools: true,
            cache_history: false,
            ttl: CacheTtl::FiveMinutes,
        };
        apply_cache_control(&mut messages, &config);
        let marked: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| has_cache_control(m))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(marked, vec![3]);
    }

    #[test]
    fn test_apply_cache_control_anthropic_before_last_message() {
        let mut messages = vec![
            system_msg("System"),
            user_msg("First"),
            assistant_msg("Reply"),
            user_msg("Latest question"),
        ];
        let config = PromptCacheConfig {
            enabled: true,
            provider: CacheProvider::Anthropic,
            cache_system_prompt: false,
            cache_tools: false,
            cache_history: true,
            ttl: CacheTtl::FiveMinutes,
        };
        apply_cache_control(&mut messages, &config);
        assert!(!has_cache_control(&messages[3]));
        assert!(has_cache_control(&messages[2]));
    }

    #[test]
    fn test_apply_cache_control_openai_is_noop() {
        let mut messages = vec![system_msg("You are helpful."), user_msg("Hello")];
        let config = PromptCacheConfig::openai();
        apply_cache_control(&mut messages, &config);
        for m in &messages {
            assert!(!has_cache_control(m));
        }
    }

    #[test]
    fn test_apply_cache_control_disabled_is_noop() {
        let mut messages = vec![system_msg("You are helpful."), user_msg("Hello")];
        let config = PromptCacheConfig::disabled();
        apply_cache_control(&mut messages, &config);
        for m in &messages {
            assert!(!has_cache_control(m));
        }
    }

    #[test]
    fn test_apply_cache_control_none_provider_is_noop() {
        let mut messages = vec![system_msg("System"), user_msg("Hello")];
        let config = PromptCacheConfig {
            enabled: true,
            provider: CacheProvider::None,
            cache_system_prompt: true,
            cache_tools: true,
            cache_history: true,
            ttl: CacheTtl::FiveMinutes,
        };
        apply_cache_control(&mut messages, &config);
        for m in &messages {
            assert!(!has_cache_control(m));
        }
    }

    #[test]
    fn test_apply_cache_control_one_hour_ttl() {
        let mut messages = vec![system_msg("System"), user_msg("Hello")];
        let config = PromptCacheConfig {
            enabled: true,
            provider: CacheProvider::Anthropic,
            cache_system_prompt: true,
            cache_tools: false,
            cache_history: false,
            ttl: CacheTtl::OneHour,
        };
        apply_cache_control(&mut messages, &config);
        let block = messages[0]["content"].as_array().unwrap().last().unwrap();
        let cc = block.get("cache_control").unwrap();
        assert_eq!(cc["type"], "ephemeral");
        assert_eq!(cc["ttl"], "1h");
    }

    #[test]
    fn test_apply_cache_control_empty_messages() {
        let mut messages: Vec<serde_json::Value> = Vec::new();
        let config = PromptCacheConfig::anthropic();
        apply_cache_control(&mut messages, &config);
        assert!(messages.is_empty());
    }

    #[test]
    fn test_apply_cache_control_string_content_converted() {
        let mut messages = vec![system_msg("You are helpful."), user_msg("Hello")];
        let config = PromptCacheConfig::anthropic();
        apply_cache_control(&mut messages, &config);
        let content = messages[0].get("content").unwrap();
        assert!(content.is_array(), "content should be array form");
        let block = content.as_array().unwrap().last().unwrap();
        assert_eq!(block["type"], "text");
        assert_eq!(block["text"], "You are helpful.");
        assert!(block.get("cache_control").is_some());
    }

    #[test]
    fn test_cache_stats_tracker_anthropic_response() {
        let mut tracker = CacheStatsTracker::new();
        let usage = serde_json::json!({
            "input_tokens": 1000,
            "cache_read_input_tokens": 800,
            "cache_creation_input_tokens": 200,
            "output_tokens": 500
        });
        tracker.record(&usage);
        let stats = tracker.stats();
        assert_eq!(stats.cache_read_tokens, 800);
        assert_eq!(stats.cache_write_tokens, 200);
        assert!((stats.cache_hit_rate - 0.8).abs() < 1e-9);
    }

    #[test]
    fn test_cache_stats_tracker_openai_response() {
        let mut tracker = CacheStatsTracker::new();
        let usage = serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 500,
            "prompt_tokens_details": {
                "cached_tokens": 600
            }
        });
        tracker.record(&usage);
        let stats = tracker.stats();
        assert_eq!(stats.cache_read_tokens, 600);
        assert_eq!(stats.cache_write_tokens, 0);
        assert!((stats.cache_hit_rate - 0.6).abs() < 1e-9);
    }

    #[test]
    fn test_cache_stats_tracker_multiple_calls() {
        let mut tracker = CacheStatsTracker::new();
        tracker.record(&serde_json::json!({
            "input_tokens": 1000,
            "cache_read_input_tokens": 500,
            "cache_creation_input_tokens": 500
        }));
        tracker.record(&serde_json::json!({
            "input_tokens": 1000,
            "cache_read_input_tokens": 900,
            "cache_creation_input_tokens": 0
        }));
        let stats = tracker.stats();
        assert_eq!(stats.cache_read_tokens, 1400);
        assert_eq!(stats.cache_write_tokens, 500);
        assert_eq!(tracker.call_count(), 2);
        assert!((stats.cache_hit_rate - 0.7).abs() < 1e-9);
    }

    #[test]
    fn test_cache_stats_tracker_reset() {
        let mut tracker = CacheStatsTracker::new();
        tracker.record(&serde_json::json!({
            "input_tokens": 1000,
            "cache_read_input_tokens": 800,
            "cache_creation_input_tokens": 200
        }));
        tracker.reset();
        let stats = tracker.stats();
        assert_eq!(stats.cache_read_tokens, 0);
        assert_eq!(stats.cache_write_tokens, 0);
        assert_eq!(stats.cache_hit_rate, 0.0);
        assert_eq!(tracker.call_count(), 0);
    }

    #[test]
    fn test_cache_stats_tracker_empty_usage() {
        let mut tracker = CacheStatsTracker::new();
        tracker.record(&serde_json::json!({}));
        let stats = tracker.stats();
        assert_eq!(stats.cache_read_tokens, 0);
        assert_eq!(stats.cache_hit_rate, 0.0);
    }

    #[test]
    fn test_cache_hit_rate_no_input_tokens() {
        let tracker = CacheStatsTracker::new();
        let stats = tracker.stats();
        assert_eq!(stats.cache_hit_rate, 0.0);
        assert_eq!(stats.savings, 0.0);
    }

    #[test]
    fn test_cache_point_builder() {
        let point = CachePoint::new(CachePosition::SystemPrompt).with_ttl(CacheTtl::OneHour);
        assert_eq!(point.position, CachePosition::SystemPrompt);
        assert_eq!(point.ttl, Some(CacheTtl::OneHour));
    }

    #[test]
    fn test_cache_position_custom() {
        let mut messages = vec![
            system_msg("System"),
            user_msg("A"),
            assistant_msg("B"),
            user_msg("C"),
        ];
        let config = PromptCacheConfig {
            enabled: true,
            provider: CacheProvider::Anthropic,
            cache_system_prompt: false,
            cache_tools: false,
            cache_history: false,
            ttl: CacheTtl::FiveMinutes,
        };
        apply_cache_control(&mut messages, &config);
        // Custom position is not wired into PromptCacheConfig; verify
        // no markers are added when all flags are off.
        for m in &messages {
            assert!(!has_cache_control(m));
        }
    }

    #[test]
    fn test_default_config_is_anthropic_enabled() {
        let config = PromptCacheConfig::default();
        assert!(config.enabled);
        assert_eq!(config.provider, CacheProvider::Anthropic);
        assert!(config.cache_system_prompt);
        assert!(config.cache_tools);
        assert!(config.cache_history);
        assert_eq!(config.ttl, CacheTtl::FiveMinutes);
    }

    #[test]
    fn test_cache_control_default() {
        let cc = CacheControl::default();
        assert!(!cc.enabled);
        assert_eq!(cc.provider, CacheProvider::None);
        assert!(cc.cache_points.is_empty());
    }
}
