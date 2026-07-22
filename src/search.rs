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
