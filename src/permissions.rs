//! Permissions: policy modes, allow/deny lists, async approver (pi_agent_rust pattern).

use crate::agent::ToolCall;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    FullAccess,
    ReadOnly,
    WorkspaceWrite,
    DenyAll,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub mode: PermissionMode,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub allowlist: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub denylist: Vec<String>,
}

impl Policy {
    pub fn full_access() -> Self {
        Self {
            mode: PermissionMode::FullAccess,
            allowlist: vec![],
            denylist: vec![],
        }
    }
    pub fn read_only() -> Self {
        Self {
            mode: PermissionMode::ReadOnly,
            allowlist: vec![],
            denylist: vec![],
        }
    }
    pub fn workspace_write() -> Self {
        Self {
            mode: PermissionMode::WorkspaceWrite,
            allowlist: vec![],
            denylist: vec![],
        }
    }
    pub fn deny_all() -> Self {
        Self {
            mode: PermissionMode::DenyAll,
            allowlist: vec![],
            denylist: vec![],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    Ask,
}

/// Approver trait — hosts implement this to prompt the user (codex-rs pattern).
pub trait Approver: Send + Sync {
    fn approve(&self, tool_call: &ToolCall) -> Decision;
}

/// Always-allow approver (for testing / yolo mode).
pub struct AlwaysAllow;
impl Approver for AlwaysAllow {
    fn approve(&self, _call: &ToolCall) -> Decision {
        Decision::Allow
    }
}

/// Always-deny approver.
pub struct AlwaysDeny;
impl Approver for AlwaysDeny {
    fn approve(&self, _call: &ToolCall) -> Decision {
        Decision::Deny
    }
}

pub fn authorize(
    policy: &Policy,
    tool_name: &str,
    _arguments: &str,
    approver: Option<&dyn Approver>,
) -> Decision {
    if policy.denylist.iter().any(|d| d == tool_name) {
        return Decision::Deny;
    }
    if !policy.allowlist.is_empty() {
        if policy.allowlist.iter().any(|a| a == tool_name) {
            return Decision::Allow;
        }
        return Decision::Deny;
    }
    let mode_decision = match policy.mode {
        PermissionMode::FullAccess => Decision::Allow,
        PermissionMode::DenyAll => Decision::Deny,
        PermissionMode::ReadOnly => {
            if is_read_only_tool(tool_name) {
                Decision::Allow
            } else {
                Decision::Ask
            }
        }
        PermissionMode::WorkspaceWrite => {
            // Read + workspace write tools auto-allow; bash/process and other tools Ask.
            if is_read_only_tool(tool_name) || is_write_tool(tool_name) {
                Decision::Allow
            } else {
                Decision::Ask
            }
        }
    };
    if mode_decision == Decision::Ask {
        if let Some(app) = approver {
            let dummy_call = ToolCall {
                id: String::new(),
                name: tool_name.to_string(),
                arguments: String::new(),
            };
            return app.approve(&dummy_call);
        }
    }
    mode_decision
}

fn is_read_only_tool(name: &str) -> bool {
    matches!(
        name,
        "read"
            | "read_file"
            | "ls"
            | "list_dir"
            | "find"
            | "find_files"
            | "grep"
            | "code_intel"
            | "cu_see"
            | "cu_image"
            | "cu_list"
    )
}

/// Returns true when the tool mutates workspace files (write/edit family).
pub fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "write" | "write_file" | "edit" | "hashline_edit" | "search_replace" | "apply_patch"
    )
}

/// Returns true when the tool is a shell/process executor.
pub fn is_process_tool(name: &str) -> bool {
    matches!(name, "bash" | "run_command")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_access_allows() {
        assert_eq!(
            authorize(&Policy::full_access(), "write", "{}", None),
            Decision::Allow
        );
    }

    #[test]
    fn deny_all_blocks() {
        assert_eq!(
            authorize(&Policy::deny_all(), "read", "{}", None),
            Decision::Deny
        );
    }

    #[test]
    fn denylist_overrides() {
        let p = Policy {
            mode: PermissionMode::FullAccess,
            allowlist: vec![],
            denylist: vec!["bash".into()],
        };
        assert_eq!(authorize(&p, "bash", "{}", None), Decision::Deny);
    }

    #[test]
    fn read_only_allows_reads() {
        assert_eq!(
            authorize(&Policy::read_only(), "read", "{}", None),
            Decision::Allow
        );
        assert_eq!(
            authorize(&Policy::read_only(), "write", "{}", None),
            Decision::Ask
        );
    }

    #[test]
    fn approver_called_on_ask() {
        assert_eq!(
            authorize(&Policy::read_only(), "write", "{}", Some(&AlwaysAllow)),
            Decision::Allow
        );
        assert_eq!(
            authorize(&Policy::read_only(), "write", "{}", Some(&AlwaysDeny)),
            Decision::Deny
        );
    }

    #[test]
    fn workspace_write_allows_edit_asks_bash() {
        assert_eq!(
            authorize(&Policy::workspace_write(), "read", "{}", None),
            Decision::Allow
        );
        assert_eq!(
            authorize(&Policy::workspace_write(), "edit", "{}", None),
            Decision::Allow
        );
        assert_eq!(
            authorize(&Policy::workspace_write(), "write", "{}", None),
            Decision::Allow
        );
        assert_eq!(
            authorize(&Policy::workspace_write(), "bash", "{}", None),
            Decision::Ask
        );
        assert_eq!(
            authorize(&Policy::workspace_write(), "unknown_tool", "{}", None),
            Decision::Ask
        );
    }
}
