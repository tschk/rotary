use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

pub const DEFAULT_PREVIEW_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpilledResult {
    pub preview: String,
    pub locator: String,
    pub spilled: bool,
    pub original_bytes: usize,
}

pub fn bound_tool_output(
    body: &str,
    max_preview: usize,
    spill_dir: &Path,
) -> io::Result<SpilledResult> {
    let original_bytes = body.len();
    if original_bytes <= max_preview {
        return Ok(SpilledResult {
            preview: body.to_string(),
            locator: String::new(),
            spilled: false,
            original_bytes,
        });
    }
    std::fs::create_dir_all(spill_dir)?;
    let digest = Sha256::digest(body.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let locator = spill_dir.join(format!("spill-{hex}.txt"));
    std::fs::write(&locator, body)?;
    let preview = preview_body(body, max_preview, &locator);
    Ok(SpilledResult {
        preview,
        locator: locator.to_string_lossy().into_owned(),
        spilled: true,
        original_bytes,
    })
}

fn preview_body(body: &str, max_preview: usize, locator: &Path) -> String {
    let take = max_preview.min(body.len());
    let mut preview = body[..take].to_string();
    if take < body.len() {
        preview.push_str("\n…[truncated, full output at ");
        preview.push_str(&locator.to_string_lossy());
        preview.push(']');
    }
    preview
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
        let spilled = bound_tool_output(&body, 1024, dir.path()).unwrap();
        assert!(spilled.spilled);
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
        let spilled = bound_tool_output("ok", 1024, dir.path()).unwrap();
        assert!(!spilled.spilled);
        assert_eq!(spilled.preview, "ok");
        assert!(spilled.locator.is_empty());
    }
}
