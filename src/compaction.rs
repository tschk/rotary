//! Semantic context compaction: token estimation, compaction markers,
//! and oldest-first message removal that preserves the system prompt and
//! a recent token window.

use crate::provider::{Message, Role};
use serde::{Deserialize, Serialize};

/// Heuristic token estimate: ~3 characters per token.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(3)
}

/// Fixed token cost attributed to a single image attachment.
pub const IMAGE_TOKEN_COST: usize = 1200;

pub fn estimate_image_tokens() -> usize {
    IMAGE_TOKEN_COST
}

/// Estimate the token cost of a message slice using zero-allocation
/// JSON byte counting where possible.
///
/// Each message contributes its role label, content, and (when present)
/// its `tool_call_id`. We approximate the serialized JSON byte length by
/// summing the raw field lengths plus a small fixed overhead for the
/// structural JSON characters (`{"role":"","content":""}` and the
/// optional `,"tool_call_id":""`), avoiding a full `serde_json::to_string`
/// allocation per message.
pub fn estimate_messages(messages: &[Message]) -> usize {
    const STRUCTURAL_OVERHEAD: usize = 22;
    const TOOL_CALL_OVERHEAD: usize = 18;
    let mut bytes: usize = 0;
    for m in messages {
        bytes += m.role.to_string().len();
        bytes += m.content.len();
        if let Some(tid) = &m.tool_call_id {
            bytes += tid.len() + TOOL_CALL_OVERHEAD;
        }
        bytes += STRUCTURAL_OVERHEAD;
    }
    bytes.div_ceil(3)
}

/// Severity of a [`CompactionMarker`]: critical markers must survive
/// compaction, important markers should survive when possible, and
/// informational markers may be dropped freely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Critical,
    Important,
    Informational,
}

/// Semantic markers attached to compacted content. Each variant carries a
/// [`Severity`] that guides what the compactor tries to preserve.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CompactionMarker {
    Task,
    FileReference,
    Decision,
    ToolOutput,
    UserCorrection,
    SystemNote,
}

impl CompactionMarker {
    pub fn severity(&self) -> Severity {
        match self {
            Self::Task => Severity::Critical,
            Self::UserCorrection => Severity::Critical,
            Self::Decision => Severity::Important,
            Self::FileReference => Severity::Important,
            Self::ToolOutput => Severity::Informational,
            Self::SystemNote => Severity::Informational,
        }
    }
}

/// Configuration for [`compact_messages`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    pub context_window: usize,
    pub reserve: usize,
    pub keep_recent: usize,
    pub trigger_threshold: usize,
}

impl CompactionConfig {
    pub const DEFAULT_CONTEXT_WINDOW: usize = 128_000;
    pub const DEFAULT_RESERVE: usize = 10_240;
    pub const DEFAULT_KEEP_RECENT: usize = 12_800;

    pub fn new(context_window: usize, reserve: usize, keep_recent: usize) -> Self {
        Self {
            context_window,
            reserve,
            keep_recent,
            trigger_threshold: context_window.saturating_sub(reserve),
        }
    }
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self::new(
            Self::DEFAULT_CONTEXT_WINDOW,
            Self::DEFAULT_RESERVE,
            Self::DEFAULT_KEEP_RECENT,
        )
    }
}

/// Result of a [`compact_messages`] call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionResult {
    pub summary: String,
    pub removed_count: usize,
    pub removed_tokens: usize,
    pub remaining_tokens: usize,
    pub markers_preserved: Vec<CompactionMarker>,
}

