//! Pi tool name mapping — bidirectional aliases between rx4 and pi tool names.
//!
//! Pi uses: read_file, write_file, list_dir, run_command, find_files, code_intel,
//! hashline_edit, grep, spawn_agent
//!
//! Rx4 uses: read, write, edit, bash, grep, find, ls

use std::collections::HashMap;
use std::sync::OnceLock;

/// Mapping from pi tool names to rx4 tool names.
pub static PI_TO_RX4: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

/// Mapping from rx4 tool names to pi tool names.
pub static RX4_TO_PI: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

/// All pi tool name aliases (for display and documentation).
/// Ordered so that the canonical pi name comes LAST for each rx4 name
/// (the reverse mapping RX4_TO_PI uses last-wins insertion).
pub const PI_TOOL_ALIASES: &[(&str, &str)] = &[
    // Rx4 native self-mappings (first, so canonical pi names overwrite them)
    ("read", "read"),
    ("write", "write"),
    ("edit", "edit"),
    ("bash", "bash"),
    ("grep", "grep"),
    ("find", "find"),
    ("ls", "ls"),
    // Additional pi-compatible aliases (codex, etc.)
    ("search_replace", "edit"),
    ("apply_patch", "edit"),
    // Canonical pi names (last, so they win in reverse mapping)
    ("read_file", "read"),
    ("write_file", "write"),
    ("list_dir", "ls"),
    ("run_command", "bash"),
    ("find_files", "find"),
    ("code_intel", "grep"),
    ("hashline_edit", "edit"),
    ("spawn_agent", "spawn_agent"),
];

fn init_maps() {
    PI_TO_RX4.get_or_init(|| {
        let mut m = HashMap::new();
        for (pi, rx4) in PI_TOOL_ALIASES {
            m.insert(*pi, *rx4);
        }
        m
    });
    RX4_TO_PI.get_or_init(|| {
        let mut m = HashMap::new();
        for (pi, rx4) in PI_TOOL_ALIASES {
            m.insert(*rx4, *pi);
        }
        m
    });
}

/// Convert a pi tool name to its rx4 equivalent.
pub fn pi_to_rx4_tool(name: &str) -> &str {
    init_maps();
    PI_TO_RX4
        .get()
        .and_then(|m| m.get(name))
        .copied()
        .unwrap_or(name)
}

/// Convert an rx4 tool name to its pi equivalent.
pub fn rx4_to_pi_tool(name: &str) -> &str {
    init_maps();
    RX4_TO_PI
        .get()
        .and_then(|m| m.get(name))
        .copied()
        .unwrap_or(name)
}

/// Check if a tool name is a recognized pi tool.
pub fn is_pi_tool_name(name: &str) -> bool {
    init_maps();
    PI_TO_RX4
        .get()
        .map(|m| m.contains_key(name))
        .unwrap_or(false)
}

/// Get all known pi tool names.
pub fn pi_tool_names() -> Vec<&'static str> {
    init_maps();
    PI_TO_RX4
        .get()
        .map(|m| m.keys().copied().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_to_rx4_mapping() {
        assert_eq!(pi_to_rx4_tool("read_file"), "read");
        assert_eq!(pi_to_rx4_tool("write_file"), "write");
        assert_eq!(pi_to_rx4_tool("list_dir"), "ls");
        assert_eq!(pi_to_rx4_tool("run_command"), "bash");
        assert_eq!(pi_to_rx4_tool("find_files"), "find");
        assert_eq!(pi_to_rx4_tool("code_intel"), "grep");
        assert_eq!(pi_to_rx4_tool("hashline_edit"), "edit");
        assert_eq!(pi_to_rx4_tool("search_replace"), "edit");
        assert_eq!(pi_to_rx4_tool("apply_patch"), "edit");
    }

    #[test]
    fn rx4_to_pi_mapping() {
        assert_eq!(rx4_to_pi_tool("read"), "read_file");
        assert_eq!(rx4_to_pi_tool("write"), "write_file");
        assert_eq!(rx4_to_pi_tool("ls"), "list_dir");
        assert_eq!(rx4_to_pi_tool("bash"), "run_command");
        assert_eq!(rx4_to_pi_tool("find"), "find_files");
        assert_eq!(rx4_to_pi_tool("grep"), "code_intel");
        assert_eq!(rx4_to_pi_tool("edit"), "hashline_edit");
    }

    #[test]
    fn unknown_passthrough() {
        assert_eq!(pi_to_rx4_tool("custom_tool"), "custom_tool");
        assert_eq!(rx4_to_pi_tool("custom_tool"), "custom_tool");
    }

    #[test]
    fn is_pi_tool() {
        assert!(is_pi_tool_name("read_file"));
        assert!(is_pi_tool_name("run_command"));
        assert!(!is_pi_tool_name("not_a_pi_tool"));
    }
}
