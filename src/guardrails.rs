//! Guardrails: empty-turn detection, repeated tool failure, loop hygiene,
//! bounded tool recursion, abort signaling, and tool effect batching.

use crate::agent::{ToolEffect, ToolResult};
use std::collections::HashMap;
use std::fmt;
#[cfg(feature = "ipc")]
use std::future::Future;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};

/// Tool classification — idempotent (read-only, safe to retry) vs mutating
/// (has side effects, dangerous to repeat blindly). Inspired by Unthinkclaw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    Idempotent,
    Mutating,
}

impl fmt::Display for ToolClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolClass::Idempotent => write!(f, "idempotent"),
            ToolClass::Mutating => write!(f, "mutating"),
        }
    }
}

/// Classify a tool by name. Unknown tools default to Mutating (safe default).
pub fn classify_tool(name: &str) -> ToolClass {
    const IDEMPOTENT: &[&str] = &[
        "read",
        "grep",
        "glob",
        "search",
        "list",
        "stat",
        "web_search",
        "web_fetch",
        "session_status",
        "memory_search",
    ];
    const MUTATING: &[&str] = &[
        "write", "edit", "shell", "exec", "bash", "delete", "rename", "move", "mkdir", "rmdir",
    ];
    if IDEMPOTENT.contains(&name) {
        ToolClass::Idempotent
    } else {
        // Explicitly-known mutating tools and unknown tools both classify as
        // Mutating (safe default).
        let _ = MUTATING.contains(&name);
        ToolClass::Mutating
    }
}

/// Guardrail configuration thresholds. Inspired by Unthinkclaw's GuardrailConfig.
#[derive(Debug, Clone)]
pub struct GuardrailConfig {
    pub warnings_enabled: bool,
    pub hard_stop_enabled: bool,
    pub exact_failure_warn_after: usize,
    pub exact_failure_block_after: usize,
    pub same_tool_failure_warn_after: usize,
    pub same_tool_failure_halt_after: usize,
    pub no_progress_warn_after: usize,
    pub no_progress_block_after: usize,
}

impl Default for GuardrailConfig {
    fn default() -> Self {
        Self {
            warnings_enabled: true,
            hard_stop_enabled: false,
            exact_failure_warn_after: 2,
            exact_failure_block_after: 5,
            same_tool_failure_warn_after: 3,
            same_tool_failure_halt_after: 8,
            no_progress_warn_after: 2,
            no_progress_block_after: 5,
        }
    }
}

/// Decision returned by the guardrail after observing a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardrailDecision {
    Proceed,
    Warn(String),
    Stop(String),
}

/// Tool call guardrails — tracks per-turn observations and returns decisions.
/// Inspired by Unthinkclaw's ToolGuardrails.
pub struct ToolGuardrails {
    pub config: GuardrailConfig,
    pub call_history: Vec<(String, String)>,
    pub error_counts: HashMap<String, usize>,
    pub identical_call_count: usize,
    pub last_call: Option<(String, String)>,
}

impl ToolGuardrails {
    pub fn new(config: GuardrailConfig) -> Self {
        Self {
            config,
            call_history: Vec::with_capacity(64),
            error_counts: HashMap::new(),
            identical_call_count: 0,
            last_call: None,
        }
    }

