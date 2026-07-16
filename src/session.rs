//! Session: conversation tree with fork/merge/persist (JSONL).

use crate::provider::{Message, Role};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: u64,
    pub parent_id: Option<u64>,
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub entries: Vec<Entry>,
    next_id: u64,
}

impl Session {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            entries: Vec::new(),
            next_id: 1,
        }
    }

    pub fn append(&mut self, role: Role, content: impl Into<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let parent = self.entries.last().map(|e| e.id);
        self.entries.push(Entry {
            id,
            parent_id: parent,
            role,
            content: content.into(),
        });
        id
    }

    pub fn fork(&self, from_entry: u64) -> Self {
        let mut forked = Self::new(format!("{}-fork", self.id), format!("{} (fork)", self.name));
        for entry in &self.entries {
            forked.entries.push(Entry {
                id: entry.id,
                parent_id: entry.parent_id,
                role: entry.role,
                content: entry.content.clone(),
            });
            if entry.id == from_entry {
                break;
            }
        }
        forked.next_id = self.next_id;
        forked
    }

    pub fn merge(&mut self, other: &Self) -> usize {
        let start = self.next_id;
        for entry in &other.entries {
            self.append(entry.role, entry.content.clone());
        }
        (self.next_id - start) as usize
    }

    pub fn save_jsonl(&self, dir: &std::path::Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.jsonl", self.id));
        let mut content = String::new();
        for entry in &self.entries {
            content.push_str(&serde_json::to_string(entry).unwrap());
            content.push('\n');
        }
        std::fs::write(&path, content)?;
        Ok(path)
    }

    pub fn load_jsonl(path: &std::path::Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let id = path.file_stem().unwrap().to_string_lossy().to_string();
        let mut session = Self::new(id.clone(), id);
        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<Entry>(line) {
                if entry.id >= session.next_id {
                    session.next_id = entry.id + 1;
                }
                session.entries.push(entry);
            }
        }
        Ok(session)
    }

    pub fn messages(&self) -> Vec<Message> {
        self.entries
            .iter()
            .map(|e| Message::new(e.role, e.content.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_fork() {
        let mut s = Session::new("s1", "test");
        s.append(Role::User, "hello");
        s.append(Role::Assistant, "hi");
        let forked = s.fork(1);
        assert_eq!(forked.entries.len(), 1);
        assert_eq!(forked.entries[0].content, "hello");
    }
}
