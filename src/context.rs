//! Context: project instruction files (AGENTS.md), system prompt composition, compaction.

use std::path::{Path, PathBuf};

pub struct ProjectInstructions {
    pub path: String,
    pub content: String,
    pub sources: Vec<String>,
}

const CANDIDATES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    ".cursor/rules",
    "CRUSH.md",
    "GEMINI.md",
];

/// Load and merge all present instruction files under `dir` (first-match order).
pub fn load_project_instructions(dir: &Path) -> Option<ProjectInstructions> {
    let mut parts: Vec<(String, String)> = Vec::new();
    for candidate in CANDIDATES {
        let path = dir.join(candidate);
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if !content.trim().is_empty() {
                    parts.push((path.to_string_lossy().to_string(), content));
                }
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    let sources: Vec<String> = parts.iter().map(|(p, _)| p.clone()).collect();
    let content = parts
        .into_iter()
        .map(|(path, body)| {
            let name = PathBuf::from(&path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            format!("# From {name}\n\n{body}")
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    Some(ProjectInstructions {
        path: sources[0].clone(),
        content,
        sources,
    })
}

pub fn compose_system_prompt(base: Option<&str>, instructions: &str) -> Option<String> {
    match base {
        Some(b) => Some(format!("{b}\n\n# Project Instructions\n\n{instructions}")),
        None => Some(format!("# Project Instructions\n\n{instructions}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn merges_multiple_instruction_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "agents rules").unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "claude rules").unwrap();
        let instr = load_project_instructions(dir.path()).unwrap();
        assert_eq!(instr.sources.len(), 2);
        assert!(instr.content.contains("agents rules"));
        assert!(instr.content.contains("claude rules"));
    }
}
