//! Pre-edit file snapshots and observed-file version guards (not git).

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSnapshot {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub digest: String,
}

impl FileSnapshot {
    pub fn from_bytes(path: impl Into<PathBuf>, bytes: Vec<u8>) -> Self {
        let digest = digest_bytes(&bytes);
        Self {
            path: path.into(),
            bytes,
            digest,
        }
    }

    pub fn from_path(path: &Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        Ok(Self::from_bytes(path, bytes))
    }
}

#[derive(Debug, Clone, Default)]
pub struct SnapshotStore {
    by_path: HashMap<PathBuf, FileSnapshot>,
}

impl SnapshotStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot_file(&mut self, path: &Path) -> Option<FileSnapshot> {
        let snap = FileSnapshot::from_path(path).ok()?;
        self.by_path.insert(snap.path.clone(), snap.clone());
        Some(snap)
    }

    pub fn snapshot_bytes(&mut self, path: impl Into<PathBuf>, bytes: Vec<u8>) -> FileSnapshot {
        let snap = FileSnapshot::from_bytes(path, bytes);
        self.by_path.insert(snap.path.clone(), snap.clone());
        snap
    }

    pub fn get(&self, path: &Path) -> Option<&FileSnapshot> {
        self.by_path.get(path)
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileVersionGuard {
    observed: HashMap<PathBuf, String>,
}

impl FileVersionGuard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, path: impl Into<PathBuf>, bytes: &[u8]) {
        self.observed.insert(path.into(), digest_bytes(bytes));
    }

    pub fn observe_path(&mut self, path: &Path) -> std::io::Result<()> {
        let bytes = std::fs::read(path)?;
        self.observe(path, &bytes);
        Ok(())
    }

    pub fn check(&self, path: &Path) -> Result<(), String> {
        let Some(expected) = self.observed.get(path) else {
            return Ok(());
        };
        let current = match std::fs::read(path) {
            Ok(bytes) => digest_bytes(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!("stale observed file {} (missing)", path.display()));
            }
            Err(e) => return Err(format!("stale observed file {}: {e}", path.display())),
        };
        if &current != expected {
            return Err(format!(
                "stale observed file {} (digest changed)",
                path.display()
            ));
        }
        Ok(())
    }
}

pub fn digest_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips_bytes() {
        let mut store = SnapshotStore::new();
        let snap = store.snapshot_bytes("src/lib.rs", b"hello".to_vec());
        assert_eq!(snap.bytes, b"hello");
        assert_eq!(
            store.get(Path::new("src/lib.rs")).unwrap().digest,
            snap.digest
        );
    }

    #[test]
    fn version_guard_allows_unobserved_and_matching() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"a").unwrap();
        let mut guard = FileVersionGuard::new();
        assert!(guard.check(&path).is_ok());
        guard.observe(&path, b"a");
        assert!(guard.check(&path).is_ok());
    }

    #[test]
    fn version_guard_fail_closed_on_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"a").unwrap();
        let mut guard = FileVersionGuard::new();
        guard.observe_path(&path).unwrap();
        std::fs::write(&path, b"b").unwrap();
        assert!(guard.check(&path).unwrap_err().contains("stale"));
    }
}
