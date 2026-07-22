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
        // Validate ID before it becomes a filename.
        crate::tools::common::validate_identifier(&self.id)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
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

    /// Export Codex/rollout-friendly JSONL (one object per line).
    /// Lines: session meta, then message events with role/content/timestamp.
    pub fn export_codex_jsonl(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = String::new();
        let meta = serde_json::json!({
            "type": "session_meta",
            "id": self.id,
            "name": self.name,
            "format": "rx4-codex-jsonl-v1",
        });
        out.push_str(&meta.to_string());
        out.push('\n');
        for entry in &self.entries {
            let line = serde_json::json!({
                "type": "message",
                "id": entry.id,
                "parent_id": entry.parent_id,
                "role": entry.role.to_string(),
                "content": entry.content,
            });
            out.push_str(&line.to_string());
            out.push('\n');
        }
        std::fs::write(path, out)
    }

    /// Import from Codex/rollout-friendly JSONL produced by [`Self::export_codex_jsonl`]
    /// or a plain message stream with `role` + `content` fields.
    pub fn import_codex_jsonl(path: &std::path::Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let fallback_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "imported".into());
        let mut id = fallback_id.clone();
        let mut name = fallback_id;
        let mut session = Self::new(id.clone(), name.clone());
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if ty == "session_meta" {
                if let Some(s) = v.get("id").and_then(|x| x.as_str()) {
                    // Validate imported ID before accepting it.
                    if let Err(e) = crate::tools::common::validate_identifier(s) {
                        tracing::warn!("rejecting malicious session id '{s}': {e}");
                        continue;
                    }
                    id = s.to_string();
                    session.id = id.clone();
                }
                if let Some(s) = v.get("name").and_then(|x| x.as_str()) {
                    name = s.to_string();
                    session.name = name.clone();
                }
                continue;
            }
            // Accept typed message lines or bare role/content lines.
            if ty == "message" || v.get("role").is_some() {
                let role_str = v.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                let role = match role_str {
                    "assistant" => Role::Assistant,
                    "system" => Role::System,
                    "tool" => Role::Tool,
                    _ => Role::User,
                };
                let text = v
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(eid) = v.get("id").and_then(|x| x.as_u64()) {
                    let parent = v.get("parent_id").and_then(|x| x.as_u64());
                    if eid >= session.next_id {
                        session.next_id = eid + 1;
                    }
                    session.entries.push(Entry {
                        id: eid,
                        parent_id: parent,
                        role,
                        content: text,
                    });
                } else {
                    session.append(role, text);
                }
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

    /// Persists this session into a SQLite database at `path`.
    #[cfg(feature = "sqlite-sessions")]
    pub fn save_sqlite(&self, path: &std::path::Path) -> Result<(), String> {
        use rusqlite::{params, Connection};

        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                next_id INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS entries (
                session_id TEXT NOT NULL,
                id INTEGER NOT NULL,
                parent_id INTEGER,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                PRIMARY KEY (session_id, id)
            );",
        )
        .map_err(|e| e.to_string())?;

        conn.execute(
            "INSERT OR REPLACE INTO sessions (id, name, next_id) VALUES (?1, ?2, ?3)",
            params![self.id, self.name, self.next_id as i64],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM entries WHERE session_id = ?1",
            params![self.id],
        )
        .map_err(|e| e.to_string())?;

        for entry in &self.entries {
            conn.execute(
                "INSERT INTO entries (session_id, id, parent_id, role, content)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    self.id,
                    entry.id as i64,
                    entry.parent_id.map(|p| p as i64),
                    entry.role.to_string(),
                    entry.content,
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Loads a session from a SQLite database at `path`.
    #[cfg(feature = "sqlite-sessions")]
    pub fn load_sqlite(path: &std::path::Path) -> Result<Self, String> {
        use rusqlite::{params, Connection};

        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        let (id, name, next_id): (String, String, i64) = conn
            .query_row(
                "SELECT id, name, next_id FROM sessions LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|e| e.to_string())?;

        let mut session = Self::new(id.clone(), name);
        session.next_id = next_id as u64;

        let mut stmt = conn
            .prepare(
                "SELECT id, parent_id, role, content FROM entries
                 WHERE session_id = ?1 ORDER BY id ASC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![id], |row| {
                let role_s: String = row.get(2)?;
                let role = match role_s.as_str() {
                    "system" => Role::System,
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    "tool" => Role::Tool,
                    other => {
                        return Err(rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("unknown role: {other}"),
                            )),
                        ));
                    }
                };
                Ok(Entry {
                    id: row.get::<_, i64>(0)? as u64,
                    parent_id: row.get::<_, Option<i64>>(1)?.map(|p| p as u64),
                    role,
                    content: row.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?;

        for row in rows {
            session.entries.push(row.map_err(|e| e.to_string())?);
        }
        Ok(session)
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

    #[test]
    fn codex_jsonl_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex.jsonl");
        let mut s = Session::new("codex1", "export-test");
        s.append(Role::User, "ping");
        s.append(Role::Assistant, "pong");
        s.export_codex_jsonl(&path).unwrap();
        let loaded = Session::import_codex_jsonl(&path).unwrap();
        assert_eq!(loaded.id, "codex1");
        assert_eq!(loaded.name, "export-test");
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].content, "ping");
        assert_eq!(loaded.entries[1].content, "pong");
    }

    #[cfg(feature = "sqlite-sessions")]
    #[test]
    fn sqlite_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.db");
        let mut s = Session::new("s1", "test");
        s.append(Role::User, "hello");
        s.append(Role::Assistant, "hi");
        s.save_sqlite(&path).unwrap();

        let loaded = Session::load_sqlite(&path).unwrap();
        assert_eq!(loaded.id, "s1");
        assert_eq!(loaded.name, "test");
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].content, "hello");
        assert_eq!(loaded.entries[1].role, Role::Assistant);
        assert_eq!(loaded.next_id, s.next_id);
    }
}
