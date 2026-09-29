//! A small, opt-in composition builder for a general-purpose agent.
//!
//! [`HarnessBuilder`] returns the ordinary [`crate::Agent`]. It does not own
//! provider selection, authorization, hooks, persistence, or lifecycle; hosts
//! attach those capabilities to the returned agent as needed.

use crate::{register_builtin_tools, Agent, Policy, Scope, TodoConfig, ToolRegistry};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Default instructions for a general-purpose agent.
pub const GENERIC_AGENT_CONTRACT: &str = "You are a careful general-purpose agent.\n\nBefore acting, explore the relevant context and constraints; do not assume files, state, or outcomes. Prefer the smallest change that satisfies the request.\n\nBefore an irreversible, destructive, externally visible, or costly action, explain the action and obtain confirmation from the host or user. Respect the host's authorization and lifecycle controls.\n\nVerify completed work with the strongest practical evidence. Report what you verified, what remains unverified, and why.";

/// The builtins selected by a [`HarnessBuilder`].
///
/// `Default` is a portable coding loadout. It includes only tools whose Cargo
/// features are enabled. `Named` is deliberate: requesting an unknown,
/// duplicate, or feature-disabled tool makes [`HarnessBuilder::build`] fail.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "names")]
pub enum BuiltinToolSelection {
    #[default]
    Default,
    Named(Vec<String>),
}

/// Serializable defaults for [`HarnessBuilder`].
///
/// This intentionally contains only data that hosts may want to store or
/// transport. Providers, approvers, hooks, sessions, and persistence remain
/// host-owned capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessConfig {
    #[serde(default = "default_scope")]
    pub scope: Scope,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default)]
    pub builtin_tools: BuiltinToolSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default = "default_todo_config")]
    pub todo_config: Option<TodoConfig>,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            scope: default_scope(),
            policy: Policy::default(),
            builtin_tools: BuiltinToolSelection::default(),
            instructions: None,
            system_prompt: None,
            todo_config: default_todo_config(),
        }
    }
}

fn default_scope() -> Scope {
    Scope::Coding
}

fn default_todo_config() -> Option<TodoConfig> {
    Some(TodoConfig::default())
}

/// Build a preassembled, ordinary [`Agent`] without taking over host policy.
#[derive(Debug, Clone, Default)]
pub struct HarnessBuilder {
    config: HarnessConfig,
}

impl HarnessBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_config(config: HarnessConfig) -> Self {
        Self { config }
    }

    pub fn scope(mut self, scope: Scope) -> Self {
        self.config.scope = scope;
        self
    }

    pub fn policy(mut self, policy: Policy) -> Self {
        self.config.policy = policy;
        self
    }

    /// Append caller instructions after the generic contract.
    ///
    /// Ignored when [`Self::system_prompt`] supplies a complete replacement.
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.config.instructions = Some(instructions.into());
        self
    }

    /// Replace the generic contract and caller instructions with a full prompt.
    ///
    /// The `Agent` still adds its selected scope addendum, as it does for every
    /// prompt configured through [`Agent::set_system_prompt`].
    pub fn system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.config.system_prompt = Some(system_prompt.into());
        self
    }

    /// Select exactly which existing builtin tools to register.
    pub fn builtin_tools<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.config.builtin_tools =
            BuiltinToolSelection::Named(names.into_iter().map(Into::into).collect());
        self
    }

    /// Configure the opt-in engine-owned todo state, or pass `None` to disable it.
    pub fn todo_config(mut self, config: Option<TodoConfig>) -> Self {
        self.config.todo_config = config;
        self
    }

    pub fn build(self) -> Result<Agent, HarnessError> {
        let names = selected_builtin_tools(&self.config.builtin_tools)?;
        let tools = ToolRegistry::new();
        register_builtin_tools(&tools);
        for name in BUILTIN_TOOL_NAMES {
            if !names.contains(*name) {
                tools.remove(name);
            }
        }

        let mut agent = Agent::new();
        agent.set_tools(tools);
        agent.set_scope(self.config.scope);
        agent.set_policy(self.config.policy);
        if let Some(todo_config) = self.config.todo_config {
            agent.set_todo_config(todo_config);
        }
        agent.set_system_prompt(
            self.config
                .system_prompt
                .unwrap_or_else(|| compose_contract(self.config.instructions.as_deref())),
        );
        Ok(agent)
    }
}

