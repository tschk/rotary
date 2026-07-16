//! Work scopes (not named agents). Profiles shape tools, policy, and prompt tone.

use crate::permissions::Policy;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Coding,
    Research,
    Plan,
    Ask,
    ComputerUse,
}

impl Scope {
    pub fn parse_scope(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "coding" | "code" => Some(Self::Coding),
            "research" | "explore" => Some(Self::Research),
            "plan" => Some(Self::Plan),
            "ask" | "chat" => Some(Self::Ask),
            "computer_use" | "computer-use" | "desktop" | "cu" => Some(Self::ComputerUse),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Research => "research",
            Self::Plan => "plan",
            Self::Ask => "ask",
            Self::ComputerUse => "computer_use",
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub scope: Scope,
    pub system_addendum: &'static str,
    pub policy: Policy,
    pub allowed_tools: Option<&'static [&'static str]>,
}

pub const CODING_TOOLS: &[&str] = &[
    "read",
    "read_file",
    "write",
    "write_file",
    "edit",
    "hashline_edit",
    "search_replace",
    "apply_patch",
    "bash",
    "run_command",
    "grep",
    "code_intel",
    "find",
    "find_files",
    "ls",
    "list_dir",
    "spawn_agent",
];
pub const RESEARCH_TOOLS: &[&str] = &[
    "read",
    "read_file",
    "ls",
    "list_dir",
    "find",
    "find_files",
    "grep",
    "code_intel",
    "bash",
    "run_command",
];
pub const PLAN_TOOLS: &[&str] = &[
    "read",
    "read_file",
    "ls",
    "list_dir",
    "find",
    "find_files",
    "grep",
    "code_intel",
];
pub const COMPUTER_USE_TOOLS: &[&str] = &[
    "cu_call",
    "cu_see",
    "cu_image",
    "cu_click",
    "cu_type",
    "cu_hotkey",
    "cu_scroll",
    "cu_window",
    "cu_app",
    "cu_list",
    "read",
    "read_file",
    "ls",
    "list_dir",
    "find",
    "find_files",
];

pub fn profile(scope: Scope) -> Profile {
    match scope {
        Scope::Coding => Profile {
            scope,
            system_addendum: "You are a precise coding harness. Inspect the tree before editing. Prefer small diffs. Run tests when useful. Never invent file contents.",
            policy: Policy::workspace_write(),
            allowed_tools: Some(CODING_TOOLS),
        },
        Scope::Research => Profile {
            scope,
            system_addendum: "Explore and explain the codebase. Prefer read-only tools. Avoid mutating files unless asked.",
            policy: Policy::read_only(),
            allowed_tools: Some(RESEARCH_TOOLS),
        },
        Scope::Plan => Profile {
            scope,
            system_addendum: "Produce a concrete multi-step plan: files, risks, verification. Do not modify the workspace.",
            policy: Policy::read_only(),
            allowed_tools: Some(PLAN_TOOLS),
        },
        Scope::Ask => Profile {
            scope,
            system_addendum: "Answer clearly from context. Tools are off unless the host enables them.",
            policy: Policy::deny_all(),
            allowed_tools: Some(&[]),
        },
        Scope::ComputerUse => Profile {
            scope,
            system_addendum: "Drive the desktop carefully via computer-use tools (embedded rs_peekaboo). Observe with see/image before click/type. Prefer reversible actions.",
            policy: Policy::full_access(),
            allowed_tools: Some(COMPUTER_USE_TOOLS),
        },
    }
}

pub fn compose_prompt(base: Option<&str>, p: &Profile) -> String {
    match base {
        Some(b) => format!(
            "{b}\n\n# Scope: {}\n\n{}",
            p.scope.name(),
            p.system_addendum
        ),
        None => format!("# Scope: {}\n\n{}", p.scope.name(), p.system_addendum),
    }
}

pub fn tool_allowed(p: &Profile, tool_name: &str) -> bool {
    match p.allowed_tools {
        None => true,
        Some(list) => list.contains(&tool_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_parse() {
        assert_eq!(Scope::parse_scope("code"), Some(Scope::Coding));
        assert_eq!(Scope::parse_scope("cu"), Some(Scope::ComputerUse));
        assert_eq!(Scope::parse_scope("nope"), None);
    }

    #[test]
    fn plan_blocks_writes() {
        let p = profile(Scope::Plan);
        assert!(tool_allowed(&p, "read_file"));
        assert!(!tool_allowed(&p, "write_file"));
    }
}
