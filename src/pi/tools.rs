//! Pi tool name mapping — bidirectional aliases between rx4 and pi tool names.
//!
//! Pi uses: read_file, write_file, list_dir, run_command, find_files, code_intel,
//! hashline_edit, grep, spawn_agent
//!
//! Rx4 uses: read, write, edit, bash, grep, find, ls

/// Convert a pi tool name to its rx4 equivalent.
pub fn pi_to_rx4_tool(name: &str) -> &str {
    match name {
        "read_file" | "read" => "read",
        "write_file" | "write" => "write",
        "list_dir" | "ls" => "ls",
        "run_command" | "bash" => "bash",
        "find_files" | "find" => "find",
        "code_intel" | "grep" => "grep",
        "hashline_edit" | "search_replace" | "apply_patch" | "edit" => "edit",
        "spawn_agent" => "spawn_agent",
        _ => name,
    }
}

/// Convert an rx4 tool name to its pi equivalent.
pub fn rx4_to_pi_tool(name: &str) -> &str {
    match name {
        "read" => "read_file",
        "write" => "write_file",
        "ls" => "list_dir",
        "bash" => "run_command",
        "find" => "find_files",
        "grep" => "code_intel",
        "edit" => "hashline_edit",
        "spawn_agent" => "spawn_agent",
        _ => name,
    }
}

/// Check if a tool name is a recognized pi tool.
pub fn is_pi_tool_name(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "read"
            | "write_file"
            | "write"
            | "list_dir"
            | "ls"
            | "run_command"
            | "bash"
            | "find_files"
            | "find"
            | "code_intel"
            | "grep"
            | "hashline_edit"
            | "search_replace"
            | "apply_patch"
            | "edit"
            | "spawn_agent"
    )
}

/// Get all known pi tool names.
pub fn pi_tool_names() -> Vec<&'static str> {
    vec![
        "read",
        "write",
        "edit",
        "bash",
        "grep",
        "find",
        "ls",
        "search_replace",
        "apply_patch",
        "read_file",
        "write_file",
        "list_dir",
        "run_command",
        "find_files",
        "code_intel",
        "hashline_edit",
        "spawn_agent",
    ]
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