/// Compact a message slice by removing the oldest messages first,
/// preserving the leading system prompt and a trailing window of
/// `keep_recent` tokens.
///
/// If the estimated token count is below the configured trigger threshold,
/// no compaction is performed and a no-op result is returned.
pub fn compact_messages(messages: &[Message], config: &CompactionConfig) -> CompactionResult {
    let total = estimate_messages(messages);
    if total <= config.trigger_threshold {
        return CompactionResult {
            summary: String::new(),
            removed_count: 0,
            removed_tokens: 0,
            remaining_tokens: total,
            markers_preserved: Vec::new(),
        };
    }

    let system_end = messages
        .iter()
        .position(|m| m.role != Role::System)
        .unwrap_or(messages.len());

    let mut preserved_tokens = 0usize;
    let mut tail_start = messages.len();
    for i in (system_end..messages.len()).rev() {
        let cost = estimate_messages(std::slice::from_ref(&messages[i]));
        if preserved_tokens + cost > config.keep_recent {
            break;
        }
        preserved_tokens += cost;
        tail_start = i;
    }

    let removable_end = tail_start.max(system_end);
    let removed = &messages[system_end..removable_end];
    let removed_tokens = estimate_messages(removed);

    let summary = summarize_removed(removed);
    let remaining_tokens = total.saturating_sub(removed_tokens);

    let mut markers_preserved = Vec::new();
    for m in &messages[..system_end] {
        collect_markers(&m.content, &mut markers_preserved);
    }
    for m in &messages[tail_start..] {
        collect_markers(&m.content, &mut markers_preserved);
    }
    markers_preserved.sort();
    markers_preserved.dedup();

    CompactionResult {
        summary,
        removed_count: removed.len(),
        removed_tokens,
        remaining_tokens,
        markers_preserved,
    }
}

fn summarize_removed(removed: &[Message]) -> String {
    if removed.is_empty() {
        return String::new();
    }
    let mut user_turns = 0usize;
    let mut assistant_turns = 0usize;
    let mut tool_turns = 0usize;
    let mut chars: usize = 0;
    for m in removed {
        chars += m.content.len();
        match m.role {
            Role::User => user_turns += 1,
            Role::Assistant => assistant_turns += 1,
            Role::Tool => tool_turns += 1,
            Role::System => {}
        }
    }
    format!(
        "Compacted {} messages ({} user, {} assistant, {} tool, ~{} chars).",
        removed.len(),
        user_turns,
        assistant_turns,
        tool_turns,
        chars,
    )
}

