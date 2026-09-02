//! Session: conversation tree with fork/merge/persist (JSONL).

use crate::provider::{Message, Role};
use crate::todo::TodoState;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;

/// Maximum file size in bytes for session JSONL files (10 MB).
/// Rejects files larger than this to prevent unbounded memory allocation on import.
const MAX_SESSION_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Securely read a session file, preventing unbounded memory allocation from
/// arbitrary user-provided paths (like `/dev/zero`) and blocking on named pipes.
fn read_limited_file(path: &std::path::Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;

    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }

    if meta.len() > MAX_SESSION_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "session file too large: {} bytes (max {MAX_SESSION_FILE_BYTES})",
                meta.len()
            ),
        ));
    }

    let mut content = String::new();
    file.take(MAX_SESSION_FILE_BYTES + 1)
        .read_to_string(&mut content)?;

    if content.len() as u64 > MAX_SESSION_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file grew too large while reading",
        ));
    }

    Ok(content)
}

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
    /// Host-visible todo state persisted with the session.
    #[serde(default)]
    pub todos: TodoState,
    next_id: u64,
}

impl Session {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            entries: Vec::new(),
            todos: TodoState::default(),
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
        let redactor = crate::secrets::Redactor::new();
        for entry in &self.entries {
            let mut safe_entry = entry.clone();
            safe_entry.content = redactor.redact(&safe_entry.content);
            content.push_str(&serde_json::to_string(&safe_entry).unwrap());
            content.push('\n');
        }
        content.push_str(
            &serde_json::json!({"type": "session_todos", "todos": self.todos}).to_string(),
        );
        content.push('\n');
        std::fs::write(&path, content)?;
        Ok(path)
    }

    pub fn load_jsonl(path: &std::path::Path) -> std::io::Result<Self> {
        let content = read_limited_file(path)?;
        let id = path.file_stem().unwrap().to_string_lossy().to_string();
        let mut session = Self::new(id.clone(), id);
        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if value.get("type").and_then(|value| value.as_str()) == Some("session_todos") {
                if let Some(todos) = value.get("todos") {
                    if let Ok(todos) = serde_json::from_value(todos.clone()) {
                        session.todos = todos;
                    }
                }
            } else if let Ok(entry) = serde_json::from_value::<Entry>(value) {
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
        let redactor = crate::secrets::Redactor::new();
        let meta = serde_json::json!({
            "type": "session_meta",
            "id": self.id,
            "name": self.name,
            "format": "rx4-codex-jsonl-v1",
        });
        out.push_str(&meta.to_string());
        out.push('\n');
        out.push_str(
            &serde_json::json!({"type": "session_todos", "todos": self.todos}).to_string(),
        );
        out.push('\n');
        for entry in &self.entries {
            let safe_content = redactor.redact(&entry.content);
            let line = serde_json::json!({
                "type": "message",
                "id": entry.id,
                "parent_id": entry.parent_id,
                "role": entry.role.to_string(),
                "content": safe_content,
            });
            out.push_str(&line.to_string());
            out.push('\n');
        }
        std::fs::write(path, out)
    }

    /// Import from Codex/rollout-friendly JSONL produced by [`Self::export_codex_jsonl`]
    /// or a plain message stream with `role` + `content` fields.
    pub fn import_codex_jsonl(path: &std::path::Path) -> std::io::Result<Self> {
        let content = read_limited_file(path)?;
        let fallback_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "imported".into());
        let mut session = Self::new(fallback_id.clone(), fallback_id);
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            session.process_codex_line(&v);
        }
        Ok(session)
    }

    fn process_codex_line(&mut self, v: &serde_json::Value) {
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty == "session_meta" {
            self.process_session_meta(v);
        } else if ty == "session_todos" {
            self.process_session_todos(v);
        } else if ty == "message" || v.get("role").is_some() {
            self.process_message(v);
        }
    }

    fn process_session_meta(&mut self, v: &serde_json::Value) {
        if let Some(s) = v.get("id").and_then(|x| x.as_str()) {
            if let Err(e) = crate::tools::common::validate_identifier(s) {
                tracing::warn!("rejecting malicious session id '{s}': {e}");
            } else {
                self.id = s.to_string();
            }
        }
        if let Some(s) = v.get("name").and_then(|x| x.as_str()) {
            self.name = s.to_string();
        }
    }

    fn process_session_todos(&mut self, v: &serde_json::Value) {
        if let Some(todos) = v.get("todos") {
            if let Ok(todos) = serde_json::from_value(todos.clone()) {
                self.todos = todos;
            }
        }
    }

    fn process_message(&mut self, v: &serde_json::Value) {
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
            if eid >= self.next_id {
                self.next_id = eid + 1;
            }
            self.entries.push(Entry {
                id: eid,
                parent_id: parent,
                role,
                content: text,
            });
        } else {
            self.append(role, text);
        }
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

        let mut conn = Connection::open(path).map_err(|e| e.to_string())?;
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

        let tx = conn.transaction().map_err(|e| e.to_string())?;

        tx.execute(
            "INSERT OR REPLACE INTO sessions (id, name, next_id) VALUES (?1, ?2, ?3)",
            params![self.id, self.name, self.next_id as i64],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM entries WHERE session_id = ?1",
            params![self.id],
        )
        .map_err(|e| e.to_string())?;

        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO entries (session_id, id, parent_id, role, content)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(|e| e.to_string())?;

            let redactor = crate::secrets::Redactor::new();
            for entry in &self.entries {
                let safe_content = redactor.redact(&entry.content);
                stmt.execute(params![
                    self.id,
                    entry.id as i64,
                    entry.parent_id.map(|p| p as i64),
                    entry.role.to_string(),
                    safe_content,
                ])
                .map_err(|e| e.to_string())?;
            }
        }

        tx.commit().map_err(|e| e.to_string())?;

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
    fn jsonl_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new("test_jsonl_session", "jsonl-test");
        s.append(Role::User, "hello");
        let secret = format!("sk-{}", "a".repeat(48));
        s.append(Role::Assistant, format!("data: {}", secret));

        let path = s.save_jsonl(dir.path()).unwrap();

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains(&secret));
        assert!(on_disk.contains("[REDACTED:api-key]"));

        let loaded = Session::load_jsonl(&path).unwrap();
        assert_eq!(loaded.id, "test_jsonl_session");
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].content, "hello");
        assert_eq!(loaded.entries[1].content, "data: [REDACTED:api-key]");
        assert!(loaded.todos.items.is_empty());
    }

    #[test]
    fn save_jsonl_invalid_id() {
        let dir = tempfile::tempdir().unwrap();
        let s = Session::new("../invalid", "test");
        let err = s.save_jsonl(dir.path()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn append_and_fork() {
        let mut s = Session::new("s1", "test");
        let id1 = s.append(Role::System, "sys");
        let id2 = s.append(Role::User, "hello");
        let id3 = s.append(Role::Assistant, "hi");

        // Forking from an intermediate entry
        let forked1 = s.fork(id2);
        assert_eq!(forked1.id, "s1-fork");
        assert_eq!(forked1.name, "test (fork)");
        assert_eq!(forked1.next_id, s.next_id);
        assert_eq!(forked1.entries.len(), 2);
        assert_eq!(forked1.entries[0].id, id1);
        assert_eq!(forked1.entries[0].content, "sys");
        assert_eq!(forked1.entries[1].id, id2);
        assert_eq!(forked1.entries[1].content, "hello");

        // Forking from a non-existent entry ID copies all entries
        let forked2 = s.fork(999);
        assert_eq!(forked2.id, "s1-fork");
        assert_eq!(forked2.name, "test (fork)");
        assert_eq!(forked2.next_id, s.next_id);
        assert_eq!(forked2.entries.len(), 3);
        assert_eq!(forked2.entries[2].id, id3);
        assert_eq!(forked2.entries[2].content, "hi");
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
        assert!(loaded.todos.items.is_empty());
        assert_eq!(loaded.entries[1].content, "pong");
    }

    #[test]
    fn persistence_redacts_secrets_without_mutating_session() {
        let dir = tempfile::tempdir().unwrap();
        let secret = format!("sk-{}", "a".repeat(48));
        let mut s = Session::new("safe", "redaction-test");
        s.append(Role::Assistant, format!("token {secret}"));
        let path = s.save_jsonl(dir.path()).unwrap();
        let on_disk = std::fs::read_to_string(path).unwrap();
        assert!(!on_disk.contains(&secret));
        assert!(on_disk.contains("[REDACTED:api-key]"));
        assert!(s.entries[0].content.contains(&secret));
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
