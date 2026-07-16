//! Context: project instruction files (AGENTS.md), system prompt composition, compaction.

use std::path::Path;

pub struct ProjectInstructions {
    pub path: String,
    pub content: String,
}

pub fn load_project_instructions(dir: &Path) -> Option<ProjectInstructions> {
    let candidates = ["AGENTS.md", "CLAUDE.md", ".cursor/rules"];
    for candidate in candidates {
        let path = dir.join(candidate);
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                return Some(ProjectInstructions {
                    path: path.to_string_lossy().to_string(),
                    content,
                });
            }
        }
    }
    None
}

pub fn compose_system_prompt(base: Option<&str>, instructions: &str) -> Option<String> {
    match base {
        Some(b) => Some(format!("{b}\n\n# Project Instructions\n\n{instructions}")),
        None => Some(format!("# Project Instructions\n\n{instructions}")),
    }
}
