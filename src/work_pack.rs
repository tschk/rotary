//! Specialist work packs as markdown data (not hard-coded agent names).
//!
//! A pack file:
//! ```text
//! ---
//! name: reviewer
//! role: reviewer
//! tools: read, grep, find
//! permission: read_only
//! ---
//! # Instructions
//! Review diffs for correctness and security.
//! ```

use crate::permissions::{PermissionMode, Policy};
use crate::subagent::SubagentConfig;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Errors loading or parsing work packs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkPackError {
    #[error("io: {0}")]
    Io(String),
    #[error("parse: {0}")]
    Parse(String),
}

/// A specialist work pack loaded from markdown + optional YAML frontmatter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkPack {
    pub name: String,
    pub role: String,
    pub tools: Vec<String>,
    pub permission: PermissionMode,
    pub instructions: String,
}

impl WorkPack {
    /// Parse a markdown work pack from string content.
    pub fn parse(content: &str) -> Result<Self, WorkPackError> {
        let (front, body) = split_frontmatter(content);
        let mut name = String::new();
        let mut role = "worker".to_string();
        let mut tools: Vec<String> = Vec::new();
        let mut permission = PermissionMode::WorkspaceWrite;

        for line in front.lines() {
            let line = line.trim();
            if line.is_empty() || line == "---" {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim().to_ascii_lowercase();
                let v = v.trim().trim_matches('"').trim_matches('\'');
                match k.as_str() {
                    "name" => name = v.to_string(),
                    "role" => role = v.to_string(),
                    "tools" => {
                        tools = v
                            .split([',', ' '])
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                            .collect();
                    }
                    "permission" | "permission_mode" => {
                        permission = match v.to_ascii_lowercase().as_str() {
                            "full_access" | "full" => PermissionMode::FullAccess,
                            "read_only" | "readonly" | "read" => PermissionMode::ReadOnly,
                            "deny_all" | "deny" => PermissionMode::DenyAll,
                            _ => PermissionMode::WorkspaceWrite,
                        };
                    }
                    _ => {}
                }
            }
        }

        if name.is_empty() {
            // Fallback: first markdown heading.
            for line in body.lines() {
                if let Some(h) = line.strip_prefix("# ") {
                    name = h.trim().to_string();
                    break;
                }
            }
        }
        if name.is_empty() {
            return Err(WorkPackError::Parse("missing pack name".into()));
        }

        let instructions = body.trim().to_string();
        if tools.is_empty() {
            tools = default_tools_for_role(&role);
        }

        Ok(Self {
            name,
            role,
            tools,
            permission,
            instructions,
        })
    }

    /// Load from a file path.
    pub fn load(path: &Path) -> Result<Self, WorkPackError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| WorkPackError::Io(e.to_string()))?;
        Self::parse(&content)
    }

    /// Load all `*.md` packs from a directory (non-recursive).
    pub fn load_dir(dir: &Path) -> Result<Vec<Self>, WorkPackError> {
        let mut packs = Vec::new();
        let entries = std::fs::read_dir(dir).map_err(|e| WorkPackError::Io(e.to_string()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                packs.push(Self::load(&path)?);
            }
        }
        packs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packs)
    }

    /// Convert to a [`SubagentConfig`] for spawning.
    pub fn to_subagent_config(&self) -> SubagentConfig {
        SubagentConfig {
            name: self.name.clone(),
            allowed_tools: Some(self.tools.clone()),
            denied_tools: None,
            permission_mode: Some(self.permission),
            system_prompt: Some(self.instructions.clone()),
            ..SubagentConfig::default()
        }
    }

    /// Policy derived from pack permission mode.
    pub fn policy(&self) -> Policy {
        match self.permission {
            PermissionMode::FullAccess => Policy::full_access(),
            PermissionMode::ReadOnly => Policy::read_only(),
            PermissionMode::DenyAll => Policy::deny_all(),
            PermissionMode::WorkspaceWrite => Policy::workspace_write(),
        }
    }
}

fn default_tools_for_role(role: &str) -> Vec<String> {
    match role.to_ascii_lowercase().as_str() {
        "reviewer" => vec!["read".into(), "grep".into(), "find".into()],
        "researcher" => vec!["read".into(), "grep".into(), "find".into(), "ls".into()],
        "coordinator" => vec!["read".into(), "ls".into()],
        _ => vec![
            "read".into(),
            "write".into(),
            "edit".into(),
            "bash".into(),
            "grep".into(),
            "find".into(),
            "ls".into(),
        ],
    }
}

fn split_frontmatter(content: &str) -> (String, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (String::new(), content.to_string());
    }
    let rest = trimmed.trim_start_matches("---");
    if let Some(end) = rest.find("\n---") {
        let front = rest[..end].to_string();
        let body = rest[end + 4..].trim_start_matches('\n').to_string();
        (front, body)
    } else {
        (String::new(), content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pack_with_frontmatter() {
        let md = r#"---
name: reviewer
role: reviewer
tools: read, grep
permission: read_only
---
# Review
Look for bugs.
"#;
        let pack = WorkPack::parse(md).unwrap();
        assert_eq!(pack.name, "reviewer");
        assert_eq!(pack.role, "reviewer");
        assert_eq!(pack.tools, vec!["read", "grep"]);
        assert_eq!(pack.permission, PermissionMode::ReadOnly);
        assert!(pack.instructions.contains("Look for bugs"));
        let cfg = pack.to_subagent_config();
        assert_eq!(cfg.name, "reviewer");
        assert_eq!(cfg.permission_mode, Some(PermissionMode::ReadOnly));
    }

    #[test]
    fn parse_heading_fallback_name() {
        let md = "# Security Auditor\n\nCheck auth paths.\n";
        let pack = WorkPack::parse(md).unwrap();
        assert_eq!(pack.name, "Security Auditor");
        assert!(!pack.tools.is_empty());
    }
}
