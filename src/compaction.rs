//! Semantic context compaction: token estimation, compaction markers,
//! and oldest-first message removal that preserves the system prompt and
//! a recent token window.

use crate::provider::{Message, Provider, ProviderError, Role};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::path::Path;

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

/// Canonical bytes of the live system prefix. Prune must not change this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefixShape {
    pub digest: String,
    pub bytes: Vec<u8>,
    pub token_estimate: usize,
}

impl PrefixShape {
    pub fn from_messages(messages: &[Message]) -> Self {
        let system_end = system_prefix_end(messages);
        Self::from_prefix(&messages[..system_end])
    }

    pub fn from_prefix(prefix: &[Message]) -> Self {
        let bytes = serde_json::to_vec(prefix).unwrap_or_default();
        let digest = Sha256::digest(&bytes);
        Self {
            digest: digest.iter().map(|b| format!("{b:02x}")).collect(),
            bytes,
            token_estimate: estimate_messages(prefix),
        }
    }
}

/// Which projection step produced a [`ProjectionResult`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStep {
    None,
    Prune,
    Fold,
}

/// Projection of an append-only session: prune first, fold only if prune cannot fit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionResult {
    pub messages: Vec<Message>,
    pub archived: Vec<Message>,
    pub step: ProjectionStep,
    pub prefix: PrefixShape,
    pub remaining_tokens: usize,
    pub summary: String,
}

/// Verbatim JSONL archive of dropped turns (not a second summary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RavenArchive {
    pub lines: Vec<String>,
}

impl RavenArchive {
    pub fn from_turns(turns: &[Message]) -> Self {
        let lines = turns
            .iter()
            .map(|m| serde_json::to_string(m).unwrap_or_else(|_| "{}".to_string()))
            .collect();
        Self { lines }
    }

    pub fn to_jsonl(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    pub fn write_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(self.to_jsonl().as_bytes())
    }
}

fn system_prefix_end(messages: &[Message]) -> usize {
    messages
        .iter()
        .position(|m| m.role != Role::System)
        .unwrap_or(messages.len())
}

fn prune_window(messages: &[Message], config: &CompactionConfig) -> (usize, usize) {
    let system_end = system_prefix_end(messages);
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
    (system_end, tail_start.max(system_end))
}

/// Prune oldest non-prefix turns. Session itself stays append-only; this is a projection.
pub fn prune_messages(messages: &[Message], config: &CompactionConfig) -> ProjectionResult {
    let prefix = PrefixShape::from_messages(messages);
    let total = estimate_messages(messages);
    if total <= config.trigger_threshold {
        return ProjectionResult {
            messages: messages.to_vec(),
            archived: Vec::new(),
            step: ProjectionStep::None,
            prefix,
            remaining_tokens: total,
            summary: String::new(),
        };
    }
    let (system_end, removable_end) = prune_window(messages, config);
    let archived = messages[system_end..removable_end].to_vec();
    let step = if archived.is_empty() {
        ProjectionStep::None
    } else {
        ProjectionStep::Prune
    };
    let mut projected =
        Vec::with_capacity(system_end + messages.len().saturating_sub(removable_end));
    projected.extend_from_slice(&messages[..system_end]);
    projected.extend_from_slice(&messages[removable_end..]);
    let remaining_tokens = estimate_messages(&projected);
    ProjectionResult {
        messages: projected,
        archived,
        step,
        prefix,
        remaining_tokens,
        summary: String::new(),
    }
}