fn collect_markers(content: &str, out: &mut Vec<CompactionMarker>) {
    let lower = content.to_ascii_lowercase();
    if lower.contains("task:") || lower.contains("objective:") {
        out.push(CompactionMarker::Task);
    }
    if lower.contains(".rs") || lower.contains("file:") || lower.contains("path:") {
        out.push(CompactionMarker::FileReference);
    }
    if lower.contains("decided") || lower.contains("decision:") {
        out.push(CompactionMarker::Decision);
    }
    if lower.contains("tool output") || lower.contains("tool_result") {
        out.push(CompactionMarker::ToolOutput);
    }
    if lower.contains("correction") || lower.contains("actually,") {
        out.push(CompactionMarker::UserCorrection);
    }
    if lower.contains("system note") {
        out.push(CompactionMarker::SystemNote);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Message;

    #[test]
    fn estimate_tokens_three_chars_per_token() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcd"), 2);
        assert_eq!(estimate_tokens("abcdef"), 2);
        assert_eq!(estimate_tokens("abcdefg"), 3);
    }

    #[test]
    fn estimate_image_tokens_is_fixed() {
        assert_eq!(estimate_image_tokens(), 1200);
        assert_eq!(estimate_image_tokens(), IMAGE_TOKEN_COST);
    }

    #[test]
    fn estimate_messages_grows_with_content() {
        let one = vec![Message::user("hello world")];
        let two = vec![Message::user("hello world"), Message::assistant("bye")];
        assert!(estimate_messages(&two) > estimate_messages(&one));
        assert!(estimate_messages(&one) > 0);
    }

    #[test]
    fn estimate_messages_includes_tool_call_id() {
        let plain = vec![Message::user("hello")];
        let with_tool = vec![Message::tool("call_1", "hello")];
        assert!(estimate_messages(&with_tool) > estimate_messages(&plain));
    }

    #[test]
    fn no_compaction_under_threshold() {
        let config = CompactionConfig::new(1_000, 100, 200);
        let messages = vec![
            Message::system("system prompt"),
            Message::user("short message"),
        ];
        let result = compact_messages(&messages, &config);
        assert_eq!(result.removed_count, 0);
        assert_eq!(result.removed_tokens, 0);
        assert_eq!(result.remaining_tokens, estimate_messages(&messages));
        assert!(result.summary.is_empty());
    }

    #[test]
    fn compaction_removes_oldest_messages() {
        let config = CompactionConfig::new(100, 30, 20);
        let messages = vec![
            Message::system("system prompt"),
            Message::user(&"old ".repeat(50)),
            Message::assistant(&"mid ".repeat(50)),
            Message::user(&"new ".repeat(50)),
        ];
        let result = compact_messages(&messages, &config);
        assert!(result.removed_count > 0);
        assert!(result.removed_tokens > 0);
        assert!(result.remaining_tokens < estimate_messages(&messages));
        assert!(!result.summary.is_empty());
    }

    #[test]
    fn system_prompt_is_preserved() {
        let config = CompactionConfig::new(100, 30, 20);
        let system_content = "important system prompt";
        let messages = vec![
            Message::system(system_content),
            Message::user(&"a".repeat(200)),
            Message::assistant(&"b".repeat(200)),
            Message::user("recent"),
        ];
        let result = compact_messages(&messages, &config);
        assert!(result.removed_count > 0);
        assert!(result.remaining_tokens >= estimate_tokens(system_content));
    }

    #[test]
    fn keep_recent_is_respected() {
        let config = CompactionConfig::new(200, 60, 30);
        let messages = vec![
            Message::system("sys"),
            Message::user(&"a".repeat(300)),
            Message::assistant(&"b".repeat(300)),
            Message::user(&"c".repeat(60)),
        ];
        let result = compact_messages(&messages, &config);
        let total = estimate_messages(&messages);
        assert!(
            result.remaining_tokens <= total,
            "remaining {} should not exceed total {}",
            result.remaining_tokens,
            total
        );
        assert!(
            result.remaining_tokens
                <= estimate_messages(std::slice::from_ref(&messages[0])) + config.keep_recent,
            "remaining {} should not exceed system + keep_recent {}",
            result.remaining_tokens,
            config.keep_recent
        );
    }

    #[test]
    fn trigger_threshold_is_context_minus_reserve() {
        let config = CompactionConfig::new(128_000, 10_240, 12_800);
        assert_eq!(config.trigger_threshold, 128_000 - 10_240);
    }

    #[test]
    fn default_config_matches_spec() {
        let config = CompactionConfig::default();
        assert_eq!(config.context_window, 128_000);
        assert_eq!(config.reserve, 10_240);
        assert_eq!(config.keep_recent, 12_800);
        assert_eq!(config.trigger_threshold, 128_000 - 10_240);
    }

    #[test]
    fn marker_severity_classification() {
        assert_eq!(CompactionMarker::Task.severity(), Severity::Critical);
        assert_eq!(
            CompactionMarker::UserCorrection.severity(),
            Severity::Critical
        );
        assert_eq!(CompactionMarker::Decision.severity(), Severity::Important);
        assert_eq!(
            CompactionMarker::FileReference.severity(),
            Severity::Important
        );
        assert_eq!(
            CompactionMarker::ToolOutput.severity(),
            Severity::Informational
        );
        assert_eq!(
            CompactionMarker::SystemNote.severity(),
            Severity::Informational
        );
    }

    #[test]
    fn markers_collected_from_preserved_messages() {
        let config = CompactionConfig::new(100, 30, 20);
        let messages = vec![
            Message::system("Task: do the thing"),
            Message::user(&"a".repeat(200)),
            Message::assistant(&"b".repeat(200)),
            Message::user("Decision: keep it simple"),
        ];
        let result = compact_messages(&messages, &config);
        assert!(result.markers_preserved.contains(&CompactionMarker::Task));
        assert!(result
            .markers_preserved
            .contains(&CompactionMarker::Decision));
    }

    #[test]
    fn compaction_result_serializes() {
        let result = CompactionResult {
            summary: "test".to_string(),
            removed_count: 1,
            removed_tokens: 10,
            remaining_tokens: 20,
            markers_preserved: vec![CompactionMarker::Task],
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: CompactionResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, result);
    }
}
