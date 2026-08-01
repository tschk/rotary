use fff_search::{FFFMode, FilePicker, FilePickerOptions, SharedFilePicker, SharedFrecency};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

static PICKERS: LazyLock<Mutex<HashMap<PathBuf, SharedFilePicker>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn picker_for(root: PathBuf) -> Result<SharedFilePicker, String> {
    let mut map = PICKERS.lock();
    if let Some(picker) = map.get(&root) {
        return Ok(picker.clone());
    }
    let shared = SharedFilePicker::default();
    let frecency = SharedFrecency::default();
    let options = FilePickerOptions {
        base_path: root.to_string_lossy().into_owned(),
        mode: FFFMode::Ai,
        watch: true,
        enable_content_indexing: true,
        ..Default::default()
    };
    FilePicker::new_with_shared_state(shared.clone(), frecency, options)
        .map_err(|e| e.to_string())?;
    shared.wait_for_scan(Duration::from_secs(30));
    map.insert(root, shared.clone());
    Ok(shared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_picker_for_creates_and_caches() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();

        // First call creates a new picker
        let picker1 = picker_for(path.clone());
        assert!(picker1.is_ok(), "First call should succeed");

        // The picker should now be cached
        assert!(
            PICKERS.lock().contains_key(&path),
            "Picker should be in the map"
        );

        // Second call should return the cached picker
        let picker2 = picker_for(path.clone());
        assert!(picker2.is_ok(), "Second call should succeed");
    }

    #[test]
    fn test_picker_for_invalid_path() {
        let path = PathBuf::from("/non/existent/path/for/test/12345");
        let result = picker_for(path);
        assert!(
            result.is_err(),
            "Call with invalid path should return an error"
        );
    }
}