    /// Observe a tool call and return the guardrail decision.
    pub fn observe(&mut self, tool_name: &str, args: &str, is_error: bool) -> GuardrailDecision {
        let hash = hash_args(args);
        let identity = (tool_name.to_string(), hash.clone());

        if Some(&identity) == self.last_call.as_ref() {
            self.identical_call_count += 1;
        } else {
            self.identical_call_count = 0;
            self.last_call = Some(identity);
        }

        if self.identical_call_count >= self.config.same_tool_failure_halt_after
            && self.config.hard_stop_enabled
        {
            return GuardrailDecision::Stop(format!(
                "Same tool call '{}' repeated {} times — loop detected. Breaking.",
                tool_name,
                self.identical_call_count + 1
            ));
        }
        if self.config.warnings_enabled
            && self.identical_call_count >= self.config.same_tool_failure_warn_after
        {
            return GuardrailDecision::Warn(format!(
                "WARNING: You're calling '{}' identically ({}x). Stop and answer with what you have.",
                tool_name,
                self.identical_call_count + 1
            ));
        }

        if is_error {
            let count = self.error_counts.entry(tool_name.to_string()).or_insert(0);
            *count += 1;
            let error_count = *count;

            if self.config.hard_stop_enabled && error_count >= self.config.exact_failure_block_after
            {
                return GuardrailDecision::Stop(format!(
                    "Tool '{tool_name}' failed {error_count} consecutive times — blocking further attempts."
                ));
            }
            if self.config.warnings_enabled && error_count >= self.config.exact_failure_warn_after {
                return GuardrailDecision::Warn(format!(
                    "WARNING: Tool '{tool_name}' keeps failing ({error_count} consecutive errors). Try a different approach."
                ));
            }
        }

        self.call_history.push((tool_name.to_string(), hash));
        GuardrailDecision::Proceed
    }

    /// Reset per-turn state.
    pub fn reset(&mut self) {
        self.call_history.clear();
        self.error_counts.clear();
        self.identical_call_count = 0;
        self.last_call = None;
    }
}

fn hash_args(args: &str) -> String {
    let mut hasher = DefaultHasher::new();
    args.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Self-healing retry mechanism — re-prompts the LLM with error context after
/// tool failures. Inspired by Unthinkclaw's loop_runner self-healing (3 attempts).
#[derive(Debug, Clone)]
pub struct SelfHealingRetry {
    pub max_attempts: u8,
    pub attempts_used: u8,
}

impl Default for SelfHealingRetry {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            attempts_used: 0,
        }
    }
}

impl SelfHealingRetry {
    pub fn new(max_attempts: u8) -> Self {
        Self {
            max_attempts,
            attempts_used: 0,
        }
    }

    /// Returns true if more healing attempts are available.
    pub fn should_retry(&self) -> bool {
        self.attempts_used < self.max_attempts
    }

    /// Build the healing message to re-prompt the LLM. Increments attempts_used.
    pub fn build_healing_message(&mut self, errors: &[String]) -> String {
        self.attempts_used += 1;
        let errors_joined = errors
            .iter()
            .map(|e| format!("  - {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "The following tool call(s) failed:\n{errors_joined}\n\nPlease try a different approach. Previous steps are still valid — don't repeat what already worked unless needed."
        )
    }
}

/// Empty-turn and stuck-tool recovery. Hosts apply the action; the engine only classifies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    Prefill(String),
    Nudge(String),
    Retry,
    Halt(String),
}

pub const PREFILL_TEXT: &str = "Continue from where you left off.";
pub const NUDGE_TEXT: &str =
    "Your last turn was empty. Answer with progress, a question, or a tool call.";

/// Empty assistant turns escalate Prefill → Nudge → Retry → Halt.
pub fn recover_empty_turn(empty_count: usize, max_empty: usize) -> RecoveryAction {
    if max_empty == 0 || empty_count >= max_empty {
        return RecoveryAction::Halt(format!(
            "empty turn limit reached ({empty_count}/{max_empty})"
        ));
    }
    match empty_count {
        0 => RecoveryAction::Prefill(PREFILL_TEXT.to_string()),
        1 => RecoveryAction::Nudge(NUDGE_TEXT.to_string()),
        n if n + 1 < max_empty => RecoveryAction::Retry,
        _ => RecoveryAction::Halt(format!(
            "empty turn limit reached ({empty_count}/{max_empty})"
        )),
    }
}

