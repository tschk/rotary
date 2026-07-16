//! Pi JSONL v3 session format — entry types, header, persistence.
//!
//! Compatible with pi_agent_rust session format:
//! - Location: ~/.pi/agent/sessions/--encoded-project-path--/
//! - Filename: YYYY-MM-DDTHH-MM-SS.sssZ_id.jsonl
//! - Format: JSON Lines (header + entries)

use crate::provider::{Message, Role};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Session header — first line of the JSONL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiSessionHeader {
    pub version: u32,
    pub id: String,
    pub project: String,
    pub created: DateTime<Utc>,
    pub model: String,
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl PiSessionHeader {
    pub fn new(project: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            version: crate::pi::PI_SESSION_VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            project: project.into(),
            created: Utc::now(),
            model: model.into(),
            provider: None,
            label: None,
        }
    }
}

/// Entry types in a pi session (pi_agent_rust SessionEntry).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PiEntryType {
    #[serde(rename = "message")]
    Message {
        role: Role,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
    },
    #[serde(rename = "model_change")]
    ModelChange { from: String, to: String },
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange { level: String },
    #[serde(rename = "compaction")]
    Compaction { summary: String, cut_at: usize },
    #[serde(rename = "branch_summary")]
    BranchSummary { from_session: String, at_entry: u64 },
    #[serde(rename = "session_info")]
    SessionInfo { key: String, value: String },
    #[serde(rename = "label")]
    Label { text: String },
    #[serde(rename = "custom")]
    Custom {
        extension: String,
        payload: serde_json::Value,
    },
}

/// A single entry in the session log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiEntry {
    #[serde(flatten)]
    pub entry_type: PiEntryType,
    pub timestamp: DateTime<Utc>,
    pub id: u64,
    pub parent_id: Option<u64>,
}

/// Pi-format session — JSONL v3 with typed entries and tree structure.
pub struct PiSession {
    pub header: PiSessionHeader,
    pub entries: Vec<PiEntry>,
    next_id: u64,
}

impl PiSession {
    pub fn new(project: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            header: PiSessionHeader::new(project, model),
            entries: Vec::new(),
            next_id: 1,
        }
    }

    pub fn append(&mut self, entry_type: PiEntryType) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let parent = self.entries.last().map(|e| e.id);
        self.entries.push(PiEntry {
            entry_type,
            timestamp: Utc::now(),
            id,
            parent_id: parent,
        });
        id
    }

    pub fn append_message(&mut self, role: Role, content: impl Into<String>) -> u64 {
        self.append(PiEntryType::Message {
            role,
            content: content.into(),
            tool_call_id: None,
        })
    }

    pub fn append_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
    ) -> u64 {
        self.append(PiEntryType::Message {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
        })
    }

    pub fn append_model_change(&mut self, from: impl Into<String>, to: impl Into<String>) -> u64 {
        self.append(PiEntryType::ModelChange {
            from: from.into(),
            to: to.into(),
        })
    }

    pub fn append_compaction(&mut self, summary: impl Into<String>, cut_at: usize) -> u64 {
        self.append(PiEntryType::Compaction {
            summary: summary.into(),
            cut_at,
        })
    }

    pub fn append_label(&mut self, text: impl Into<String>) -> u64 {
        self.append(PiEntryType::Label { text: text.into() })
    }

    /// Fork the session from a specific entry (pi branching).
    pub fn fork(&self, from_entry: u64) -> Self {
        let mut forked = Self::new(self.header.project.clone(), self.header.model.clone());
        forked.header.id = uuid::Uuid::new_v4().to_string();
        forked.header.label = Some(format!("fork of {} at {}", self.header.id, from_entry));

        for entry in &self.entries {
            forked.entries.push(PiEntry {
                entry_type: clone_entry_type(&entry.entry_type),
                timestamp: entry.timestamp,
                id: entry.id,
                parent_id: entry.parent_id,
            });
            if entry.id == from_entry {
                break;
            }
        }
        forked.next_id = self.next_id;
        forked
    }

    /// Save as JSONL v3 (header on first line, entries follow).
    pub fn save_jsonl(&self, dir: &Path) -> std::io::Result<std::path::PathBuf> {
        std::fs::create_dir_all(dir)?;
        let filename = format!(
            "{}_{}.jsonl",
            self.header.created.format("%Y-%m-%dT%H-%M-%S%.3fZ"),
            &self.header.id[..8]
        );
        let path = dir.join(filename);
        let mut content = String::new();
        content.push_str(&serde_json::to_string(&self.header).unwrap());
        content.push('\n');
        for entry in &self.entries {
            content.push_str(&serde_json::to_string(entry).unwrap());
            content.push('\n');
        }
        std::fs::write(&path, content)?;
        Ok(path)
    }

    /// Load a JSONL v3 session file.
    pub fn load_jsonl(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut lines = content.lines();
        let header_line = lines.next().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "empty session file")
        })?;
        let header: PiSessionHeader = serde_json::from_str(header_line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let mut entries = Vec::new();
        let mut next_id = 1u64;
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<PiEntry>(line) {
                if entry.id >= next_id {
                    next_id = entry.id + 1;
                }
                entries.push(entry);
            }
        }

        Ok(Self {
            header,
            entries,
            next_id,
        })
    }

    /// Convert entries to provider Messages for the agent loop.
    pub fn messages(&self) -> Vec<Message> {
        self.entries
            .iter()
            .filter_map(|e| match &e.entry_type {
                PiEntryType::Message {
                    role,
                    content,
                    tool_call_id,
                } => {
                    if let Some(tid) = tool_call_id {
                        Some(Message::tool(tid, content.clone()))
                    } else {
                        Some(Message::new(*role, content.clone()))
                    }
                }
                _ => None,
            })
            .collect()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn message_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.entry_type, PiEntryType::Message { .. }))
            .count()
    }
}

