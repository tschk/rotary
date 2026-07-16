//! Session store V2 — segmented append log with sidecar offset index.
//!
//! Features:
//! - Segmented append log with automatic rotation at `max_segment_bytes`
//! - Sidecar binary offset index for O(1) entry lookup
//! - Rolling hash chain for integrity verification
//! - Schema versioning for forward compatibility
//! - Checkpoint snapshots for compaction tracking
//! - JSON manifest with session metadata

use crate::pi::session::PiEntry;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Segment frame schema version.
pub const SEGMENT_FRAME_SCHEMA: u16 = 1;
/// Offset index schema version.
pub const OFFSET_INDEX_SCHEMA: u16 = 1;
/// Default maximum segment size in bytes (1 MB).
pub const DEFAULT_MAX_SEGMENT_BYTES: u64 = 1024 * 1024;

const INDEX_ENTRY_SIZE: usize = 20;
const FRAME_PREFIX_SIZE: usize = 4;
const FRAME_HEADER_SIZE: usize = 26;

/// A checkpoint snapshot marking a compaction boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub compacted_before_entry_seq: u64,
    pub timestamp: DateTime<Utc>,
}

/// Session manifest — top-level metadata for a session store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionManifest {
    pub session_id: String,
    pub schema_version: u16,
    pub segment_count: u32,
    pub entry_count: u64,
    pub created: DateTime<Utc>,
    pub last_modified: DateTime<Utc>,
    pub checkpoints: Vec<Checkpoint>,
}

#[derive(Debug, Clone, Copy)]
struct IndexEntry {
    segment_id: u32,
    offset: u64,
}

/// Errors returned by session store operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("entry not found: {0}")]
    NotFound(u64),
    #[error("integrity check failed at entry {0}")]
    IntegrityFailed(u64),
}

/// Segmented append log session store with sidecar offset index.
pub struct SessionStoreV2 {
    dir: PathBuf,
    session_id: String,
    max_segment_bytes: u64,
    current_segment_id: u32,
    current_segment_size: u64,
    next_seq: u64,
    last_chain_hash: u64,
    index: BTreeMap<u64, IndexEntry>,
    manifest: SessionManifest,
}

impl SessionStoreV2 {
    /// Create a new session store in the given directory.
    pub fn new(dir: PathBuf) -> Self {
        Self::with_max_segment_bytes(dir, DEFAULT_MAX_SEGMENT_BYTES)
    }

    /// Create a new session store with a custom max segment size.
    pub fn with_max_segment_bytes(dir: PathBuf, max_segment_bytes: u64) -> Self {
        std::fs::create_dir_all(&dir).ok();

        let mpath = manifest_path(&dir);

        if mpath.exists() {
            let content = std::fs::read_to_string(&mpath).unwrap_or_default();
            let manifest: SessionManifest = serde_json::from_str(&content).unwrap_or_else(|_| {
                let now = Utc::now();
                SessionManifest {
                    session_id: uuid::Uuid::new_v4().to_string(),
                    schema_version: SEGMENT_FRAME_SCHEMA,
                    segment_count: 1,
                    entry_count: 0,
                    created: now,
                    last_modified: now,
                    checkpoints: Vec::new(),
                }
            });

            let index = load_index(&index_path(&dir)).unwrap_or_default();

            let current_segment_id = manifest.segment_count.saturating_sub(1);
            let current_segment_size = std::fs::metadata(segment_path(&dir, current_segment_id))
                .map(|m| m.len())
                .unwrap_or(0);

            let next_seq = index.keys().last().map(|k| k + 1).unwrap_or(1);

            let last_chain_hash = index
                .keys()
                .last()
                .and_then(|&seq| {
                    let idx = index.get(&seq)?;
                    read_frame_at(&dir, idx.segment_id, idx.offset)
                        .ok()
                        .map(|(_, hash, _)| hash)
                })
                .unwrap_or(0);

            Self {
                session_id: manifest.session_id.clone(),
                dir,
                max_segment_bytes,
                current_segment_id,
                current_segment_size,
                next_seq,
                last_chain_hash,
                index,
                manifest,
            }
        } else {
            let session_id = uuid::Uuid::new_v4().to_string();
            let now = Utc::now();
            let manifest = SessionManifest {
                session_id: session_id.clone(),
                schema_version: SEGMENT_FRAME_SCHEMA,
                segment_count: 1,
                entry_count: 0,
                created: now,
                last_modified: now,
                checkpoints: Vec::new(),
            };

            Self {
                dir,
                session_id,
                max_segment_bytes,
                current_segment_id: 0,
                current_segment_size: 0,
                next_seq: 1,
                last_chain_hash: 0,
                index: BTreeMap::new(),
                manifest,
            }
        }
    }

