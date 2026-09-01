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

            self.extract_decision(&mut result, &content, turn, &loc, now);
            self.extract_bug(&mut result, &content, turn, &loc, now);
            self.extract_feature(&mut result, &content, turn, &loc, now);
            self.extract_pattern(&mut result, &content, turn, &loc, now);
            self.extract_concept(&mut result, &content, turn, &loc, now);
        }
        result
    }

    fn create_node(
        label: String,
        node_type: NodeType,
        description: String,
        source_location: String,
        tags: Vec<String>,
        created_at: chrono::DateTime<Utc>,
    ) -> MemoryNode {
        MemoryNode {
            id: String::new(),
            label,
            node_type,
            description,
            source_file: None,
            source_location: Some(source_location),
            tags,
            created_at,
        }
    }

    fn extract_decision(
        &self,
        result: &mut ExtractionResult,
        content: &str,
        turn: &ConversationTurn,
        loc: &str,
        now: chrono::DateTime<Utc>,
    ) {
        if content.contains("decided to") || content.contains("we decided") {
            let label = extract_snippet(&turn.content, "decided");
            result.push_node(Self::create_node(
                label,
                NodeType::Decision,
                turn.content.clone(),
                loc.to_string(),
                vec!["decision".to_string()],
                now,
            ));
        }
    }

    fn extract_bug(
        &self,
        result: &mut ExtractionResult,
        content: &str,
        turn: &ConversationTurn,
        loc: &str,
        now: chrono::DateTime<Utc>,
    ) {
        if content.contains("fixed bug")
            || content.contains("fixes bug")
            || content.contains("bug fix")
        {
            let bug_label = extract_snippet(&turn.content, "bug");
            let bug_id = result.push_node(Self::create_node(
                bug_label,
                NodeType::Bug,
                turn.content.clone(),
                loc.to_string(),
                vec!["bug".to_string()],
                now,
            ));
            let fix_id = result.push_node(Self::create_node(
                format!(
                    "Fix for {}",
                    turn.content.chars().take(40).collect::<String>()
                ),
                NodeType::Feature,
                turn.content.clone(),
                loc.to_string(),
                vec!["fix".to_string()],
                now,
            ));
            result.edges.push(MemoryEdge {
                source: bug_id,
                target: fix_id,
                relation: EdgeRelation::FixedBy,
                confidence: 0.8,
                source_file: None,
            });
        }
    }

    fn extract_feature(
        &self,
        result: &mut ExtractionResult,
        content: &str,
        turn: &ConversationTurn,
        loc: &str,
        now: chrono::DateTime<Utc>,
    ) {
        if content.contains("implemented") || content.contains("implements") {
            let label = extract_snippet(&turn.content, "implemented");
            result.push_node(Self::create_node(
                label,
                NodeType::Feature,
                turn.content.clone(),
                loc.to_string(),
                vec!["feature".to_string()],
                now,
            ));
        }
    }

    fn extract_pattern(
        &self,
        result: &mut ExtractionResult,
        content: &str,
        turn: &ConversationTurn,
        loc: &str,
        now: chrono::DateTime<Utc>,
    ) {
        if content.contains("pattern") {
            let label = extract_snippet(&turn.content, "pattern");
            result.push_node(Self::create_node(
                label,
                NodeType::Pattern,
                turn.content.clone(),
                loc.to_string(),
                vec!["pattern".to_string()],
                now,
            ));
        }
    }

    fn extract_concept(
        &self,
        result: &mut ExtractionResult,
        content: &str,
        turn: &ConversationTurn,
        loc: &str,
        now: chrono::DateTime<Utc>,
    ) {
        if content.contains("concept") || content.contains("idea") {
            let label = extract_snippet(&turn.content, "concept");
            result.push_node(Self::create_node(
                label,
                NodeType::Concept,
                turn.content.clone(),
                loc.to_string(),
                vec!["concept".to_string()],
                now,
            ));
        }
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