/// Stuck identical tool calls escalate Nudge → Retry → Halt.
pub fn recover_stuck_tool(repeat_count: usize, halt_after: usize) -> RecoveryAction {
    if halt_after == 0 || repeat_count >= halt_after {
        return RecoveryAction::Halt(format!(
            "stuck tool repeated {repeat_count} times (halt after {halt_after})"
        ));
    }
    if repeat_count == 0 {
        return RecoveryAction::Nudge(
            "The same tool call is repeating. Change arguments or stop.".to_string(),
        );
    }
    if repeat_count + 1 < halt_after {
        RecoveryAction::Retry
    } else {
        RecoveryAction::Halt(format!(
            "stuck tool repeated {repeat_count} times (halt after {halt_after})"
        ))
    }
}

pub fn check_empty_turn(assistant_content: &str) -> bool {
    assistant_content.trim().is_empty()
}

pub fn check_repeated_failures(results: &[ToolResult], threshold: usize) -> bool {
    let failures = results.iter().filter(|r| r.is_error).count();
    failures >= threshold
}

pub fn should_stop(
    empty_turns: usize,
    max_empty: usize,
    failures: usize,
    max_failures: usize,
) -> bool {
    empty_turns >= max_empty || failures >= max_failures
}

/// Default maximum tool iterations before the agent loop stops.
pub const MAX_TOOL_ITERATIONS_DEFAULT: usize = 50;

/// Hard ceiling for max tool iterations — values above this are clamped.
pub const MAX_TOOL_ITERATIONS_CEILING: usize = 1_000;

/// Numerator for the iteration warning threshold (warn at 80%).
pub const ITERATION_WARN_NUMERATOR: usize = 4;

/// Denominator for the iteration warning threshold (warn at 80%).
pub const ITERATION_WARN_DENOMINATOR: usize = 5;

/// Resolve the max tool iterations from CLI flag, env var, or default.
/// CLI flag takes precedence, then env var, then the default constant.
pub fn resolve_max_tool_iterations(env_var: Option<&str>, cli_flag: Option<usize>) -> usize {
    if let Some(flag) = cli_flag {
        return clamp_max_tool_iterations(flag);
    }
    if let Some(env) = env_var {
        if let Ok(value) = env.parse::<usize>() {
            return clamp_max_tool_iterations(value);
        }
    }
    MAX_TOOL_ITERATIONS_DEFAULT
}

/// Clamp a max tool iterations value to the hard ceiling.
pub fn clamp_max_tool_iterations(value: usize) -> usize {
    value.min(MAX_TOOL_ITERATIONS_CEILING)
}

/// Returns true when the current iteration count reaches the warning threshold
/// (80% of max by default).
pub fn should_warn_at_iteration_threshold(current: usize, max: usize) -> bool {
    if max == 0 {
        return false;
    }
    current * ITERATION_WARN_DENOMINATOR >= max * ITERATION_WARN_NUMERATOR
}

/// Returns a steering message to inject when approaching the iteration limit.
pub fn iteration_handoff_steering_text(current: usize, max: usize) -> String {
    format!(
        "Approaching tool iteration limit ({current}/{max}). Wrap up the current task and provide a summary of progress so far."
    )
}

/// Abort signal for cooperative cancellation (pi_agent_rust pattern).
/// Uses an atomic flag for polling and a tokio Notify for async waiting.
pub struct AbortSignal {
    aborted: AtomicBool,
    #[cfg(feature = "ipc")]
    notify: tokio::sync::Notify,
}

impl AbortSignal {
    /// Create a new, non-aborted signal.
    pub fn new() -> Self {
        Self {
            aborted: AtomicBool::new(false),
            #[cfg(feature = "ipc")]
            notify: tokio::sync::Notify::new(),
        }
    }

    /// Signal abort — sets the atomic flag and notifies any waiters.
    pub fn signal(&self) {
        self.aborted.store(true, Ordering::SeqCst);
        #[cfg(feature = "ipc")]
        self.notify.notify_one();
    }

