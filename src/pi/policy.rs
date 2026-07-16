//! Pi capability policy — 5-layer precedence chain, fail-closed security.
//!
//! Compatible with pi_agent_rust ExtensionPolicy:
//! - Modes: Strict (deny-by-default), Prompt (ask user), Permissive (allow + audit)
//! - Capabilities: exec, write, http, read, tool, session, ui
//! - 5-layer precedence: per-ext deny > global deny > per-ext allow > global default > mode fallback

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiPolicyMode {
    Strict,
    Prompt,
    Permissive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiCapability {
    Exec,
    Write,
    Http,
    Read,
    Tool,
    Session,
    Ui,
}

impl PiCapability {
    pub fn risk_level(&self) -> &'static str {
        match self {
            Self::Exec | Self::Write => "high",
            Self::Http | Self::Read | Self::Tool => "medium",
            Self::Session | Self::Ui => "low",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "exec" => Some(Self::Exec),
            "write" => Some(Self::Write),
            "http" => Some(Self::Http),
            "read" => Some(Self::Read),
            "tool" => Some(Self::Tool),
            "session" => Some(Self::Session),
            "ui" => Some(Self::Ui),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::Write => "write",
            Self::Http => "http",
            Self::Read => "read",
            Self::Tool => "tool",
            Self::Session => "session",
            Self::Ui => "ui",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiExtensionOverride {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub max_memory_mb: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiCapabilityPolicy {
    pub mode: PiPolicyMode,
    #[serde(default)]
    pub max_memory_mb: u32,
    #[serde(default)]
    pub default_caps: Vec<String>,
    #[serde(default)]
    pub deny_caps: Vec<String>,
    #[serde(default)]
    pub per_extension: HashMap<String, PiExtensionOverride>,
}

impl Default for PiCapabilityPolicy {
    fn default() -> Self {
        Self {
            mode: PiPolicyMode::Strict,
            max_memory_mb: 128,
            default_caps: vec!["read".into(), "session".into(), "ui".into()],
            deny_caps: vec![],
            per_extension: HashMap::new(),
        }
    }
}

impl PiCapabilityPolicy {
    pub fn strict() -> Self {
        Self {
            mode: PiPolicyMode::Strict,
            ..Default::default()
        }
    }

    pub fn permissive() -> Self {
        Self {
            mode: PiPolicyMode::Permissive,
            default_caps: vec![
                "read".into(),
                "write".into(),
                "exec".into(),
                "http".into(),
                "tool".into(),
                "session".into(),
                "ui".into(),
            ],
            ..Default::default()
        }
    }

    pub fn prompt() -> Self {
        Self {
            mode: PiPolicyMode::Prompt,
            ..Default::default()
        }
    }

    /// Check if an extension is allowed a capability.
    /// Implements the 5-layer precedence chain:
    /// 1. Per-extension deny (highest)
    /// 2. Global deny_caps
    /// 3. Per-extension allow
    /// 4. Global default_caps
    /// 5. Mode fallback (lowest)
    pub fn check(&self, extension_id: &str, cap: PiCapability) -> PolicyDecision {
        let cap_str = cap.as_str();

        // Layer 1: per-extension deny
        if let Some(ext) = self.per_extension.get(extension_id) {
            if ext.deny.iter().any(|c| c == cap_str) {
                return PolicyDecision::Deny;
            }
        }

        // Layer 2: global deny
        if self.deny_caps.iter().any(|c| c == cap_str) {
            return PolicyDecision::Deny;
        }

        // Layer 3: per-extension allow
        if let Some(ext) = self.per_extension.get(extension_id) {
            if ext.allow.iter().any(|c| c == cap_str) {
                return PolicyDecision::Allow;
            }
        }

        // Layer 4: global default caps
        if self.default_caps.iter().any(|c| c == cap_str) {
            return PolicyDecision::Allow;
        }

        // Layer 5: mode fallback
        match self.mode {
            PiPolicyMode::Strict => PolicyDecision::Deny,
            PiPolicyMode::Prompt => PolicyDecision::Ask,
            PiPolicyMode::Permissive => PolicyDecision::Allow,
        }
    }

    /// Load policy from a JSON file (pi format: ~/.pi/agent/extension_policy.json).
    pub fn load(path: &std::path::Path) -> Self {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(policy) = serde_json::from_str(&content) {
                return policy;
            }
        }
        Self::default()
    }

    /// Save policy to a JSON file.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).unwrap();
        std::fs::write(path, json)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny,
    Ask,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_denies_by_default() {
        let p = PiCapabilityPolicy::strict();
        assert_eq!(p.check("ext1", PiCapability::Exec), PolicyDecision::Deny);
        assert_eq!(p.check("ext1", PiCapability::Read), PolicyDecision::Allow);
    }

    #[test]
    fn permissive_allows_all() {
        let p = PiCapabilityPolicy::permissive();
        assert_eq!(p.check("ext1", PiCapability::Exec), PolicyDecision::Allow);
        assert_eq!(p.check("ext1", PiCapability::Write), PolicyDecision::Allow);
    }

    #[test]
    fn per_ext_deny_overrides() {
        let mut p = PiCapabilityPolicy::permissive();
        p.per_extension.insert(
            "evil".into(),
            PiExtensionOverride {
                allow: vec![],
                deny: vec!["exec".into()],
                max_memory_mb: None,
            },
        );
        assert_eq!(p.check("evil", PiCapability::Exec), PolicyDecision::Deny);
        assert_eq!(p.check("good", PiCapability::Exec), PolicyDecision::Allow);
    }

    #[test]
    fn global_deny_overrides_per_ext_allow() {
        let mut p = PiCapabilityPolicy::permissive();
        p.deny_caps.push("exec".into());
        p.per_extension.insert(
            "ext1".into(),
            PiExtensionOverride {
                allow: vec!["exec".into()],
                deny: vec![],
                max_memory_mb: None,
            },
        );
        assert_eq!(p.check("ext1", PiCapability::Exec), PolicyDecision::Deny);
    }

    #[test]
    fn prompt_mode_asks() {
        let p = PiCapabilityPolicy::prompt();
        assert_eq!(p.check("ext1", PiCapability::Exec), PolicyDecision::Ask);
        assert_eq!(p.check("ext1", PiCapability::Read), PolicyDecision::Allow);
    }
}
