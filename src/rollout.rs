//! Rollout persistence: durable JSONL event logging with buffered I/O.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

const EVENTS_FILE: &str = "events.jsonl";
const PAYLOADS_DIR: &str = "payloads";
const PAYLOAD_THRESHOLD: usize = 1024;
/// Maximum file size for rollout events JSONL (10 MB).
const MAX_ROLLOUT_FILE_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum RolloutError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, RolloutError>;

/// A buffered writer for JSONL event logs.
pub struct TraceWriter {
    writer: BufWriter<File>,
}

impl TraceWriter {
    /// Create a new `TraceWriter`, making the directory and opening `events.jsonl`.
    pub fn new(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        let path = dir.join(EVENTS_FILE);
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    /// Serialize `event` to JSON, write it with a trailing newline, and flush.
    pub fn append(&mut self, event: &serde_json::Value) -> Result<()> {
        let line = serde_json::to_string(event)?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.flush()?;
        Ok(())
    }

    /// Flush the underlying buffer to disk.
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}

impl Drop for TraceWriter {
    fn drop(&mut self) {
        let _ = self.writer.flush();
    }
}

/// A single rollout event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RolloutEntry {
    #[serde(rename = "turn_start")]
    TurnStart {
        turn: usize,
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "turn_end")]
    TurnEnd {
        turn: usize,
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "message")]
    Message {
        role: String,
        content: String,
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "tool_call")]
    ToolCall {
        id: String,
        name: String,
        arguments: String,
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        id: String,
        content: String,
        is_error: bool,
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "error")]
    Error {
        message: String,
        timestamp: DateTime<Utc>,
    },
}

impl RolloutEntry {
    /// The timestamp associated with this entry.
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            RolloutEntry::TurnStart { timestamp, .. }
            | RolloutEntry::TurnEnd { timestamp, .. }
            | RolloutEntry::Message { timestamp, .. }
            | RolloutEntry::ToolCall { timestamp, .. }
            | RolloutEntry::ToolResult { timestamp, .. }
            | RolloutEntry::Error { timestamp, .. } => *timestamp,
        }
    }

    /// Returns a mutable reference to the content field, if present.
    fn content_mut(&mut self) -> Option<&mut String> {
        match self {
            RolloutEntry::Message { content, .. } => Some(content),
            RolloutEntry::ToolCall { arguments, .. } => Some(arguments),
            RolloutEntry::ToolResult { content, .. } => Some(content),
            _ => None,
        }
    }
}

/// Manages rollout persistence for a single session.
pub struct RolloutManager {
    writer: TraceWriter,
    dir: PathBuf,
    closed: bool,
}

impl RolloutManager {
    /// Create a new manager rooted at `session_dir`.
    pub fn new(session_dir: PathBuf) -> Result<Self> {
        let writer = TraceWriter::new(session_dir.join("rollout"))?;
        Ok(Self {
            writer,
            dir: session_dir,
            closed: false,
        })
    }

    /// The directory holding this manager's rollout data.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Serialize `entry`, separating large payloads, and append it to the log.
    pub fn record(&mut self, entry: &RolloutEntry) -> Result<()> {
        let mut to_write = entry.clone();
        if let Some(content) = to_write.content_mut() {
            if content.len() > PAYLOAD_THRESHOLD {
                let rollout_dir = self.dir.join("rollout");
                let path = write_payload(&rollout_dir, content)?;
                *content = path;
            }
        }
        let value = serde_json::to_value(&to_write)?;
        self.writer.append(&value)
    }

    /// Load all entries previously written to `dir/rollout/events.jsonl`.
    pub fn load(dir: &Path) -> Result<Vec<RolloutEntry>> {
        let path = dir.join("rollout").join(EVENTS_FILE);
        // Reject oversized rollout files before reading into memory.
        if let Ok(meta) = fs::metadata(&path) {
            if meta.len() > MAX_ROLLOUT_FILE_BYTES {
                return Err(RolloutError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "rollout file too large: {} bytes (max {MAX_ROLLOUT_FILE_BYTES})",
                        meta.len()
                    ),
                )));
            }
        }
        let contents = fs::read_to_string(path)?;
        let mut entries = Vec::new();
        for line in contents.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let entry: RolloutEntry = serde_json::from_str(line)?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Flush pending writes and mark the rollout as complete.
    pub fn close(&mut self) -> Result<()> {
        if !self.closed {
            self.writer.flush()?;
            self.closed = true;
        }
        Ok(())
    }
}

