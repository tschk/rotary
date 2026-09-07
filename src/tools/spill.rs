use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const DEFAULT_PREVIEW_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpillStatus {
    Inline,
    Spilled,
    SpillFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpillNotice {
    pub status: SpillStatus,
    pub locator: String,
    pub original_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpilledResult {
    pub preview: String,
    pub locator: String,
    pub spilled: bool,
    pub original_bytes: usize,
    pub status: SpillStatus,
}

impl SpilledResult {
    pub fn notice(&self) -> SpillNotice {
        SpillNotice {
            status: self.status,
            locator: self.locator.clone(),
            original_bytes: self.original_bytes,
        }
    }
}

pub fn bound_tool_output(body: &str, max_preview: usize, spill_dir: &Path) -> SpilledResult {
    let original_bytes = body.len();
    if original_bytes <= max_preview {
        return SpilledResult {
            preview: body.to_string(),
            locator: String::new(),
            spilled: false,
            original_bytes,
            status: SpillStatus::Inline,
        };
    }
    match write_spill(body, spill_dir) {
        Ok(locator) => {
            let preview = preview_body(body, max_preview, Some(&locator));
            SpilledResult {
                preview,
                locator: locator.to_string_lossy().into_owned(),
                spilled: true,
                original_bytes,
                status: SpillStatus::Spilled,
            }
        }
        Err(_) => SpilledResult {
            preview: preview_body(body, max_preview, None),
            locator: String::new(),
            spilled: false,
            original_bytes,
            status: SpillStatus::SpillFailed,
        },
    }
}

fn write_spill(body: &str, spill_dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(spill_dir)?;
    let digest = Sha256::digest(body.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let locator = spill_dir.join(format!("spill-{hex}.txt"));
    std::fs::write(&locator, body)?;
    Ok(locator)
}

fn preview_body(body: &str, max_preview: usize, locator: Option<&Path>) -> String {
    let take = char_boundary_at(body, max_preview.min(body.len()));
    let mut preview = body[..take].to_string();
    if take < body.len() {
        preview.push_str("\n…[truncated");
        match locator {
            Some(path) => {
                preview.push_str(", full output at ");
                preview.push_str(&path.to_string_lossy());
            }
            None => preview.push_str(", spill failed"),
        }
        preview.push(']');
    }
    preview
}

fn char_boundary_at(body: &str, max_bytes: usize) -> usize {
    if max_bytes >= body.len() {
        return body.len();
    }
    let mut end = max_bytes;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    end
}

pub fn locator_is_file(locator: &str) -> bool {
    !locator.is_empty() && PathBuf::from(locator).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huge_stdout_truncated_and_locator_valid() {
        let dir = tempfile::tempdir().unwrap();
        let body = "x".repeat(20_000);
        let spilled = bound_tool_output(&body, 1024, dir.path());
        assert!(spilled.spilled);
        assert_eq!(spilled.status, SpillStatus::Spilled);
        assert!(spilled.preview.len() < body.len());
        assert!(spilled.preview.contains("truncated"));
        assert!(locator_is_file(&spilled.locator));
        let on_disk = std::fs::read_to_string(&spilled.locator).unwrap();
        assert_eq!(on_disk.len(), 20_000);
        assert_eq!(spilled.original_bytes, 20_000);
    }

    #[test]
    fn small_output_not_spilled() {
        let dir = tempfile::tempdir().unwrap();
        let spilled = bound_tool_output("ok", 1024, dir.path());
        assert!(!spilled.spilled);
        assert_eq!(spilled.status, SpillStatus::Inline);
        assert_eq!(spilled.preview, "ok");
        assert!(spilled.locator.is_empty());
    }

    #[test]
    fn preview_truncates_on_utf8_char_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!("{}é{}", "a".repeat(4), "z".repeat(8));
        let spilled = bound_tool_output(&body, 5, dir.path());
        assert_eq!(spilled.status, SpillStatus::Spilled);
        let cut = spilled.preview.split('\n').next().unwrap();
        assert_eq!(cut, "aaaa");
        assert!(cut.is_char_boundary(cut.len()));
    }

    #[test]
    fn spill_write_failure_returns_bounded_preview() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("blocked");
        std::fs::write(&not_a_dir, "file").unwrap();
        let body = "y".repeat(2_000);
        let spilled = bound_tool_output(&body, 16, &not_a_dir);
        assert_eq!(spilled.status, SpillStatus::SpillFailed);
        assert!(!spilled.spilled);
        assert!(spilled.locator.is_empty());
        assert!(spilled.preview.len() < body.len());
        assert!(spilled.preview.contains("spill failed"));
        assert_eq!(spilled.notice().status, SpillStatus::SpillFailed);
    }
}
