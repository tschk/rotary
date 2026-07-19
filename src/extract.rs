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

/// Best-effort extract knowledge objects from free text / fenced JSON.
pub fn extract_knowledge_loose(text: &str) -> Vec<ExtractedKnowledge> {
    if let Some(slice) = first_json_array(text) {
        if let Ok(items) = parse_knowledge(slice) {
            return items;
        }
    }
    if let Ok(items) = parse_knowledge(text.trim()) {
        return items;
    }
    Vec::new()
}

/// Best-effort extract proactive items from free text / fenced JSON.
pub fn extract_proactive_loose(text: &str) -> Vec<ProactiveItem> {
    if let Some(slice) = first_json_array(text) {
        if let Ok(items) = parse_proactive(slice) {
            return items;
        }
    }
    if let Ok(items) = parse_proactive(text.trim()) {
        return items;
    }
    Vec::new()
}

fn first_json_array(text: &str) -> Option<&str> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    if end > start {
        Some(&text[start..=end])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_loose_extract() {
        let json = r#"[{"topic":"a","summary":"s","tags":["t"]}]"#;
        let items = parse_knowledge(json).unwrap();
        assert_eq!(items[0].topic, "a");
        let wrapped = format!("here is data:\n```json\n{json}\n```");
        let loose = extract_knowledge_loose(&wrapped);
        assert_eq!(loose.len(), 1);
    }
}