    /// Append an entry to the log. Returns the assigned sequence number.
    pub fn append(&mut self, entry: &PiEntry) -> Result<u64, SessionStoreError> {
        let seq = self.next_seq;
        let mut stored = entry.clone();
        stored.id = seq;
        let json = serde_json::to_vec(&stored)?;

        let entry_data_hash = fnv1a_hash(&json);
        let chain_hash = compute_chain_hash(self.last_chain_hash, entry_data_hash);

        let frame_size = (FRAME_PREFIX_SIZE + FRAME_HEADER_SIZE + json.len()) as u64;
        self.maybe_rotate_segment(frame_size)?;

        let path = segment_path(&self.dir, self.current_segment_id);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        let offset = self.current_segment_size;
        let frame_len = (FRAME_HEADER_SIZE + json.len()) as u32;
        file.write_all(&frame_len.to_le_bytes())?;
        file.write_all(&SEGMENT_FRAME_SCHEMA.to_le_bytes())?;
        file.write_all(&seq.to_le_bytes())?;
        file.write_all(&chain_hash.to_le_bytes())?;
        file.write_all(&(json.len() as u64).to_le_bytes())?;
        file.write_all(&json)?;
        file.flush()?;

        self.current_segment_size += frame_size;

        self.index.insert(
            seq,
            IndexEntry {
                segment_id: self.current_segment_id,
                offset,
            },
        );

        append_index_entry(&index_path(&self.dir), seq, self.current_segment_id, offset)?;

        self.next_seq += 1;
        self.last_chain_hash = chain_hash;
        self.manifest.entry_count += 1;
        self.manifest.segment_count = self.current_segment_id + 1;
        self.manifest.last_modified = Utc::now();
        self.save_manifest()?;

        Ok(seq)
    }

    /// O(1) lookup of an entry by sequence number via the sidecar index.
    pub fn get(&self, seq: u64) -> Result<Option<PiEntry>, SessionStoreError> {
        match self.index.get(&seq) {
            Some(idx) => {
                let (_, _, json) = read_frame_at(&self.dir, idx.segment_id, idx.offset)?;
                let entry: PiEntry = serde_json::from_slice(&json)?;
                Ok(Some(entry))
            }
            None => Ok(None),
        }
    }

    /// Iterate over all entries in sequence order.
    pub fn iter(&self) -> impl Iterator<Item = PiEntry> {
        self.iter_all().unwrap_or_default().into_iter()
    }

    /// Record a checkpoint marking that entries before `compacted_before` have been compacted.
    pub fn checkpoint(&mut self, compacted_before: u64) -> Result<(), SessionStoreError> {
        self.manifest.checkpoints.push(Checkpoint {
            compacted_before_entry_seq: compacted_before,
            timestamp: Utc::now(),
        });
        self.manifest.last_modified = Utc::now();
        self.save_manifest()?;
        Ok(())
    }

    /// Verify the hash chain across all entries. Returns `Ok(false)` if any chain link is broken.
    pub fn verify_integrity(&self) -> Result<bool, SessionStoreError> {
        let mut previous_hash: u64 = 0;
        for idx in self.index.values() {
            let (_, stored_hash, json) = read_frame_at(&self.dir, idx.segment_id, idx.offset)?;
            let entry_data_hash = fnv1a_hash(&json);
            let expected = compute_chain_hash(previous_hash, entry_data_hash);
            if expected != stored_hash {
                return Ok(false);
            }
            previous_hash = stored_hash;
        }
        Ok(true)
    }

    /// Borrow the session manifest.
    pub fn manifest(&self) -> &SessionManifest {
        &self.manifest
    }

    /// Return the session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn iter_all(&self) -> Result<Vec<PiEntry>, SessionStoreError> {
        let mut entries = Vec::with_capacity(self.index.len());
        for idx in self.index.values() {
            let (_, _, json) = read_frame_at(&self.dir, idx.segment_id, idx.offset)?;
            let entry: PiEntry = serde_json::from_slice(&json)?;
            entries.push(entry);
        }
        Ok(entries)
    }

    fn save_manifest(&self) -> Result<(), SessionStoreError> {
        let json = serde_json::to_string_pretty(&self.manifest)?;
        std::fs::write(manifest_path(&self.dir), json)?;
        Ok(())
    }

    fn maybe_rotate_segment(&mut self, frame_size: u64) -> Result<(), SessionStoreError> {
        if self.current_segment_size > 0
            && self.current_segment_size + frame_size > self.max_segment_bytes
        {
            self.current_segment_id += 1;
            self.current_segment_size = 0;
        }
        Ok(())
    }
}

fn segment_path(dir: &Path, segment_id: u32) -> PathBuf {
    dir.join(format!("segment_{:06}.log", segment_id))
}

fn index_path(dir: &Path) -> PathBuf {
    dir.join("index.bin")
}

fn manifest_path(dir: &Path) -> PathBuf {
    dir.join("manifest.json")
}

fn fnv1a_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn compute_chain_hash(previous: u64, entry_data_hash: u64) -> u64 {
    previous.wrapping_mul(31).wrapping_add(entry_data_hash)
}

