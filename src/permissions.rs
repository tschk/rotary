//! Permissions: policy modes, allow/deny lists, async approver (pi_agent_rust pattern).

use crate::agent::ToolCall;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

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
    /// When true, hosts/Agent should enable OS seatbelt/bwrap for process tools.
    #[serde(default)]
    pub enable_os_sandbox: bool,
}

impl Policy {
    pub fn full_access() -> Self {
        Self {
            mode: PermissionMode::FullAccess,
            allowlist: vec![],
            denylist: vec![],
            enable_os_sandbox: false,
        }
    }
    pub fn read_only() -> Self {
        Self {
            mode: PermissionMode::ReadOnly,
            allowlist: vec![],
            denylist: vec![],
            enable_os_sandbox: false,
        }
    }
    pub fn workspace_write() -> Self {
        Self {
            mode: PermissionMode::WorkspaceWrite,
            allowlist: vec![],
            denylist: vec![],
            enable_os_sandbox: true,
        }
    }
    pub fn deny_all() -> Self {
        Self {
            mode: PermissionMode::DenyAll,
            allowlist: vec![],
            denylist: vec![],
            enable_os_sandbox: false,
        }
    }

    /// Enable or disable OS sandbox plugin flag (seatbelt/bwrap).
    pub fn with_os_sandbox(mut self, enabled: bool) -> Self {
        self.enable_os_sandbox = enabled;
        self
    }
}

impl Default for Policy {
    /// Secure default matches `Agent::new` — not full access.
    fn default() -> Self {
        Self::workspace_write()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Allow,
    Deny,
    Ask,
}

/// Rich approval payload for host UX (Codex-style ask).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: String,
    pub reason: String,
    pub policy_mode: String,
    pub is_process_tool: bool,
    pub is_write_tool: bool,
}

impl ApprovalRequest {
    pub fn from_call(call: &ToolCall, policy: &Policy) -> Self {
        let name = call.name.as_str();
        Self {
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            arguments: call.arguments.clone(),
            reason: format!(
                "policy {:?} requires approval for tool `{name}`",
                policy.mode
            ),
            policy_mode: format!("{:?}", policy.mode),
            is_process_tool: is_process_tool(name),
            is_write_tool: is_write_tool(name),
        }
    }
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

/// True when `path` escapes `workspace_root` (absolute or after `..` resolution).
pub fn path_outside_workspace(workspace_root: &Path, path: &str) -> bool {
    let p = Path::new(path);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace_root.join(p)
    };
    let canon = normalize_lexically(&joined);
    let root = normalize_lexically(workspace_root);
    !canon.starts_with(&root)
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn path_from_args(arguments: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(arguments).ok()?;
    for key in ["path", "file", "file_path"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

pub fn authorize(
    policy: &Policy,
    tool_name: &str,
    arguments: &str,
    approver: Option<&dyn Approver>,
) -> Decision {
    authorize_with_workspace(policy, tool_name, arguments, approver, None)
}

pub fn authorize_with_workspace(
    policy: &Policy,
    tool_name: &str,
    arguments: &str,
    approver: Option<&dyn Approver>,
    workspace_root: Option<&Path>,
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

    if matches!(
        policy.mode,
        PermissionMode::WorkspaceWrite | PermissionMode::ReadOnly
    ) && is_write_tool(tool_name)
    {
        if let (Some(root), Some(path)) = (workspace_root, path_from_args(arguments)) {
            if path_outside_workspace(root, &path) {
                return Decision::Deny;
            }
        }
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
            let call = ToolCall {
                id: String::new(),
                name: tool_name.to_string(),
                arguments: arguments.to_string(),
            };
            return app.approve(&call);
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
            | "web_fetch"
            | "enter_plan_mode"
            | "exit_plan_mode"
    ) || name.starts_with("lsp_")
}

/// Returns true when the tool mutates workspace files (write/edit family).
pub fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "write"
            | "write_file"
            | "edit"
            | "hashline_edit"
            | "search_replace"
            | "apply_patch"
            | "todo"
    )
}

/// Returns true when the tool is a shell/process executor.
pub fn is_process_tool(name: &str) -> bool {
    matches!(name, "bash" | "run_command" | "spawn_agent")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn full_access_allows() {
        assert_eq!(
            authorize(&Policy::full_access(), "write", "{}", None),
            Decision::Allow
        );
    }

    #[test]
    fn default_is_workspace_write() {
        assert_eq!(Policy::default().mode, PermissionMode::WorkspaceWrite);
        assert_eq!(
            authorize(&Policy::default(), "bash", "{}", None),
            Decision::Ask
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
            enable_os_sandbox: false,
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

    struct CaptureApprover {
        seen: Mutex<Option<ToolCall>>,
    }

    impl Approver for CaptureApprover {
        fn approve(&self, call: &ToolCall) -> Decision {
            *self.seen.lock().unwrap() = Some(call.clone());
            Decision::Allow
        }
    }

    #[test]
    fn approver_sees_real_arguments() {
        let app = CaptureApprover {
            seen: Mutex::new(None),
        };
        let args = r#"{"path":"secret.txt","content":"x"}"#;
        assert_eq!(
            authorize(&Policy::read_only(), "write", args, Some(&app)),
            Decision::Allow
        );
        let seen = app.seen.lock().unwrap().clone().expect("approver called");
        assert_eq!(seen.name, "write");
        assert_eq!(seen.arguments, args);
    }

    #[test]
    fn write_outside_workspace_denied_under_workspace_write() {
        let root = Path::new("/proj");
        let outside = r#"{"path":"/tmp/escape.txt"}"#;
        assert_eq!(
            authorize_with_workspace(
                &Policy::workspace_write(),
                "write",
                outside,
                None,
                Some(root)
            ),
            Decision::Deny
        );
        let relative_escape = r#"{"path":"../../etc/passwd"}"#;
        assert_eq!(
            authorize_with_workspace(
                &Policy::workspace_write(),
                "write",
                relative_escape,
                None,
                Some(root)
            ),
            Decision::Deny
        );
        let inside = r#"{"path":"src/main.rs"}"#;
        assert_eq!(
            authorize_with_workspace(
                &Policy::workspace_write(),
                "write",
                inside,
                None,
                Some(root)
            ),
            Decision::Allow
        );
        assert_eq!(
            authorize_with_workspace(
                &Policy::full_access(),
                "write",
                outside,
                None,
                Some(root)
            ),
            Decision::Allow
        );
    }

    #[test]
    fn path_outside_workspace_helper() {
        let root = Path::new("/proj");
        assert!(path_outside_workspace(root, "/tmp/x"));
        assert!(path_outside_workspace(root, "../escape"));
        assert!(!path_outside_workspace(root, "src/lib.rs"));
        assert!(!path_outside_workspace(root, "/proj/src/lib.rs"));
    }
}
