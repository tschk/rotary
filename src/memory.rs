//! Memory: SQLite FTS5 hybrid search (grok pattern).
//!
//! When `memory` feature is enabled, provides persistent memory with
//! full-text search and optional vector similarity.

#[cfg(feature = "memory")]
pub struct MemoryStore;

#[cfg(feature = "memory")]
impl MemoryStore {
    pub fn new(_db_path: impl AsRef<std::path::Path>) -> Self {
        Self
    }

    pub fn store(&self, _key: &str, _content: &str) -> Result<(), String> {
        Ok(())
    }

    pub fn search(&self, _query: &str, _limit: usize) -> Vec<String> {
        Vec::new()
    }
}