    /// Returns true if abort has been signaled.
    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }

    /// Returns a future that completes when the signal is set.
    #[cfg(feature = "ipc")]
    pub fn cancelled<'a>(&'a self) -> impl Future<Output = ()> + 'a {
        self.notify.notified()
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        Self::new()
    }
}

pub fn reclassify_effect(name: &str, args: &str, registered: ToolEffect) -> ToolEffect {
    if name == "bash" || name == "exec" || name == "shell" {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
            if let Some(cmd) = v.get("command").and_then(|c| c.as_str()) {
                let first = cmd.split_whitespace().next().unwrap_or("");
                if matches!(
                    first,
                    "cat" | "ls" | "head" | "tail" | "pwd" | "echo" | "rg" | "grep" | "find"
                ) {
                    return ToolEffect::Read;
                }
                return ToolEffect::Process;
            }
        }
        return ToolEffect::Process;
    }
    if is_mutating_name(name) {
        ToolEffect::Write
    } else {
        registered
    }
}

fn is_mutating_name(name: &str) -> bool {
    matches!(
        name,
        "write" | "edit" | "hashline_edit" | "apply_patch" | "delete" | "rename" | "move"
    )
}

pub fn schedule_tool_calls(calls: &[(String, String, ToolEffect)]) -> Vec<Vec<usize>> {
    let effects: Vec<ToolEffect> = calls
        .iter()
        .map(|(name, args, registered)| reclassify_effect(name, args, *registered))
        .collect();
    plan_tool_effect_batches(&effects)
}

