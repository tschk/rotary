use super::extract::ExtractionResult;
use super::graph::{next_node_id, *};
use chrono::Utc;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Scans a workspace and builds a structural graph of files, functions, and classes.
#[derive(Debug)]
pub struct CodebaseScanner {
    workspace: PathBuf,
}

impl CodebaseScanner {
    /// Create a scanner rooted at the given workspace.
    pub fn new(workspace: PathBuf) -> Self {
        CodebaseScanner { workspace }
    }

    /// Scan all `.rs`, `.ts`, `.py`, and `.go` files in the workspace.
    pub fn scan(&self) -> Result<ExtractionResult, GraphMemoryError> {
        let mut files = Vec::new();
        self.collect_files(&self.workspace, &mut files)?;
        self.scan_incremental(&files)
    }

    /// Scan only the provided changed files.
    pub fn scan_incremental(
        &self,
        changed_files: &[PathBuf],
    ) -> Result<ExtractionResult, GraphMemoryError> {
        let mut result = ExtractionResult::default();
        let now = Utc::now();
        let mut file_ids: HashMap<String, String> = HashMap::new();
        for path in changed_files {
            if !path.exists() {
                continue;
            }
            let rel = path
                .strip_prefix(&self.workspace)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let file_id = next_node_id();
            file_ids.insert(rel.clone(), file_id.clone());
            result.push_node(MemoryNode {
                id: file_id.clone(),
                label: rel.clone(),
                node_type: NodeType::File,
                description: format!("Source file: {}", rel),
                source_file: Some(rel.clone()),
                source_location: None,
                tags: vec![language_tag(path)],
                created_at: now,
            });
            let defs = extract_definitions(&content, path);
            for def in defs {
                let def_id = next_node_id();
                result.edges.push(MemoryEdge {
                    source: file_id.clone(),
                    target: def_id.clone(),
                    relation: EdgeRelation::Contains,
                    confidence: 1.0,
                    source_file: Some(rel.clone()),
                });
                result.push_node(MemoryNode {
                    id: def_id,
                    label: def.name.clone(),
                    node_type: def.kind,
                    description: format!("{} defined in {}", def.name, rel),
                    source_file: Some(rel.clone()),
                    source_location: Some(def.location.clone()),
                    tags: Vec::new(),
                    created_at: now,
                });
            }
            for imp in extract_imports(&content, path) {
                if let Some(target_id) = file_ids.get(&imp) {
                    if target_id != &file_id {
                        result.edges.push(MemoryEdge {
                            source: file_id.clone(),
                            target: target_id.clone(),
                            relation: EdgeRelation::Imports,
                            confidence: 0.9,
                            source_file: Some(rel.clone()),
                        });
                    }
                }
            }
        }
        Ok(result)
    }

    fn collect_files(&self, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), GraphMemoryError> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if name.starts_with('.')
                    || name == "target"
                    || name == "node_modules"
                    || name == "vendor"
                {
                    continue;
                }
                self.collect_files(&path, out)?;
            } else if is_source_file(&path) {
                out.push(path);
            }
        }
        Ok(())
    }
}

fn is_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("rs") | Some("ts") | Some("py") | Some("go")
    )
}

fn language_tag(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust",
        Some("ts") => "typescript",
        Some("py") => "python",
        Some("go") => "go",
        _ => "unknown",
    }
    .to_string()
}

struct Definition {
    name: String,
    kind: NodeType,
    location: String,
}

fn extract_definitions(content: &str, path: &Path) -> Vec<Definition> {
    let mut defs = Vec::new();
    let lang = language_tag(path);
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        let name = match lang.as_str() {
            "rust" => extract_rust_def(trimmed),
            "typescript" => extract_ts_def(trimmed),
            "python" => extract_py_def(trimmed),
            "go" => extract_go_def(trimmed),
            _ => None,
        };
        if let Some((name, kind)) = name {
            defs.push(Definition {
                name,
                kind,
                location: format!("line:{}", i + 1),
            });
        }
    }
    defs
}

