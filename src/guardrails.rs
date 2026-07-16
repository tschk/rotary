//! Guardrails: empty-turn detection, repeated tool failure, loop hygiene,
//! bounded tool recursion, abort signaling, and tool effect batching.

use crate::agent::{ToolEffect, ToolResult};
#[cfg(feature = "ipc")]
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};

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
        "Approaching tool iteration limit ({}/{}). Wrap up the current task and provide a summary of progress so far.",
        current, max
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
    fn empty_detection() {
        assert!(check_empty_turn(""));
        assert!(check_empty_turn("   "));
        assert!(!check_empty_turn("hello"));
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
}