/// Errors found before a partially configured agent is returned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HarnessError {
    #[error("unknown builtin tool: {0}")]
    UnknownBuiltinTool(String),
    #[error("builtin tool selected more than once: {0}")]
    DuplicateBuiltinTool(String),
    #[error("builtin tool `{tool}` requires the `{feature}` feature")]
    FeatureDisabled { tool: String, feature: &'static str },
}

const BUILTIN_TOOL_NAMES: &[&str] = &[
    "read",
    "write",
    "edit",
    "hashline_edit",
    "bash",
    "grep",
    "find",
    "ls",
    "web_fetch",
    "todo",
    "spawn_agent",
    "enter_plan_mode",
    "exit_plan_mode",
    "lsp_diagnostics",
    "lsp_definition",
    "lsp_references",
    "exec",
];

fn selected_builtin_tools(
    selection: &BuiltinToolSelection,
) -> Result<BTreeSet<&str>, HarnessError> {
    match selection {
        BuiltinToolSelection::Default => Ok(default_builtin_tools().into_iter().collect()),
        BuiltinToolSelection::Named(names) => {
            let mut selected = BTreeSet::new();
            for name in names {
                if !BUILTIN_TOOL_NAMES.contains(&name.as_str()) {
                    return Err(HarnessError::UnknownBuiltinTool(name.clone()));
                }
                if !selected.insert(name.as_str()) {
                    return Err(HarnessError::DuplicateBuiltinTool(name.clone()));
                }
                if let Some(feature) = disabled_feature(name) {
                    return Err(HarnessError::FeatureDisabled {
                        tool: name.clone(),
                        feature,
                    });
                }
            }
            Ok(selected)
        }
    }
}

fn default_builtin_tools() -> Vec<&'static str> {
    #[cfg(feature = "builtin-tools")]
    {
        let mut names = vec!["read", "write", "edit", "hashline_edit", "ls", "todo"];
        names.extend(["bash", "grep", "find"]);
        names
    }
    #[cfg(not(feature = "builtin-tools"))]
    {
        vec!["read", "write", "edit", "hashline_edit", "ls", "todo"]
    }
}

fn disabled_feature(name: &str) -> Option<&'static str> {
    match name {
        "bash" | "grep" | "find" if !cfg!(feature = "builtin-tools") => Some("builtin-tools"),
        "web_fetch" if !cfg!(feature = "providers") => Some("providers"),
        "lsp_diagnostics" | "lsp_definition" | "lsp_references" if !cfg!(feature = "ipc") => {
            Some("ipc")
        }
        _ => None,
    }
}