/// Plan tool effect batches — groups indices that can run in parallel.
/// Consecutive parallel-capable tools (Read, Network) are grouped into a
/// single batch; each non-parallel tool (Write, Process) gets its own batch.
/// Batch order preserves the original tool order.
pub fn plan_tool_effect_batches(effects: &[ToolEffect]) -> Vec<Vec<usize>> {
    let mut batches: Vec<Vec<usize>> = Vec::new();
    let mut current_parallel: Vec<usize> = Vec::new();

    for (i, effect) in effects.iter().enumerate() {
        if effect.supports_parallel() {
            current_parallel.push(i);
        } else {
            if !current_parallel.is_empty() {
                batches.push(std::mem::take(&mut current_parallel));
            }
            batches.push(vec![i]);
        }
    }

    if !current_parallel.is_empty() {
        batches.push(current_parallel);
    }

    batches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ToolEffect;

    #[test]
    fn test_check_empty_turn() {
        // Empty and whitespace only
        assert!(check_empty_turn(""));
        assert!(check_empty_turn("   "));
        assert!(check_empty_turn("\t\n\r"));

        // Valid text
        assert!(!check_empty_turn("hello"));
        assert!(!check_empty_turn("  hello  "));
        assert!(!check_empty_turn("\n\thello\n"));
    }

    #[test]
    fn resolve_uses_cli_flag() {
        assert_eq!(resolve_max_tool_iterations(None, Some(100)), 100);
    }

    #[test]
    fn resolve_uses_env_var() {
        assert_eq!(resolve_max_tool_iterations(Some("75"), None), 75);
    }

    #[test]
    fn resolve_uses_default() {
        assert_eq!(
            resolve_max_tool_iterations(None, None),
            MAX_TOOL_ITERATIONS_DEFAULT
        );
    }

    #[test]
    fn resolve_cli_overrides_env() {
        assert_eq!(resolve_max_tool_iterations(Some("75"), Some(100)), 100);
    }

    #[test]
    fn resolve_clamps_to_ceiling() {
        assert_eq!(
            resolve_max_tool_iterations(None, Some(2000)),
            MAX_TOOL_ITERATIONS_CEILING
        );
    }

    #[test]
    fn resolve_invalid_env_falls_back() {
        assert_eq!(
            resolve_max_tool_iterations(Some("not-a-number"), None),
            MAX_TOOL_ITERATIONS_DEFAULT
        );
    }

    #[test]
    fn clamp_below_ceiling() {
        assert_eq!(clamp_max_tool_iterations(50), 50);
    }

    #[test]
    fn clamp_at_ceiling() {
        assert_eq!(
            clamp_max_tool_iterations(MAX_TOOL_ITERATIONS_CEILING),
            MAX_TOOL_ITERATIONS_CEILING
        );
    }

    #[test]
    fn clamp_above_ceiling() {
        assert_eq!(clamp_max_tool_iterations(2000), MAX_TOOL_ITERATIONS_CEILING);
    }

    #[test]
    fn warn_below_threshold() {
        assert!(!should_warn_at_iteration_threshold(39, 50));
    }

    #[test]
    fn warn_at_threshold() {
        assert!(should_warn_at_iteration_threshold(40, 50));
    }

    #[test]
    fn warn_above_threshold() {
        assert!(should_warn_at_iteration_threshold(45, 50));
    }

    #[test]
    fn warn_zero_max() {
        assert!(!should_warn_at_iteration_threshold(0, 0));
    }

    #[test]
    fn handoff_text_contains_counts() {
        let text = iteration_handoff_steering_text(40, 50);
        assert!(text.contains("40"));
        assert!(text.contains("50"));
    }

    #[test]
    fn abort_signal_initial_not_aborted() {
        let signal = AbortSignal::new();
        assert!(!signal.is_aborted());
    }

    #[test]
    fn abort_signal_signal_sets_aborted() {
        let signal = AbortSignal::new();
        signal.signal();
        assert!(signal.is_aborted());
    }

    #[test]
    fn abort_signal_default_not_aborted() {
        let signal = AbortSignal::default();
        assert!(!signal.is_aborted());
    }

    #[cfg(feature = "ipc")]
    #[tokio::test]
    async fn abort_signal_cancelled_completes_after_signal() {
        let signal = AbortSignal::new();
        signal.signal();
        signal.cancelled().await;
    }

    #[cfg(feature = "ipc")]
    #[tokio::test]
    async fn abort_signal_cancelled_completes_concurrent_signal() {
        let signal = std::sync::Arc::new(AbortSignal::new());
        let signal2 = signal.clone();
        tokio::spawn(async move {
            signal2.signal();
        });
        signal.cancelled().await;
    }

    #[test]
    fn plan_batches_all_parallel() {
        let effects = [ToolEffect::Read, ToolEffect::Read, ToolEffect::Network];
        let batches = plan_tool_effect_batches(&effects);
        assert_eq!(batches, vec![vec![0, 1, 2]]);
    }

    #[test]
    fn plan_batches_all_serial() {
        let effects = [ToolEffect::Write, ToolEffect::Process];
        let batches = plan_tool_effect_batches(&effects);
        assert_eq!(batches, vec![vec![0], vec![1]]);
    }

    #[test]
    fn plan_batches_mixed() {
        let effects = [
            ToolEffect::Read,
            ToolEffect::Read,
            ToolEffect::Write,
            ToolEffect::Read,
            ToolEffect::Process,
        ];
        let batches = plan_tool_effect_batches(&effects);
        assert_eq!(batches, vec![vec![0, 1], vec![2], vec![3], vec![4]]);
    }

    #[test]
    fn plan_batches_empty() {
        let effects: [ToolEffect; 0] = [];
        let batches = plan_tool_effect_batches(&effects);
        assert!(batches.is_empty());
    }

    #[test]
    fn test_classify_tool_edge_cases() {
        // String matching is case-sensitive and exact
        assert_eq!(classify_tool("READ"), ToolClass::Mutating);
        assert_eq!(classify_tool("read "), ToolClass::Mutating);
        assert_eq!(classify_tool(" read"), ToolClass::Mutating);
        assert_eq!(classify_tool("r\0ead"), ToolClass::Mutating);
        assert_eq!(classify_tool("Idempotent"), ToolClass::Mutating);
        assert_eq!(classify_tool("Mutating"), ToolClass::Mutating);
    }

    #[test]
    fn test_classify_tool() {
        assert_eq!(classify_tool("read"), ToolClass::Idempotent);
        assert_eq!(classify_tool("grep"), ToolClass::Idempotent);
        assert_eq!(classify_tool("glob"), ToolClass::Idempotent);
        assert_eq!(classify_tool("search"), ToolClass::Idempotent);
        assert_eq!(classify_tool("list"), ToolClass::Idempotent);
        assert_eq!(classify_tool("stat"), ToolClass::Idempotent);
        assert_eq!(classify_tool("web_search"), ToolClass::Idempotent);
        assert_eq!(classify_tool("web_fetch"), ToolClass::Idempotent);
        assert_eq!(classify_tool("session_status"), ToolClass::Idempotent);
        assert_eq!(classify_tool("memory_search"), ToolClass::Idempotent);

        assert_eq!(classify_tool("write"), ToolClass::Mutating);
        assert_eq!(classify_tool("edit"), ToolClass::Mutating);
        assert_eq!(classify_tool("shell"), ToolClass::Mutating);
        assert_eq!(classify_tool("exec"), ToolClass::Mutating);
        assert_eq!(classify_tool("bash"), ToolClass::Mutating);
        assert_eq!(classify_tool("delete"), ToolClass::Mutating);
        assert_eq!(classify_tool("rename"), ToolClass::Mutating);
        assert_eq!(classify_tool("move"), ToolClass::Mutating);
        assert_eq!(classify_tool("mkdir"), ToolClass::Mutating);
        assert_eq!(classify_tool("rmdir"), ToolClass::Mutating);

        // Unknown tool defaults to Mutating (safe default).
        assert_eq!(classify_tool("unknown_tool"), ToolClass::Mutating);
        assert_eq!(classify_tool(""), ToolClass::Mutating);

        // Display impl.
        assert_eq!(format!("{}", ToolClass::Idempotent), "idempotent");
        assert_eq!(format!("{}", ToolClass::Mutating), "mutating");
    }

    #[test]
    fn test_loop_detection() {
        // Warn after same_tool_failure_warn_after identical calls.
        let config = GuardrailConfig {
            same_tool_failure_warn_after: 3,
            ..Default::default()
        };
        let mut g = ToolGuardrails::new(config);
        for _ in 0..3 {
            let _ = g.observe("shell", r#"{"command":"ls"}"#, false);
        }
        let decision = g.observe("shell", r#"{"command":"ls"}"#, false);
        assert!(matches!(decision, GuardrailDecision::Warn(_)));

        // Hard stop after same_tool_failure_halt_after identical calls.
        let config = GuardrailConfig {
            hard_stop_enabled: true,
            same_tool_failure_halt_after: 3,
            ..Default::default()
        };
        let mut g = ToolGuardrails::new(config);
        for _ in 0..3 {
            let _ = g.observe("shell", r#"{"command":"ls"}"#, false);
        }
        let decision = g.observe("shell", r#"{"command":"ls"}"#, false);
        assert!(matches!(decision, GuardrailDecision::Stop(_)));

        // Different args do not trigger loop detection.
        let config = GuardrailConfig {
            same_tool_failure_warn_after: 3,
            ..Default::default()
        };
        let mut g = ToolGuardrails::new(config);
        let _ = g.observe("shell", r#"{"command":"ls"}"#, false);
        let decision = g.observe("shell", r#"{"command":"pwd"}"#, false);
        assert!(matches!(decision, GuardrailDecision::Proceed));

        // Reset clears loop state.
        let config = GuardrailConfig {
            same_tool_failure_warn_after: 3,
            ..Default::default()
        };
        let mut g = ToolGuardrails::new(config);
        for _ in 0..4 {
            let _ = g.observe("read", r#"{"path":"x"}"#, false);
        }
        g.reset();
        let decision = g.observe("read", r#"{"path":"x"}"#, false);
        assert!(matches!(decision, GuardrailDecision::Proceed));
    }

    #[test]
    fn test_error_counting() {
        // Warn after exact_failure_warn_after consecutive errors.
        let config = GuardrailConfig {
            exact_failure_warn_after: 2,
            ..Default::default()
        };
        let mut g = ToolGuardrails::new(config);
        let _ = g.observe("shell", r#"{"command":"ls"}"#, true);
        let decision = g.observe("shell", r#"{"command":"ls"}"#, true);
        assert!(matches!(decision, GuardrailDecision::Warn(_)));

        // Hard stop after exact_failure_block_after consecutive errors.
        let config = GuardrailConfig {
            hard_stop_enabled: true,
            exact_failure_block_after: 3,
            same_tool_failure_warn_after: 100,
            same_tool_failure_halt_after: 100,
            ..Default::default()
        };
        let mut g = ToolGuardrails::new(config);
        // Vary args so loop detection does not interfere with error counting.
        for i in 0..3 {
            let _ = g.observe("shell", &format!(r#"{{"command":"ls{i}"}}"#), true);
        }
        let decision = g.observe("shell", r#"{"command":"ls_final"}"#, true);
        assert!(matches!(decision, GuardrailDecision::Stop(_)));

        // Non-error calls do not increment error counts.
        let config = GuardrailConfig::default();
        let mut g = ToolGuardrails::new(config);
        let decision = g.observe("read", r#"{"path":"x"}"#, false);
        assert!(matches!(decision, GuardrailDecision::Proceed));
        assert!(g.error_counts.is_empty());
    }

    #[test]
    fn test_self_healing_retry() {
        let mut retry = SelfHealingRetry::default();
        assert_eq!(retry.max_attempts, 3);
        assert_eq!(retry.attempts_used, 0);
        assert!(retry.should_retry());

        let msg = retry.build_healing_message(&["error one".to_string()]);
        assert!(msg.contains("The following tool call(s) failed:"));
        assert!(msg.contains("error one"));
        assert!(msg.contains("Please try a different approach."));
        assert_eq!(retry.attempts_used, 1);
        assert!(retry.should_retry());

        let _ = retry.build_healing_message(&["e2".to_string()]);
        assert_eq!(retry.attempts_used, 2);
        assert!(retry.should_retry());

        let _ = retry.build_healing_message(&["e3".to_string()]);
        assert_eq!(retry.attempts_used, 3);
        assert!(!retry.should_retry());

        // Custom max_attempts.
        let mut retry = SelfHealingRetry::new(1);
        assert!(retry.should_retry());
        let _ = retry.build_healing_message(&["e".to_string()]);
        assert!(!retry.should_retry());
    }

    #[test]
    fn test_guardrail_decision_proceed() {
        let mut g = ToolGuardrails::new(GuardrailConfig::default());
        let decision = g.observe("read", r#"{"path":"test"}"#, false);
        assert_eq!(decision, GuardrailDecision::Proceed);
    }

    #[test]
    fn test_guardrail_decision_stop() {
        let config = GuardrailConfig {
            hard_stop_enabled: true,
            same_tool_failure_halt_after: 2,
            ..Default::default()
        };
        let mut g = ToolGuardrails::new(config);
        for _ in 0..2 {
            let _ = g.observe("shell", r#"{"command":"ls"}"#, false);
        }
        let decision = g.observe("shell", r#"{"command":"ls"}"#, false);
        assert!(matches!(decision, GuardrailDecision::Stop(_)));
    }

    #[test]
    fn test_check_repeated_failures() {
        // Empty results list
        let empty_results: Vec<ToolResult> = vec![];
        assert!(!check_repeated_failures(&empty_results, 3));
        assert!(check_repeated_failures(&empty_results, 0));

        let fail1 = ToolResult {
            id: "test".to_string(),
            content: "test".to_string(),
            is_error: true,
            error_kind: None,
        };
        let fail2 = ToolResult {
            id: "test".to_string(),
            content: "test".to_string(),
            is_error: true,
            error_kind: None,
        };
        let fail3 = ToolResult {
            id: "test".to_string(),
            content: "test".to_string(),
            is_error: true,
            error_kind: None,
        };
        let fail4 = ToolResult {
            id: "test".to_string(),
            content: "test".to_string(),
            is_error: true,
            error_kind: None,
        };
        let success1 = ToolResult {
            id: "test".to_string(),
            content: "test".to_string(),
            is_error: false,
            error_kind: None,
        };

        // Failures below threshold
        let results_below = vec![fail1.clone(), success1.clone(), fail2.clone()];
        assert!(!check_repeated_failures(&results_below, 3));

        // Failures exactly at threshold
        let results_exact = vec![
            fail1.clone(),
            fail2.clone(),
            success1.clone(),
            fail3.clone(),
        ];
        assert!(check_repeated_failures(&results_exact, 3));

        // Failures exceeding threshold
        let results_exceed = vec![fail1.clone(), fail2.clone(), fail3.clone(), fail4.clone()];
        assert!(check_repeated_failures(&results_exceed, 3));
    }

    #[test]
    fn test_should_stop() {
        assert!(!should_stop(0, 1, 0, 1));
        assert!(should_stop(1, 1, 0, 1));
        assert!(should_stop(0, 1, 1, 1));
        assert!(should_stop(1, 1, 1, 1));
    }

    #[test]
    fn recover_empty_turn_prefill_then_nudge_then_halt() {
        assert_eq!(
            recover_empty_turn(0, 3),
            RecoveryAction::Prefill(PREFILL_TEXT.to_string())
        );
        assert_eq!(
            recover_empty_turn(1, 3),
            RecoveryAction::Nudge(NUDGE_TEXT.to_string())
        );
        assert_eq!(
            recover_empty_turn(2, 3),
            RecoveryAction::Halt("empty turn limit reached (2/3)".to_string())
        );
        assert!(matches!(recover_empty_turn(0, 0), RecoveryAction::Halt(_)));
    }

    #[test]
    fn recover_empty_turn_retry_before_halt() {
        assert_eq!(recover_empty_turn(2, 5), RecoveryAction::Retry);
        assert!(matches!(recover_empty_turn(4, 5), RecoveryAction::Halt(_)));
    }

    #[test]
    fn schedule_reclassifies_then_preserves_model_order() {
        let calls = [
            (
                "bash".into(),
                r#"{"command":"ls"}"#.into(),
                ToolEffect::Process,
            ),
            (
                "bash".into(),
                r#"{"command":"pwd"}"#.into(),
                ToolEffect::Process,
            ),
            ("write".into(), r#"{"path":"a"}"#.into(), ToolEffect::Write),
            ("read".into(), r#"{"path":"b"}"#.into(), ToolEffect::Read),
        ];
        let batches = schedule_tool_calls(&calls);
        assert_eq!(batches, vec![vec![0, 1], vec![2], vec![3]]);
        assert_eq!(
            reclassify_effect("bash", r#"{"command":"rm -rf x"}"#, ToolEffect::Read),
            ToolEffect::Process
        );
    }

    #[test]
    fn recover_stuck_tool_nudge_retry_halt() {
        assert!(matches!(recover_stuck_tool(0, 3), RecoveryAction::Nudge(_)));
        assert_eq!(recover_stuck_tool(1, 3), RecoveryAction::Retry);
        assert!(matches!(recover_stuck_tool(2, 3), RecoveryAction::Halt(_)));
        assert!(matches!(recover_stuck_tool(0, 0), RecoveryAction::Halt(_)));
    }
}
