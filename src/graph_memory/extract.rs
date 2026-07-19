use super::graph::*;
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// A single turn in an agent conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: String,
    pub content: String,
}

/// The result of extracting memory from a conversation or scan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub nodes: Vec<MemoryNode>,
    pub edges: Vec<MemoryEdge>,
}

impl ExtractionResult {
    pub(crate) fn push_node(&mut self, node: MemoryNode) -> String {
        let id = node.id.clone();
        self.nodes.push(node);
        id
    }
}

/// Extracts memory nodes and edges from agent conversations via keyword matching.
#[derive(Debug, Default)]
pub struct ConversationExtractor;

impl ConversationExtractor {
    /// Create a new conversation extractor.
    pub fn new() -> Self {
        Self
    }

    /// Extract concepts, decisions, patterns, bugs, and features from a conversation.
    pub fn extract(&self, conversation: &[ConversationTurn]) -> ExtractionResult {
        let mut result = ExtractionResult::default();
        let now = Utc::now();
        for (i, turn) in conversation.iter().enumerate() {
            let content = turn.content.to_lowercase();
            let loc = format!("turn:{}", i);
            if content.contains("decided to") || content.contains("we decided") {
                let label = extract_snippet(&turn.content, "decided");
                result.push_node(MemoryNode {
                    id: String::new(),
                    label,
                    node_type: NodeType::Decision,
                    description: turn.content.clone(),
                    source_file: None,
                    source_location: Some(loc.clone()),
                    tags: vec!["decision".to_string()],
                    created_at: now,
                });
            }
            if content.contains("fixed bug")
                || content.contains("fixes bug")
                || content.contains("bug fix")
            {
                let bug_label = extract_snippet(&turn.content, "bug");
                let bug_id = result.push_node(MemoryNode {
                    id: String::new(),
                    label: bug_label,
                    node_type: NodeType::Bug,
                    description: turn.content.clone(),
                    source_file: None,
                    source_location: Some(loc.clone()),
                    tags: vec!["bug".to_string()],
                    created_at: now,
                });
                let fix_id = result.push_node(MemoryNode {
                    id: String::new(),
                    label: format!(
                        "Fix for {}",
                        turn.content.chars().take(40).collect::<String>()
                    ),
                    node_type: NodeType::Feature,
                    description: turn.content.clone(),
                    source_file: None,
                    source_location: Some(loc.clone()),
                    tags: vec!["fix".to_string()],
                    created_at: now,
                });
                result.edges.push(MemoryEdge {
                    source: bug_id,
                    target: fix_id,
                    relation: EdgeRelation::FixedBy,
                    confidence: 0.8,
                    source_file: None,
                });
            }
            if content.contains("implemented") || content.contains("implements") {
                let label = extract_snippet(&turn.content, "implemented");
                result.push_node(MemoryNode {
                    id: String::new(),
                    label,
                    node_type: NodeType::Feature,
                    description: turn.content.clone(),
                    source_file: None,
                    source_location: Some(loc.clone()),
                    tags: vec!["feature".to_string()],
                    created_at: now,
                });
            }
            if content.contains("pattern") {
                let label = extract_snippet(&turn.content, "pattern");
                result.push_node(MemoryNode {
                    id: String::new(),
                    label,
                    node_type: NodeType::Pattern,
                    description: turn.content.clone(),
                    source_file: None,
                    source_location: Some(loc.clone()),
                    tags: vec!["pattern".to_string()],
                    created_at: now,
                });
            }
            if content.contains("concept") || content.contains("idea") {
                let label = extract_snippet(&turn.content, "concept");
                result.push_node(MemoryNode {
                    id: String::new(),
                    label,
                    node_type: NodeType::Concept,
                    description: turn.content.clone(),
                    source_file: None,
                    source_location: Some(loc.clone()),
                    tags: vec!["concept".to_string()],
                    created_at: now,
                });
            }
        }
        result
    }
}

fn extract_snippet(content: &str, keyword: &str) -> String {
    let lower = content.to_lowercase();
    if let Some(idx) = lower.find(keyword) {
        let start = idx;
        let end = (start + 60).min(content.len());
        content[start..end].trim().to_string()
    } else {
        content.chars().take(60).collect()
    }
}