fn compose_contract(instructions: Option<&str>) -> String {
    match instructions.filter(|instructions| !instructions.trim().is_empty()) {
        Some(instructions) => {
            format!("{GENERIC_AGENT_CONTRACT}\n\n# Caller instructions\n\n{instructions}")
        }
        None => GENERIC_AGENT_CONTRACT.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepts_agent(_: Agent) {}

    #[test]
    fn build_returns_an_ordinary_agent_with_the_default_loadout() {
        let agent = HarnessBuilder::new().build().unwrap();
        accepts_agent(agent);

        let agent = HarnessBuilder::new().build().unwrap();
        assert_eq!(agent.scope, Scope::Coding);
        assert_eq!(agent.todo_config, Some(TodoConfig::default()));
        let mut expected = default_builtin_tools()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(agent.tools.names(), expected);
    }

    #[test]
    fn caller_instructions_follow_the_generic_contract() {
        let agent = HarnessBuilder::new()
            .instructions("Use the repository's existing test command.")
            .build()
            .unwrap();
        let prompt = agent.system_prompt.unwrap();

        assert!(prompt.contains(GENERIC_AGENT_CONTRACT));
        assert!(
            prompt.contains("# Caller instructions\n\nUse the repository's existing test command.")
        );
        assert!(
            prompt.find(GENERIC_AGENT_CONTRACT).unwrap()
                < prompt
                    .find("Use the repository's existing test command.")
                    .unwrap()
        );
    }

    #[test]
    fn full_system_prompt_replaces_the_contract_and_instructions() {
        let agent = HarnessBuilder::new()
            .instructions("This must not be appended.")
            .system_prompt("Host-owned complete prompt.")
            .build()
            .unwrap();
        let prompt = agent.system_prompt.unwrap();

        assert!(prompt.starts_with("Host-owned complete prompt."));
        assert!(!prompt.contains(GENERIC_AGENT_CONTRACT));
        assert!(!prompt.contains("This must not be appended."));
    }

    #[test]
    fn named_builtin_selection_is_exact_and_rejects_collisions_and_unknown_names() {
        let agent = HarnessBuilder::new()
            .builtin_tools(["read", "write"])
            .build()
            .unwrap();
        assert_eq!(agent.tools.names(), ["read", "write"]);

        match HarnessBuilder::new()
            .builtin_tools(["read", "read"])
            .build()
        {
            Err(error) => assert_eq!(error, HarnessError::DuplicateBuiltinTool("read".into())),
            Ok(_) => panic!("duplicate builtin tool selection must fail"),
        }
        match HarnessBuilder::new()
            .builtin_tools(["not_a_builtin"])
            .build()
        {
            Err(error) => assert_eq!(
                error,
                HarnessError::UnknownBuiltinTool("not_a_builtin".into())
            ),
            Ok(_) => panic!("unknown builtin tool selection must fail"),
        }
    }

    #[cfg(not(feature = "providers"))]
    #[test]
    fn feature_disabled_tools_fail_during_build() {
        match HarnessBuilder::new().builtin_tools(["web_fetch"]).build() {
            Err(error) => assert_eq!(
                error,
                HarnessError::FeatureDisabled {
                    tool: "web_fetch".into(),
                    feature: "providers",
                }
            ),
            Ok(_) => panic!("feature-disabled builtin tool selection must fail"),
        }
    }

    #[cfg(not(feature = "builtin-tools"))]
    #[test]
    fn builtin_feature_tools_fail_during_build() {
        match HarnessBuilder::new().builtin_tools(["bash"]).build() {
            Err(error) => assert_eq!(
                error,
                HarnessError::FeatureDisabled {
                    tool: "bash".into(),
                    feature: "builtin-tools",
                }
            ),
            Ok(_) => panic!("feature-disabled builtin tool selection must fail"),
        }
    }

    #[cfg(not(feature = "ipc"))]
    #[test]
    fn ipc_feature_tools_fail_during_build() {
        match HarnessBuilder::new()
            .builtin_tools(["lsp_diagnostics"])
            .build()
        {
            Err(error) => assert_eq!(
                error,
                HarnessError::FeatureDisabled {
                    tool: "lsp_diagnostics".into(),
                    feature: "ipc",
                }
            ),
            Ok(_) => panic!("feature-disabled builtin tool selection must fail"),
        }
    }

    #[test]
    fn config_round_trips_without_runtime_capabilities() {
        let config = HarnessConfig {
            scope: Scope::Research,
            policy: Policy::read_only(),
            builtin_tools: BuiltinToolSelection::Named(vec!["read".into()]),
            instructions: Some("Cite sources.".into()),
            system_prompt: None,
            todo_config: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        let round_tripped = serde_json::from_str::<HarnessConfig>(&json).unwrap();
        assert_eq!(serde_json::to_string(&round_tripped).unwrap(), json);
        assert!(!json.contains("provider"));
        assert!(!json.contains("approver"));
    }
}