fn extract_rust_def(line: &str) -> Option<(String, NodeType)> {
    let no_pub = line.strip_prefix("pub ").unwrap_or(line);
    let no_pub = no_pub.strip_prefix("pub(crate) ").unwrap_or(no_pub);
    if let Some(rest) = no_pub.strip_prefix("fn ") {
        return Some((take_ident(rest), NodeType::Function));
    }
    if let Some(rest) = no_pub.strip_prefix("struct ") {
        return Some((take_ident(rest), NodeType::Class));
    }
    if let Some(rest) = no_pub.strip_prefix("enum ") {
        return Some((take_ident(rest), NodeType::Class));
    }
    if let Some(rest) = no_pub.strip_prefix("trait ") {
        return Some((take_ident(rest), NodeType::Class));
    }
    if let Some(rest) = no_pub.strip_prefix("mod ") {
        return Some((take_ident(rest), NodeType::Module));
    }
    None
}

fn extract_ts_def(line: &str) -> Option<(String, NodeType)> {
    if let Some(rest) = line.strip_prefix("export ") {
        if let Some(r) = rest.strip_prefix("function ") {
            return Some((take_ident(r), NodeType::Function));
        }
        if let Some(r) = rest.strip_prefix("class ") {
            return Some((take_ident(r), NodeType::Class));
        }
    }
    if let Some(rest) = line.strip_prefix("function ") {
        return Some((take_ident(rest), NodeType::Function));
    }
    if let Some(rest) = line.strip_prefix("class ") {
        return Some((take_ident(rest), NodeType::Class));
    }
    None
}

fn extract_py_def(line: &str) -> Option<(String, NodeType)> {
    if let Some(rest) = line.strip_prefix("def ") {
        return Some((take_ident(rest), NodeType::Function));
    }
    if let Some(rest) = line.strip_prefix("class ") {
        return Some((take_ident(rest), NodeType::Class));
    }
    None
}

fn extract_go_def(line: &str) -> Option<(String, NodeType)> {
    if let Some(rest) = line.strip_prefix("func ") {
        if let Some(rest) = rest.strip_prefix("(") {
            if let Some(idx) = rest.find(')') {
                let after = &rest[idx + 1..].trim_start();
                if let Some(r) = after.strip_prefix(" ") {
                    return Some((take_ident(r), NodeType::Function));
                }
                return Some((take_ident(after), NodeType::Function));
            }
        }
        return Some((take_ident(rest), NodeType::Function));
    }
    if let Some(rest) = line.strip_prefix("type ") {
        return Some((take_ident(rest), NodeType::Class));
    }
    None
}

fn take_ident(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            break;
        }
    }
    out
}

fn extract_imports(content: &str, path: &Path) -> Vec<String> {
    let mut imports = Vec::new();
    let lang = language_tag(path);
    for line in content.lines() {
        let trimmed = line.trim_start();
        match lang.as_str() {
            "rust" => {
                if let Some(rest) = trimmed.strip_prefix("mod ") {
                    let name = take_ident(rest);
                    if !name.is_empty() {
                        imports.push(format!("{}.rs", name));
                    }
                }
                if let Some(rest) = trimmed.strip_prefix("use ") {
                    if let Some(last) = rest
                        .split(|c: char| !c.is_alphanumeric() && c != '_')
                        .next_back()
                    {
                        if !last.is_empty() {
                            imports.push(format!("{}.rs", last));
                        }
                    }
                }
            }
            "python" => {
                if let Some(rest) = trimmed.strip_prefix("from ") {
                    if let Some(idx) = rest.find(" import ") {
                        imports.push(rest[..idx].trim().to_string());
                    }
                } else if let Some(rest) = trimmed.strip_prefix("import ") {
                    imports.push(rest.split_whitespace().next().unwrap_or("").to_string());
                }
            }
            "typescript" => {
                if let Some(rest) = trimmed.strip_prefix("import ") {
                    if let Some(idx) = rest.find("from ") {
                        let target = rest[idx + 5..]
                            .trim()
                            .trim_matches(|c: char| c == '"' || c == '\'');
                        imports.push(target.to_string());
                    }
                }
            }
            "go" if trimmed.starts_with("import") => {
                let quoted: Vec<&str> = trimmed.split('"').collect();
                if quoted.len() >= 3 {
                    imports.push(quoted[1].to_string());
                }
            }
            _ => {}
        }
    }
    imports
}