/// Write `content` to `dir/payloads/{uuid}.txt` and return the relative path string.
pub fn write_payload(dir: &Path, content: &str) -> Result<String> {
    let payloads_dir = dir.join(PAYLOADS_DIR);
    fs::create_dir_all(&payloads_dir)?;
    let id = uuid::Uuid::new_v4();
    let file_name = format!("{id}.txt");
    let path = payloads_dir.join(&file_name);
    fs::write(&path, content)?;
    Ok(format!("{PAYLOADS_DIR}/{file_name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("create temp dir")
    }

    #[test]
    fn write_and_read_entries() {
        let tmp = temp_dir();
        let mut manager = RolloutManager::new(tmp.path().to_path_buf()).expect("new manager");
        let ts = Utc::now();
        manager
            .record(&RolloutEntry::TurnStart {
                turn: 1,
                timestamp: ts,
            })
            .expect("record turn start");
        manager
            .record(&RolloutEntry::Message {
                role: "user".into(),
                content: "hello".into(),
                timestamp: ts,
            })
            .expect("record message");
        manager.close().expect("close");

        let loaded = RolloutManager::load(tmp.path()).expect("load");
        assert_eq!(loaded.len(), 2);
        assert!(matches!(
            &loaded[0],
            RolloutEntry::TurnStart { turn, .. } if *turn == 1
        ));
        assert!(matches!(
            &loaded[1],
            RolloutEntry::Message { role, content, .. }
                if role == "user" && content == "hello"
        ));
    }

    #[test]
    fn payload_separation_for_large_content() {
        let tmp = temp_dir();
        let mut manager = RolloutManager::new(tmp.path().to_path_buf()).expect("new manager");
        let ts = Utc::now();
        let big = "x".repeat(PAYLOAD_THRESHOLD + 1);
        manager
            .record(&RolloutEntry::Message {
                role: "assistant".into(),
                content: big.clone(),
                timestamp: ts,
            })
            .expect("record large message");
        manager.close().expect("close");

        let loaded = RolloutManager::load(tmp.path()).expect("load");
        assert_eq!(loaded.len(), 1);
        if let RolloutEntry::Message { content, .. } = &loaded[0] {
            assert!(content.starts_with("payloads/"));
            assert!(content.ends_with(".txt"));
            let payload_path = tmp.path().join("rollout").join(content);
            let read_back = fs::read_to_string(&payload_path).expect("read payload");
            assert_eq!(read_back, big);
        } else {
            panic!("expected message entry");
        }
    }

    #[test]
    fn small_content_stays_inline() {
        let tmp = temp_dir();
        let mut manager = RolloutManager::new(tmp.path().to_path_buf()).expect("new manager");
        let ts = Utc::now();
        manager
            .record(&RolloutEntry::Message {
                role: "user".into(),
                content: "small".into(),
                timestamp: ts,
            })
            .expect("record small message");
        manager.close().expect("close");

        let loaded = RolloutManager::load(tmp.path()).expect("load");
        if let RolloutEntry::Message { content, .. } = &loaded[0] {
            assert_eq!(content, "small");
        } else {
            panic!("expected message entry");
        }
    }

    #[test]
    fn flush_on_drop() {
        let tmp = temp_dir();
        let ts = Utc::now();
        {
            let mut manager = RolloutManager::new(tmp.path().to_path_buf()).expect("new manager");
            manager
                .record(&RolloutEntry::TurnStart {
                    turn: 1,
                    timestamp: ts,
                })
                .expect("record");
        }

        let loaded = RolloutManager::load(tmp.path()).expect("load");
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn multiple_entry_types_roundtrip() {
        let tmp = temp_dir();
        let ts = Utc::now();
        let entries = vec![
            RolloutEntry::TurnStart {
                turn: 1,
                timestamp: ts,
            },
            RolloutEntry::Message {
                role: "user".into(),
                content: "run tests".into(),
                timestamp: ts,
            },
            RolloutEntry::ToolCall {
                id: "call_1".into(),
                name: "shell".into(),
                arguments: "{}".into(),
                timestamp: ts,
            },
            RolloutEntry::ToolResult {
                id: "call_1".into(),
                content: "ok".into(),
                is_error: false,
                timestamp: ts,
            },
            RolloutEntry::TurnEnd {
                turn: 1,
                timestamp: ts,
            },
            RolloutEntry::Error {
                message: "boom".into(),
                timestamp: ts,
            },
        ];

        let mut manager = RolloutManager::new(tmp.path().to_path_buf()).expect("new manager");
        for entry in &entries {
            manager.record(entry).expect("record");
        }
        manager.close().expect("close");

        let loaded = RolloutManager::load(tmp.path()).expect("load");
        assert_eq!(loaded.len(), entries.len());
        for (i, expected) in entries.iter().enumerate() {
            assert_eq!(
                serde_json::to_string(expected).unwrap(),
                serde_json::to_string(&loaded[i]).unwrap(),
                "entry {i} mismatch"
            );
        }
    }

    #[test]
    fn type_tag_uses_snake_case() {
        let ts = Utc::now();
        let entry = RolloutEntry::TurnStart {
            turn: 1,
            timestamp: ts,
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["type"], "turn_start");

        let entry = RolloutEntry::TurnEnd {
            turn: 1,
            timestamp: ts,
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["type"], "turn_end");

        let entry = RolloutEntry::ToolCall {
            id: "1".into(),
            name: "n".into(),
            arguments: "{}".into(),
            timestamp: ts,
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["type"], "tool_call");

        let entry = RolloutEntry::ToolResult {
            id: "1".into(),
            content: "c".into(),
            is_error: false,
            timestamp: ts,
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["type"], "tool_result");
    }

    #[test]
    fn trace_writer_append_flushes() {
        let tmp = temp_dir();
        let mut writer = TraceWriter::new(tmp.path().to_path_buf()).expect("new writer");
        let value = serde_json::json!({"hello": "world"});
        writer.append(&value).expect("append");
        drop(writer);

        let contents = fs::read_to_string(tmp.path().join(EVENTS_FILE)).expect("read");
        assert!(contents.contains("hello"));
        assert!(contents.ends_with('\n'));
    }

    #[test]
    fn write_payload_returns_relative_path() {
        let tmp = temp_dir();
        let path = write_payload(tmp.path(), "payload data").expect("write payload");
        assert!(path.starts_with("payloads/"));
        assert!(path.ends_with(".txt"));
        let full = tmp.path().join(&path);
        assert_eq!(fs::read_to_string(full).unwrap(), "payload data");
    }
}
