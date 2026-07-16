//! Structured extraction: JSON contracts for knowledge/proactive extraction.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedKnowledge {
    pub topic: String,
    pub summary: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProactiveItem {
    pub title: String,
    pub description: String,
    pub priority: u8,
    pub action: String,
}

pub fn parse_knowledge(json: &str) -> Result<Vec<ExtractedKnowledge>, serde_json::Error> {
    serde_json::from_str(json)
}

pub fn parse_proactive(json: &str) -> Result<Vec<ProactiveItem>, serde_json::Error> {
    serde_json::from_str(json)
}
