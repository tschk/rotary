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
    "web_fetch",
    "web_search",
    "darash",
    "darash_search",
    "todo",
    "enter_plan_mode",
    "exit_plan_mode",
    "lsp_diagnostics",
    "lsp_definition",
    "lsp_references",
    "exec",
    "exec_spawn",
    #[cfg(feature = "script")]
    "script",
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
    "web_fetch",
    "web_search",
    "darash",
    "darash_search",
    "lsp_diagnostics",
    "lsp_definition",
    "lsp_references",
    "exec",
    "exec_spawn",
    #[cfg(feature = "script")]
    "script",
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
    "todo",
    "web_fetch",
    "web_search",
    "darash",
    "darash_search",
    "enter_plan_mode",
    "exit_plan_mode",
    "lsp_diagnostics",
    "lsp_definition",
    "lsp_references",
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
            system_addendum: "Drive the desktop carefully via computer-use tools (embedded rs_peekaboo). Observe with see/image before click/type. Prefer reversible actions. Host may elevate to full_access.",
            // Safer default; hosts opt into FullAccess via --full-access / set_policy.
            policy: Policy::workspace_write(),
            allowed_tools: Some(COMPUTER_USE_TOOLS),
        },
    }
}

/// Tool guidance ships with its tool: the `script` line is compiled in only
/// when the `script` feature (and therefore the tool registration) is on, and
/// `full_addendum` shows it only for profiles whose allowlist admits it.
#[cfg(feature = "script")]
const SCRIPT_TOOL_GUIDANCE: &str = "For multi-step data processing, parsing, numeric verification, or bulk file inspection, prefer the `script` tool (sandboxed LuaJIT) over shelling out to python/node or making many tool calls.";

fn full_addendum(p: &Profile) -> String {
    #[cfg(not(feature = "script"))]
    {
        p.system_addendum.to_string()
    }
    #[cfg(feature = "script")]
    {
        if p.allowed_tools.is_none_or(|l| l.contains(&"script")) {
            format!("{} {SCRIPT_TOOL_GUIDANCE}", p.system_addendum)
        } else {
            p.system_addendum.to_string()
        }
    }
}

pub fn compose_prompt(base: Option<&str>, p: &Profile) -> String {
    match base {
        Some(b) => format!("{b}\n\n# Scope: {}\n\n{}", p.scope.name(), full_addendum(p)),
        None => format!("# Scope: {}\n\n{}", p.scope.name(), full_addendum(p)),
    }
}

pub fn mcp_tool_allowed(scope: Scope, tool_name: &str) -> bool {
    matches!(scope, Scope::Coding | Scope::Research) && tool_name.starts_with("mcp__")
}

pub fn tool_allowed(p: &Profile, tool_name: &str) -> bool {
    match p.allowed_tools {
        None => true,
        Some(list) => list.contains(&tool_name) || mcp_tool_allowed(p.scope, tool_name),
    }
}

pub fn tool_allowed_with_extra(p: &Profile, extra: &[String], tool_name: &str) -> bool {
    tool_allowed(p, tool_name) || extra.iter().any(|name| name == tool_name)
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

    #[test]
    fn web_search_is_available_in_non_mutating_scopes() {
        for scope in [Scope::Coding, Scope::Research, Scope::Plan] {
            assert!(tool_allowed(&profile(scope), "web_search"));
            assert!(tool_allowed(&profile(scope), "darash_search"));
        }
        assert!(!tool_allowed(&profile(Scope::Ask), "web_search"));
    }

    #[test]
    fn coding_and_research_allow_discovered_mcp_tools() {
        for scope in [Scope::Coding, Scope::Research] {
            let p = profile(scope);
            assert!(tool_allowed(&p, "mcp__fs__read_file"), "{scope}");
            assert!(tool_allowed(&p, "mcp__github__list_issues"), "{scope}");
        }
        assert!(!tool_allowed(&profile(Scope::Plan), "mcp__fs__read_file"));
        assert!(!tool_allowed(&profile(Scope::Ask), "mcp__fs__read_file"));
        assert!(!tool_allowed(
            &profile(Scope::ComputerUse),
            "mcp__fs__read_file"
        ));
    }

    #[test]
    fn extra_tools_extend_the_scope_allowlist() {
        let p = profile(Scope::Ask);
        assert!(!tool_allowed(&p, "custom_host_tool"));
        assert!(tool_allowed_with_extra(
            &p,
            &["custom_host_tool".into()],
            "custom_host_tool"
        ));
    }

    #[test]
    fn compose_prompt_formats_correctly() {
        let p = profile(Scope::Coding);

        let with_base = compose_prompt(Some("Base prompt"), &p);
        assert_eq!(
            with_base,
            format!("Base prompt\n\n# Scope: coding\n\n{}", full_addendum(&p))
        );

        let without_base = compose_prompt(None, &p);
        assert_eq!(
            without_base,
            format!("# Scope: coding\n\n{}", full_addendum(&p))
        );
    }

    #[test]
    #[cfg(feature = "script")]
    fn script_guidance_follows_the_registered_tool() {
        for scope in [Scope::Coding, Scope::Research] {
            let p = profile(scope);
            assert!(tool_allowed(&p, "script"), "{scope}");
            let prompt = compose_prompt(None, &p);
            assert!(
                prompt.contains("prefer the `script` tool (sandboxed LuaJIT)"),
                "{scope}: {prompt}"
            );
        }
        // Ask has tools off, so the guidance must not appear there.
        let prompt = compose_prompt(None, &profile(Scope::Ask));
        assert!(!prompt.contains("prefer the `script` tool"), "{prompt}");
    }

    #[test]
    #[cfg(not(feature = "script"))]
    fn script_guidance_absent_without_the_feature() {
        for scope in [Scope::Coding, Scope::Research] {
            assert!(!compose_prompt(None, &profile(scope)).contains("prefer the `script` tool"));
        }
    }
}