fn read_frame_at(dir: &Path, segment_id: u32, offset: u64) -> std::io::Result<(u64, u64, Vec<u8>)> {
    let path = segment_path(dir, segment_id);
    let mut file = std::fs::File::open(&path)?;
    file.seek(SeekFrom::Start(offset))?;

    let mut len_buf = [0u8; FRAME_PREFIX_SIZE];
    file.read_exact(&mut len_buf)?;

    let mut schema_buf = [0u8; 2];
    file.read_exact(&mut schema_buf)?;

    let mut seq_buf = [0u8; 8];
    file.read_exact(&mut seq_buf)?;
    let seq = u64::from_le_bytes(seq_buf);

    let mut hash_buf = [0u8; 8];
    file.read_exact(&mut hash_buf)?;
    let chain_hash = u64::from_le_bytes(hash_buf);

    let mut json_len_buf = [0u8; 8];
    file.read_exact(&mut json_len_buf)?;
    let json_len = u64::from_le_bytes(json_len_buf) as usize;

    let mut json = vec![0u8; json_len];
    file.read_exact(&mut json)?;

    Ok((seq, chain_hash, json))
}

fn load_index(path: &Path) -> std::io::Result<BTreeMap<u64, IndexEntry>> {
    let data = std::fs::read(path)?;
    let mut index = BTreeMap::new();
    for chunk in data.chunks_exact(INDEX_ENTRY_SIZE) {
        let seq = u64::from_le_bytes(chunk[0..8].try_into().unwrap());
        let segment_id = u32::from_le_bytes(chunk[8..12].try_into().unwrap());
        let offset = u64::from_le_bytes(chunk[12..20].try_into().unwrap());
        index.insert(seq, IndexEntry { segment_id, offset });
    }
    Ok(index)
}

fn append_index_entry(path: &Path, seq: u64, segment_id: u32, offset: u64) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(&seq.to_le_bytes())?;
    file.write_all(&segment_id.to_le_bytes())?;
    file.write_all(&offset.to_le_bytes())?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pi::session::PiEntryType;
    use crate::provider::Role;
    use tempfile::TempDir;

    fn make_entry(role: Role, content: &str) -> PiEntry {
        PiEntry {
            entry_type: PiEntryType::Message {
                role,
                content: content.to_string(),
                tool_call_id: None,
            },
            timestamp: Utc::now(),
            id: 0,
            parent_id: None,
        }
    }

    #[test]
    fn append_and_get() {
        let tmp = TempDir::new().unwrap();
        let mut store = SessionStoreV2::new(tmp.path().to_path_buf());

        let entry = make_entry(Role::User, "hello");
        let seq = store.append(&entry).unwrap();
        assert_eq!(seq, 1);

        let got = store.get(seq).unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().id, seq);

        let none = store.get(999).unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn segment_rotation() {
        let tmp = TempDir::new().unwrap();
        let mut store = SessionStoreV2::with_max_segment_bytes(tmp.path().to_path_buf(), 200);

        for i in 0..20 {
            let entry = make_entry(Role::User, &format!("message number {}", i));
            store.append(&entry).unwrap();
        }

        assert!(store.current_segment_id > 0);
        assert!(store.manifest.segment_count > 1);

        for i in 1..=20 {
            let got = store.get(i).unwrap();
            assert!(got.is_some(), "entry {} should exist", i);
        }
    }

    #[test]
    fn hash_chain_integrity() {
        let tmp = TempDir::new().unwrap();
        let mut store = SessionStoreV2::new(tmp.path().to_path_buf());

        for i in 0..5 {
            let entry = make_entry(Role::User, &format!("msg {}", i));
            store.append(&entry).unwrap();
        }

        assert!(store.verify_integrity().unwrap());
    }

    #[test]
    fn checkpoint_creation() {
        let tmp = TempDir::new().unwrap();
        let mut store = SessionStoreV2::new(tmp.path().to_path_buf());

        for i in 0..5 {
            let entry = make_entry(Role::User, &format!("msg {}", i));
            store.append(&entry).unwrap();
        }

        store.checkpoint(3).unwrap();
        assert_eq!(store.manifest.checkpoints.len(), 1);
        assert_eq!(store.manifest.checkpoints[0].compacted_before_entry_seq, 3);

        let reloaded = SessionStoreV2::new(tmp.path().to_path_buf());
        assert_eq!(reloaded.manifest().checkpoints.len(), 1);
    }

    #[test]
    fn iter_all_entries() {
        let tmp = TempDir::new().unwrap();
        let mut store = SessionStoreV2::new(tmp.path().to_path_buf());

        for i in 0..10 {
            let entry = make_entry(Role::User, &format!("msg {}", i));
            store.append(&entry).unwrap();
        }

        let entries: Vec<PiEntry> = store.iter().collect();
        assert_eq!(entries.len(), 10);

        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.id, (i + 1) as u64);
        }
    }

    #[test]
    fn reload_preserves_entries() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        {
            let mut store = SessionStoreV2::new(dir.clone());
            for i in 0..5 {
                let entry = make_entry(Role::Assistant, &format!("reply {}", i));
                store.append(&entry).unwrap();
            }
        }

        let reloaded = SessionStoreV2::new(dir);
        assert_eq!(reloaded.manifest().entry_count, 5);
        for i in 1..=5 {
            let got = reloaded.get(i).unwrap();
            assert!(got.is_some(), "entry {} should exist after reload", i);
        }
        assert!(reloaded.verify_integrity().unwrap());
    }
}
