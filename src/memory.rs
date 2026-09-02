//! Memory: SQLite FTS5 hybrid search (grok pattern).
//!
//! When `memory` feature is enabled, provides persistent memory with
//! full-text search and optional vector similarity.

pub mod policy;

#[cfg(feature = "memory")]
mod inner {
    use chrono::{DateTime, Utc};
    use parking_lot::Mutex;
    use rusqlite::{params, Connection};
    use std::path::Path;
    use thiserror::Error;

    /// Errors returned by memory store operations.
    #[derive(Debug, Error)]
    pub enum MemoryError {
        #[error("database error: {0}")]
        Database(String),
        #[error("memory not found")]
        NotFound,
        #[error("parse error: {0}")]
        Parse(String),
    }

    impl From<rusqlite::Error> for MemoryError {
        fn from(err: rusqlite::Error) -> Self {
            MemoryError::Database(err.to_string())
        }
    }

    /// A single stored memory entry.
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct MemoryEntry {
        pub id: String,
        pub key: String,
        pub content: String,
        pub source: String,
        pub created: DateTime<Utc>,
        pub accessed: DateTime<Utc>,
        pub access_count: i64,
    }

    impl MemoryEntry {
        fn try_from_row(row: &rusqlite::Row<'_>) -> Result<Self, MemoryError> {
            Ok(Self {
                id: row.get(0)?,
                key: row.get(1)?,
                content: row.get(2)?,
                source: row.get(3)?,
                created: parse_dt(&row.get::<_, String>(4)?)?,
                accessed: parse_dt(&row.get::<_, String>(5)?)?,
                access_count: row.get(6)?,
            })
        }
    }

    /// SQLite-backed memory store with FTS5 full-text search and hybrid ranking.
    pub struct MemoryStore {
        conn: Mutex<Connection>,
    }

    impl MemoryStore {
        /// Open or create a memory store at the given SQLite database path.
        pub fn new(db_path: impl AsRef<Path>) -> Result<Self, MemoryError> {
            let conn = Connection::open(db_path)?;
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;

            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS memories (
                    id TEXT PRIMARY KEY,
                    key TEXT,
                    content TEXT,
                    source TEXT,
                    created TEXT,
                    accessed TEXT,
                    access_count INTEGER DEFAULT 0
                );

                CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                    content,
                    content='memories',
                    content_rowid='rowid'
                );

                CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                    INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content);
                END;

                CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                    INSERT INTO memories_fts(memories_fts, rowid, content)
                        VALUES ('delete', old.rowid, old.content);
                END;

                CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
                    INSERT INTO memories_fts(memories_fts, rowid, content)
                        VALUES ('delete', old.rowid, old.content);
                    INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content);
                END;",
            )?;

            Ok(Self {
                conn: Mutex::new(conn),
            })
        }

        /// Store a new memory entry. Returns the generated UUID id.
        pub fn store(&self, key: &str, content: &str, source: &str) -> Result<String, MemoryError> {
            let id = uuid::Uuid::new_v4().to_string();
            let now = Utc::now();
            let created = now.to_rfc3339();
            let accessed = created.clone();

            let conn = self.conn.lock();
            conn.execute(
                "INSERT INTO memories (id, key, content, source, created, accessed, access_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
                params![id, key, content, source, created, accessed],
            )?;

            Ok(id)
        }

        /// Full-text search with BM25 ranking, boosted by access frequency.
        ///
        /// `final_score = -bm25(memories_fts) + ln(1 + access_count)`
        pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
            let conn = self.conn.lock();

            // Sanitize the query to prevent FTS syntax errors or SQL injection.
            let sanitized_query = sanitize_query(query);

            let mut stmt = conn.prepare(
                "SELECT m.id, m.key, m.content, m.source, m.created, m.accessed, m.access_count,
                        bm25(memories_fts) AS score
                 FROM memories_fts
                 JOIN memories m ON m.rowid = memories_fts.rowid
                 WHERE memories_fts MATCH ?1
                 ORDER BY score
                 LIMIT ?2",
            )?;

            let mut rows = stmt.query(params![sanitized_query, limit as i64])?;

            let mut scored: Vec<(MemoryEntry, f64)> = Vec::new();
            while let Some(row) = rows.next()? {
                let entry = MemoryEntry::try_from_row(row)?;
                let bm25_score: f64 = row.get(7)?;

                let boost = (1.0_f64 + entry.access_count as f64).ln();
                let final_score = -bm25_score + boost;

                scored.push((entry, final_score));
            }

            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            Ok(scored.into_iter().map(|(e, _)| e).collect())
        }

        /// Retrieve a memory by id.
        pub fn get(&self, id: &str) -> Result<Option<MemoryEntry>, MemoryError> {
            let conn = self.conn.lock();

            let mut stmt = conn.prepare(
                "SELECT id, key, content, source, created, accessed, access_count
                 FROM memories WHERE id = ?1",
            )?;

            let mut rows = stmt.query(params![id])?;
            match rows.next()? {
                Some(row) => Ok(Some(MemoryEntry::try_from_row(row)?)),
                None => Ok(None),
            }
        }

        /// Delete a memory by id.
        pub fn delete(&self, id: &str) -> Result<(), MemoryError> {
            let conn = self.conn.lock();
            let affected = conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
            if affected == 0 {
                return Err(MemoryError::NotFound);
            }
            Ok(())
        }

        /// Increment access count and update the accessed timestamp.
        pub fn touch(&self, id: &str) -> Result<(), MemoryError> {
            let now = Utc::now().to_rfc3339();
            let conn = self.conn.lock();
            let affected = conn.execute(
                "UPDATE memories SET accessed = ?1, access_count = access_count + 1 WHERE id = ?2",
                params![now, id],
            )?;
            if affected == 0 {
                return Err(MemoryError::NotFound);
            }
            Ok(())
        }
    }

    fn parse_dt(s: &str) -> Result<DateTime<Utc>, MemoryError> {
        DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| MemoryError::Parse(e.to_string()))
    }

    fn sanitize_query(query: &str) -> String {
        query
            .split_whitespace()
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect::<Vec<String>>()
            .join(" ")
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tempfile::NamedTempFile;

        fn store() -> MemoryStore {
            let f = NamedTempFile::new().unwrap();
            MemoryStore::new(f.path()).unwrap()
        }

        #[test]
        fn store_and_search() {
            let s = store();
            let id = s
                .store("note", "rust is a systems language", "test")
                .unwrap();
            let results = s.search("rust", 10).unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].id, id);
            assert_eq!(results[0].key, "note");
            assert_eq!(results[0].source, "test");
            assert_eq!(results[0].access_count, 0);
        }

        #[test]
        fn fts_ranking_relevant_first() {
            let s = store();
            s.store("a", "the rust programming language is fast", "test")
                .unwrap();
            s.store("b", "python is a scripting language", "test")
                .unwrap();
            s.store("c", "rust rust rust repeated terms", "test")
                .unwrap();

            let results = s.search("rust", 10).unwrap();
            assert!(!results.is_empty());
            assert_eq!(results[0].key, "c");
        }

        #[test]
        fn access_count_tracking() {
            let s = store();
            let id = s.store("note", "important memory content", "test").unwrap();
            s.touch(&id).unwrap();
            s.touch(&id).unwrap();

            let entry = s.get(&id).unwrap().unwrap();
            assert_eq!(entry.access_count, 2);
            assert!(entry.accessed > entry.created);
        }

        #[test]
        fn delete_removes_entry() {
            let s = store();
            let id = s.store("note", "to be deleted", "test").unwrap();
            assert!(s.get(&id).unwrap().is_some());
            s.delete(&id).unwrap();
            assert!(s.get(&id).unwrap().is_none());
        }

        #[test]
        fn delete_missing_is_not_found() {
            let s = store();
            let err = s.delete("nonexistent").unwrap_err();
            assert!(matches!(err, MemoryError::NotFound));
        }

        #[test]
        fn touch_missing_is_not_found() {
            let s = store();
            let err = s.touch("nonexistent").unwrap_err();
            assert!(matches!(err, MemoryError::NotFound));
        }

        #[test]
        fn search_no_results() {
            let s = store();
            s.store("note", "rust memory store", "test").unwrap();
            let results = s.search("nonexistentterm", 10).unwrap();
            assert!(results.is_empty());
        }

        #[test]
        fn multiple_memories_and_ranking() {
            let s = store();
            s.store("a", "rust memory management ownership", "test")
                .unwrap();
            let b = s.store("b", "rust memory management", "test").unwrap();
            s.store("c", "garbage collection in other languages", "test")
                .unwrap();

            for _ in 0..3 {
                s.touch(&b).unwrap();
            }

            let results = s.search("rust memory", 10).unwrap();
            assert!(!results.is_empty());
            assert_eq!(results[0].id, b);
            assert_eq!(results[0].access_count, 3);
        }

        #[test]
        fn get_missing_returns_none() {
            let s = store();
            assert!(s.get("nonexistent").unwrap().is_none());
        }

        #[test]
        fn search_with_quotes_does_not_crash() {
            let s = store();
            s.store("a", "rust memory management", "test").unwrap();

            // This query contains quotes which would normally cause an FTS syntax error
            // if not sanitized properly.
            let results = s.search("rust \"memory\"", 10).unwrap();
            assert!(!results.is_empty());
        }
    }
}

#[cfg(feature = "memory")]
pub use inner::{MemoryEntry, MemoryError, MemoryStore};
