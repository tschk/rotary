//! Guardrails: empty-turn detection, repeated tool failure, loop hygiene.

use crate::agent::ToolResult;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_detection() {
        assert!(check_empty_turn(""));
        assert!(check_empty_turn("   "));
        assert!(!check_empty_turn("hello"));
    }
}
