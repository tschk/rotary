//! Prewalk / plan-yolo: investigate on the big model, apply on smol.
//!
//! Hosts enable the capability. The first real write switches one-way to the
//! smol/apply model. Reads never switch. There is no switch-back mid-session.
//!
//! Environment (hosts may set these):
//! - `RX4_PREWALK=1` — enable the mode
//! - `RX4_SMOL_MODEL` — apply model id (required to actually switch)
//! - `RX4_INVESTIGATE_MODEL` — plan/investigate model id

use crate::permissions::{is_process_tool, is_write_tool};

/// One-way investigate → apply switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prewalk {
    enabled: bool,
    investigate_model: String,
    smol_model: Option<String>,
    switched: bool,
}

impl Prewalk {
    pub fn new(investigate_model: impl Into<String>, smol_model: Option<String>) -> Self {
        Self {
            enabled: true,
            investigate_model: investigate_model.into(),
            smol_model,
            switched: false,
        }
    }

    /// Disabled prewalk that still tracks an investigate model.
    pub fn disabled(investigate_model: impl Into<String>) -> Self {
        Self {
            enabled: false,
            investigate_model: investigate_model.into(),
            smol_model: None,
            switched: false,
        }
    }

    /// Read `RX4_PREWALK`, `RX4_SMOL_MODEL`, `RX4_INVESTIGATE_MODEL`.
    pub fn from_env() -> Self {
        let investigate = std::env::var("RX4_INVESTIGATE_MODEL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "investigate".into());
        let smol = std::env::var("RX4_SMOL_MODEL")
            .ok()
            .filter(|s| !s.is_empty());
        let enabled = matches!(
            std::env::var("RX4_PREWALK").as_deref(),
            Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
        );
        Self {
            enabled,
            investigate_model: investigate,
            smol_model: smol,
            switched: false,
        }
    }

    pub fn enable(&mut self) {
        self.enabled = true;
    }

    pub fn set_smol_model(&mut self, model: impl Into<String>) {
        self.smol_model = Some(model.into());
    }

    pub fn set_investigate_model(&mut self, model: impl Into<String>) {
        if !self.switched {
            self.investigate_model = model.into();
        }
    }

    pub fn current_model(&self) -> &str {
        if self.switched {
            self.smol_model
                .as_deref()
                .unwrap_or(&self.investigate_model)
        } else {
            &self.investigate_model
        }
    }

    pub fn smol_model(&self) -> Option<&str> {
        self.smol_model.as_deref()
    }

    pub fn is_switched(&self) -> bool {
        self.switched
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Record a tool. The first mutating call switches one-way when smol is set.
    /// Returns `true` if this call performed the switch.
    pub fn record_tool(&mut self, name: &str, args: Option<&str>) -> bool {
        if !self.enabled || self.switched {
            return false;
        }
        if !is_mutating_call(name, args) {
            return false;
        }
        match &self.smol_model {
            Some(_) => {
                self.switched = true;
                true
            }
            None => {
                tracing::info!(
                    "prewalk: first write seen but RX4_SMOL_MODEL / smol_model unset; staying on {}",
                    self.investigate_model
                );
                false
            }
        }
    }
}

/// Write tools, hashline apply, and shells that mutate.
pub fn is_mutating_call(name: &str, args: Option<&str>) -> bool {
    // Session-scoped planning metadata is not a workspace write.
    if name == "todo" {
        return false;
    }
    if name == "hashline_edit" || is_write_tool(name) {
        return true;
    }
    if is_process_tool(name) {
        return args.map(shell_mutates).unwrap_or(true);
    }
    matches!(name, "shell" | "exec" | "delete" | "rename" | "move")
}

fn shell_mutates(args: &str) -> bool {
    let owned = extract_command(args);
    let cmd = owned.as_deref().unwrap_or(args);
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return false;
    }
    // Mutation markers (redirects, find -delete, pipes) beat the RO whitelist.
    if crate::permissions::has_unsupported_shell_syntax(cmd) || has_pipe_or_subshell(cmd) {
        return true;
    }
    if has_mutating_flags(cmd) {
        return true;
    }
    read_only_shell(cmd).map(|ro| !ro).unwrap_or(true)
}

fn has_mutating_flags(cmd: &str) -> bool {
    cmd.split_whitespace()
        .any(|tok| matches!(tok, "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"))
}

fn has_pipe_or_subshell(command: &str) -> bool {
    let bytes = command.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_single {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'\\' => {
                i += 2;
                continue;
            }
            b'\'' => {
                in_single = true;
                i += 1;
                continue;
            }
            b'"' => {
                in_double = true;
                i += 1;
                continue;
            }
            b'|' | b'(' | b')' => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn extract_command(args: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get("command")
        .or_else(|| v.get("cmd"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

fn read_only_shell(cmd: &str) -> Option<bool> {
    let first = cmd.split_whitespace().next().unwrap_or("");
    const RO: &[&str] = &[
        "ls", "cat", "head", "tail", "pwd", "echo", "true", "false", "rg", "grep", "find", "wc",
        "date", "whoami", "which", "type",
    ];
    if RO.contains(&first) {
        return Some(true);
    }
    if first == "git" {
        let sub = cmd.split_whitespace().nth(1).unwrap_or("");
        return Some(matches!(
            sub,
            "status" | "log" | "diff" | "show" | "blame" | "rev-parse"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_write_switches_one_way() {
        let mut p = Prewalk::new("big-model", Some("smol-model".into()));
        assert_eq!(p.current_model(), "big-model");
        assert!(!p.record_tool("read", None));
        assert_eq!(p.current_model(), "big-model");
        assert!(p.record_tool("write", Some(r#"{"path":"a"}"#)));
        assert!(p.is_switched());
        assert_eq!(p.current_model(), "smol-model");
        assert!(!p.record_tool("edit", None));
        assert_eq!(p.current_model(), "smol-model");
        assert!(!p.record_tool("read", None));
        assert_eq!(p.current_model(), "smol-model");
    }

    #[test]
    fn reads_do_not_switch() {
        let mut p = Prewalk::new("big", Some("smol".into()));
        assert!(!p.record_tool("read", Some(r#"{"path":"x"}"#)));
        assert!(!p.record_tool("grep", None));
        assert!(!p.record_tool("ls", None));
        assert!(!p.is_switched());
    }

    #[test]
    fn hashline_edit_switches() {
        let mut p = Prewalk::new("big", Some("smol".into()));
        assert!(p.record_tool("hashline_edit", None));
        assert_eq!(p.current_model(), "smol");
    }

    #[test]
    fn unset_smol_stays_on_investigate() {
        let mut p = Prewalk::new("big", None);
        assert!(!p.record_tool("write", None));
        assert!(!p.is_switched());
        assert_eq!(p.current_model(), "big");
    }

    #[test]
    fn readonly_shell_does_not_switch() {
        let mut p = Prewalk::new("big", Some("smol".into()));
        assert!(!p.record_tool("bash", Some(r#"{"command":"ls -la"}"#)));
        assert!(p.record_tool("bash", Some(r#"{"command":"rm file"}"#)));
        assert!(p.is_switched());
    }

    #[test]
    fn echo_and_cat_redirects_mutate() {
        assert!(shell_mutates(r#"{"command":"echo hi > /tmp/x"}"#));
        assert!(shell_mutates(r#"{"command":"cat < /etc/passwd"}"#));
        assert!(shell_mutates(r#"{"command":"echo hi >> /tmp/x"}"#));
        assert!(!shell_mutates(r#"{"command":"echo hello"}"#));
        assert!(!shell_mutates(r#"{"command":"cat README.md"}"#));
    }

    #[test]
    fn pipes_and_subshells_mutate() {
        assert!(shell_mutates(r#"{"command":"ls | wc"}"#));
        assert!(shell_mutates(r#"{"command":"(echo hi)"}"#));
        assert!(shell_mutates(r#"{"command":"echo `whoami`"}"#));
        assert!(is_mutating_call(
            "bash",
            Some(r#"{"command":"echo hi > /tmp/x"}"#)
        ));
        let mut p = Prewalk::new("big", Some("smol".into()));
        assert!(p.record_tool("bash", Some(r#"{"command":"echo hi > /tmp/x"}"#)));
        assert!(p.is_switched());
    }

    #[test]
    fn find_delete_mutates_before_ro_whitelist() {
        assert!(shell_mutates(r#"{"command":"find . -name '*.o' -delete"}"#));
        let mut p = Prewalk::new("big", Some("smol".into()));
        assert!(p.record_tool("bash", Some(r#"{"command":"find . -delete"}"#)));
        assert!(p.is_switched());
    }

    #[test]
    fn todo_does_not_flip_prewalk() {
        let mut p = Prewalk::new("big", Some("smol".into()));
        assert!(!is_mutating_call("todo", Some(r#"{"action":"list"}"#)));
        assert!(!p.record_tool("todo", Some(r#"{"action":"list"}"#)));
        assert!(!p.record_tool("todo", Some(r#"{"action":"add"}"#)));
        assert!(!p.is_switched());
        assert_eq!(p.current_model(), "big");
    }
}