fn clone_entry_type(et: &PiEntryType) -> PiEntryType {
    match et {
        PiEntryType::Message {
            role,
            content,
            tool_call_id,
        } => PiEntryType::Message {
            role: *role,
            content: content.clone(),
            tool_call_id: tool_call_id.clone(),
        },
        PiEntryType::ModelChange { from, to } => PiEntryType::ModelChange {
            from: from.clone(),
            to: to.clone(),
        },
        PiEntryType::ThinkingLevelChange { level } => PiEntryType::ThinkingLevelChange {
            level: level.clone(),
        },
        PiEntryType::Compaction { summary, cut_at } => PiEntryType::Compaction {
            summary: summary.clone(),
            cut_at: *cut_at,
        },
        PiEntryType::BranchSummary {
            from_session,
            at_entry,
        } => PiEntryType::BranchSummary {
            from_session: from_session.clone(),
            at_entry: *at_entry,
        },
        PiEntryType::SessionInfo { key, value } => PiEntryType::SessionInfo {
            key: key.clone(),
            value: value.clone(),
        },
        PiEntryType::Label { text } => PiEntryType::Label { text: text.clone() },
        PiEntryType::Custom { extension, payload } => PiEntryType::Custom {
            extension: extension.clone(),
            payload: payload.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn session_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut s = PiSession::new("/test/project", "gpt-4o");
        s.append_message(Role::User, "hello");
        s.append_message(Role::Assistant, "hi there");
        s.append_label("test-label");

        let path = s.save_jsonl(tmp.path()).unwrap();
        let loaded = PiSession::load_jsonl(&path).unwrap();
        assert_eq!(loaded.header.model, "gpt-4o");
        assert_eq!(loaded.entry_count(), 3);
        assert_eq!(loaded.message_count(), 2);
    }

    #[test]
    fn fork_preserves_prefix() {
        let mut s = PiSession::new("/test", "gpt-4o");
        s.append_message(Role::User, "first");
        let fork_point = s.append_message(Role::Assistant, "second");
        s.append_message(Role::User, "third");

        let forked = s.fork(fork_point);
        assert_eq!(forked.entry_count(), 2);
    }

    #[test]
    fn messages_extracts_only_messages() {
        let mut s = PiSession::new("/test", "gpt-4o");
        s.append_message(Role::User, "hello");
        s.append_model_change("gpt-4o", "claude-3");
        s.append_message(Role::Assistant, "hi");

        let msgs = s.messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
    }
}