fn fold_projected(pruned: ProjectionResult, config: &CompactionConfig) -> ProjectionResult {
    if pruned.remaining_tokens <= config.trigger_threshold {
        return pruned;
    }
    let system_end = system_prefix_end(&pruned.messages);
    let foldable_end = pruned.messages.len().saturating_sub(1).max(system_end);
    if system_end >= pruned.messages.len() || foldable_end <= system_end {
        return pruned;
    }
    let folded = pruned.messages[system_end..foldable_end].to_vec();
    let summary = summarize_removed(&folded);
    let mut archived = pruned.archived;
    archived.extend(folded);
    let mut messages = Vec::with_capacity(system_end + 2);
    messages.extend_from_slice(&pruned.messages[..system_end]);
    if !summary.is_empty() {
        messages.push(Message::system(format!("[context compacted] {summary}")));
    }
    messages.extend_from_slice(&pruned.messages[foldable_end..]);
    let remaining_tokens = estimate_messages(&messages);
    ProjectionResult {
        messages,
        archived,
        step: ProjectionStep::Fold,
        prefix: pruned.prefix,
        remaining_tokens,
        summary,
    }
}

/// Prune first. Fold only when prune still cannot fit the trigger threshold.
pub fn project_compact(messages: &[Message], config: &CompactionConfig) -> ProjectionResult {
    let pruned = prune_messages(messages, config);
    if pruned.step == ProjectionStep::None || pruned.remaining_tokens <= config.trigger_threshold {
        return pruned;
    }
    fold_projected(pruned, config)
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

/// Apply compaction in-place: keep system prefix, insert a summary system note,
/// and retain the recent tail window. No-op when under the trigger threshold.
pub fn apply_compaction(
    messages: &mut Vec<Message>,
    config: &CompactionConfig,
) -> CompactionResult {
    let result = compact_messages(messages, config);
    if result.removed_count == 0 {
        return result;
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

    let tail: Vec<Message> = messages[tail_start..].to_vec();
    messages.truncate(system_end);
    if !result.summary.is_empty() {
        messages.push(Message::system(format!(
            "[context compacted] {} Markers preserved: {:?}",
            result.summary, result.markers_preserved
        )));
    }
    messages.extend(tail);
    result
}

pub async fn compact_messages_semantically(
    messages: &[Message],
    config: &CompactionConfig,
    provider: &dyn Provider,
    model: &str,
) -> Result<CompactionResult, ProviderError> {
    let mut result = compact_messages(messages, config);
    if result.removed_count == 0 {
        return Ok(result);
    }

    let system_end = messages
        .iter()
        .position(|m| m.role != Role::System)
        .unwrap_or(messages.len());
    let removed_end = system_end + result.removed_count;
    let system = Some(
        "Summarize the conversation into a continuation-grade checkpoint. Preserve the user's objective and corrections, decisions and rationale, files changed or inspected, commands and test results, failures, and unfinished work. Be concise, factual, and specific. Do not continue the task."
            .to_string(),
    );
    let transcript = messages[system_end..removed_end]
        .iter()
        .map(|message| {
            let tool_calls = message
                .tool_calls
                .iter()
                .map(|call| format!("\ntool call {} {}: {}", call.id, call.name, call.arguments))
                .collect::<String>();
            format!("{}: {}{}", message.role, message.content, tool_calls)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    result.summary = provider
        .generate(&[Message::user(transcript)], &system, model, &[])
        .await?;
    if result.summary.trim().is_empty() {
        return Err(ProviderError::Api(
            "compaction provider returned an empty summary".to_string(),
        ));
    }
    Ok(result)
}

pub(crate) fn apply_compaction_result(
    messages: &mut Vec<Message>,
    original: &[Message],
    result: &CompactionResult,
) -> bool {
    if result.removed_count == 0 {
        return false;
    }
    if !messages.starts_with(original) {
        return false;
    }

    let system_end = messages
        .iter()
        .position(|m| m.role != Role::System)
        .unwrap_or(messages.len());
    let removed_end = system_end + result.removed_count;
    messages.drain(system_end..removed_end);
    messages.insert(
        system_end,
        Message::system(format!(
            "[context compacted] {} Markers preserved: {:?}",
            result.summary, result.markers_preserved
        )),
    );
    true
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

    struct SummaryProvider {
        summary: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl Provider for SummaryProvider {
        fn id(&self) -> &str {
            "summary"
        }

        fn name(&self) -> &str {
            "Summary"
        }

        #[cfg(feature = "providers")]
        async fn stream(
            &self,
            _messages: &[Message],
            _system: &Option<String>,
            _model: &str,
            _tools: &[serde_json::Value],
            _reasoning_effort: Option<&str>,
        ) -> Result<crate::provider::StreamResult, ProviderError> {
            unreachable!()
        }

        async fn generate(
            &self,
            _messages: &[Message],
            _system: &Option<String>,
            _model: &str,
            _tools: &[serde_json::Value],
        ) -> Result<String, ProviderError> {
            self.summary
                .map(str::to_string)
                .ok_or_else(|| ProviderError::Api("summary failed".to_string()))
        }
    }

    #[test]
    fn estimate_tokens_three_chars_per_token() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcd"), 2);
        assert_eq!(estimate_tokens("abcdef"), 2);
        assert_eq!(estimate_tokens("abcdefg"), 3);
    }

    #[test]
    fn test_estimate_image_tokens_prevents_regressions() {
        let tokens = estimate_image_tokens();
        assert_eq!(
            tokens, IMAGE_TOKEN_COST,
            "Should return the constant IMAGE_TOKEN_COST"
        );
        assert_eq!(
            tokens, 1200,
            "Image token cost must remain exactly 1200 to prevent regressions"
        );
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
    fn apply_compaction_mutates_message_list() {
        let config = CompactionConfig::new(100, 30, 20);
        let mut messages = vec![
            Message::system("system prompt"),
            Message::user("old ".repeat(50)),
            Message::assistant("mid ".repeat(50)),
            Message::user("new ".repeat(50)),
        ];
        let before_len = messages.len();
        let result = apply_compaction(&mut messages, &config);
        assert!(result.removed_count > 0);
        assert!(messages.len() < before_len);
        assert_eq!(messages.first().unwrap().role, Role::System);
        assert!(messages
            .iter()
            .any(|m| m.content.contains("context compacted")));
    }

    #[tokio::test]
    async fn semantic_compaction_uses_provider_summary() {
        let config = CompactionConfig::new(100, 30, 20);
        let messages = vec![
            Message::system("system prompt"),
            Message::user("old objective ".repeat(50)),
            Message::assistant("old work ".repeat(50)),
            Message::user("recent tail"),
        ];
        let provider = SummaryProvider {
            summary: Some("Objective retained; tests still need to run."),
        };
        let result = compact_messages_semantically(&messages, &config, &provider, "test")
            .await
            .unwrap();
        assert_eq!(
            result.summary,
            "Objective retained; tests still need to run."
        );
    }

    #[tokio::test]
    async fn semantic_compaction_failure_leaves_messages_untouched() {
        let config = CompactionConfig::new(100, 30, 20);
        let messages = vec![
            Message::system("system prompt"),
            Message::user("old objective ".repeat(50)),
            Message::assistant("old work ".repeat(50)),
            Message::user("recent tail"),
        ];
        let original = messages.clone();
        let provider = SummaryProvider { summary: None };
        assert!(
            compact_messages_semantically(&messages, &config, &provider, "test")
                .await
                .is_err()
        );
        assert_eq!(messages, original);
    }

    #[test]
    fn applying_semantic_result_preserves_messages_appended_after_snapshot() {
        let config = CompactionConfig::new(100, 30, 20);
        let snapshot = vec![
            Message::system("system prompt"),
            Message::user("old objective ".repeat(50)),
            Message::assistant("old work ".repeat(50)),
            Message::user("recent tail"),
        ];
        let mut result = compact_messages(&snapshot, &config);
        result.summary = "checkpoint".to_string();
        let mut messages = snapshot.clone();
        messages.push(Message::user("appended while summarizing"));

        assert!(apply_compaction_result(&mut messages, &snapshot, &result));
        assert_eq!(
            messages.last().unwrap().content,
            "appended while summarizing"
        );
    }

    #[test]
    fn applying_semantic_result_rejects_divergent_prefix() {
        let config = CompactionConfig::new(100, 30, 20);
        let snapshot = vec![
            Message::system("system prompt"),
            Message::user("old objective ".repeat(50)),
            Message::assistant("old work ".repeat(50)),
            Message::user("recent tail"),
        ];
        let mut result = compact_messages(&snapshot, &config);
        result.summary = "checkpoint".to_string();
        let mut messages = snapshot.clone();
        messages[1] = Message::user("changed while summarizing");
        let divergent = messages.clone();

        assert!(!apply_compaction_result(&mut messages, &snapshot, &result));
        assert_eq!(messages, divergent);
    }

    #[test]
    fn compaction_removes_oldest_messages() {
        let config = CompactionConfig::new(100, 30, 20);
        let messages = vec![
            Message::system("system prompt"),
            Message::user("old ".repeat(50)),
            Message::assistant("mid ".repeat(50)),
            Message::user("new ".repeat(50)),
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
            Message::user("a".repeat(200)),
            Message::assistant("b".repeat(200)),
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
            Message::user("a".repeat(300)),
            Message::assistant("b".repeat(300)),
            Message::user("c".repeat(60)),
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
            Message::user("a".repeat(200)),
            Message::assistant("b".repeat(200)),
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

    fn oversized_transcript() -> Vec<Message> {
        vec![
            Message::system("important system prompt"),
            Message::user("old ".repeat(80)),
            Message::assistant("mid ".repeat(80)),
            Message::user("newer ".repeat(80)),
            Message::assistant("tail"),
        ]
    }

    #[test]
    fn prefix_bytes_stable_across_prune() {
        let config = CompactionConfig::new(80, 20, 15);
        let messages = oversized_transcript();
        let before = PrefixShape::from_messages(&messages);
        let pruned = prune_messages(&messages, &config);
        assert_eq!(pruned.step, ProjectionStep::Prune);
        assert!(!pruned.archived.is_empty());
        let after = PrefixShape::from_messages(&pruned.messages);
        assert_eq!(before.bytes, after.bytes);
        assert_eq!(before.digest, after.digest);
        assert_eq!(pruned.prefix.bytes, before.bytes);
    }

    #[test]
    fn fold_only_after_prune_fails_to_fit() {
        let config = CompactionConfig::new(40, 25, 90);
        let messages = vec![
            Message::system("important system prompt"),
            Message::user("old ".repeat(80)),
            Message::user("keep ".repeat(20)),
            Message::assistant("tail ".repeat(20)),
        ];
        let pruned_only = prune_messages(&messages, &config);
        assert_eq!(pruned_only.step, ProjectionStep::Prune);
        assert!(pruned_only.remaining_tokens > config.trigger_threshold);
        let result = project_compact(&messages, &config);
        assert_eq!(result.step, ProjectionStep::Fold);
        assert!(!result.summary.is_empty());
    }

    #[test]
    fn project_compact_stops_at_prune_when_it_fits() {
        let config = CompactionConfig::new(200, 40, 20);
        let messages = oversized_transcript();
        let result = project_compact(&messages, &config);
        assert_eq!(result.step, ProjectionStep::Prune);
        assert!(result.summary.is_empty());
        assert!(result.remaining_tokens <= config.trigger_threshold);
    }

    #[test]
    fn raven_archive_is_verbatim_jsonl_not_a_summary() {
        let dropped = vec![
            Message::user("first dropped"),
            Message::assistant("second dropped"),
        ];
        let archive = RavenArchive::from_turns(&dropped);
        assert_eq!(archive.lines.len(), 2);
        assert!(archive.lines[0].contains("first dropped"));
        assert!(!archive.to_jsonl().contains("context compacted"));
        let replayed: Message = serde_json::from_str(&archive.lines[0]).unwrap();
        assert_eq!(replayed, dropped[0]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raven.jsonl");
        archive.write_to(&path).unwrap();
        archive.write_to(&path).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk.lines().count(), 4);
    }
}
