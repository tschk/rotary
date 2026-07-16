//! Skill creation from experience — a self-improving learning loop.
//!
//! Skills are reusable units of agent behavior distilled from conversations.
//! Inspired by Hermes Agent's self-improving learning loop: observe what
//! worked, extract a reusable skill, refine confidence over time, prune
//! what no longer serves.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// YAML frontmatter parsed from a SKILL.md file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    pub id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub confidence: Option<f64>,
    pub success_count: Option<u32>,
    pub failure_count: Option<u32>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub tags: Option<Vec<String>>,
    pub trigger_patterns: Option<Vec<String>>,
    pub category: Option<String>,
    pub related_skills: Option<Vec<String>>,
}

/// Errors produced by the skill engine.
#[derive(Debug, Error)]
pub enum SkillError {
    #[error("io error: {0}")]
    Io(String),
    #[error("skill not found: {0}")]
    NotFound(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("extraction error: {0}")]
    Extraction(String),
    #[error("yaml error: {0}")]
    Yaml(String),
    #[error("parse error: {0}")]
    Parse(String),
}

impl From<std::io::Error> for SkillError {
    fn from(err: std::io::Error) -> Self {
        SkillError::Io(err.to_string())
    }
}

impl From<serde_json::Error> for SkillError {
    fn from(err: serde_json::Error) -> Self {
        SkillError::Serialization(err.to_string())
    }
}

#[cfg(feature = "pi-compat")]
impl From<serde_yaml::Error> for SkillError {
    fn from(err: serde_yaml::Error) -> Self {
        SkillError::Yaml(err.to_string())
    }
}

/// Outcome of a conversation that produced (or tested) a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillOutcome {
    Success,
    Failure,
    Partial,
}

/// Lifecycle state of a skill, managed by the curator.
///
/// Skills transition `Active → Stale → Archived`. Pinned skills bypass all
/// auto-transitions. Archived skills are never auto-deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SkillState {
    #[default]
    Active,
    Stale,
    Archived,
}

/// A single turn in a conversation used for skill extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: String,
    pub content: String,
    pub tool_calls: Vec<String>,
}

/// A reusable skill created from agent experience.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub trigger_patterns: Vec<String>,
    pub instructions: String,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    pub success_count: u32,
    pub failure_count: u32,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub source_conversation: Option<String>,
    /// Whether this skill is pinned (bypasses all auto-transitions).
    #[serde(default)]
    pub pinned: bool,
    /// Lifecycle state managed by the curator.
    #[serde(default)]
    pub state: SkillState,
}

impl Skill {
    /// Total number of recorded outcomes.
    pub fn total_outcomes(&self) -> u32 {
        self.success_count + self.failure_count
    }

    /// Recompute confidence using a Bayesian-ish prior:
    /// `confidence = (successes + 1) / (total + 2)`.
    pub fn recompute_confidence(&mut self) {
        let total = self.total_outcomes();
        self.confidence = (self.success_count as f64 + 1.0) / (total as f64 + 2.0);
    }
}

/// Manages the skill lifecycle: creation, retrieval, update, pruning, persistence.
#[derive(Debug, Clone)]
pub struct SkillEngine {
    skills: HashMap<String, Skill>,
    skills_dir: PathBuf,
    /// Additional directories scanned on load (e.g. project-local `.rx4/skills`).
    extra_dirs: Vec<PathBuf>,
}

impl Default for SkillEngine {
    fn default() -> Self {
        let dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".agents")
            .join("skills");
        Self::new(dir)
    }
}

impl SkillEngine {
    /// Create a new engine rooted at `skills_dir`.
    pub fn new(skills_dir: PathBuf) -> Self {
        Self {
            skills: HashMap::new(),
            skills_dir,
            extra_dirs: Vec::new(),
        }
    }

    /// Returns the configured skills directory.
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }

    /// Add an extra directory to scan on `load()`.
    pub fn add_extra_dir(&mut self, dir: PathBuf) {
        if !self.extra_dirs.contains(&dir) {
            self.extra_dirs.push(dir);
        }
    }

    /// Returns the extra (non-primary) skill directories.
    pub fn extra_dirs(&self) -> &[PathBuf] {
        &self.extra_dirs
    }

    /// Insert a pre-constructed skill into the engine (for testing).
    #[cfg(test)]
    pub fn insert_skill_for_test(&mut self, skill: Skill) {
        self.skills.insert(skill.id.clone(), skill);
    }

    /// Analyze a conversation and extract a reusable skill.
    ///
    /// Identifies repeated tool-call patterns, extracts the user's intent
    /// from the first user message, generates trigger patterns from
    /// keywords, and produces step-by-step instructions from what the
    /// agent did. Initial confidence is 0.5.
    pub fn create_from_conversation(
        &mut self,
        conversation: &[ConversationTurn],
        outcome: SkillOutcome,
    ) -> Result<Skill, SkillError> {
        if conversation.is_empty() {
            return Err(SkillError::Extraction(
                "cannot extract a skill from an empty conversation".into(),
            ));
        }

        let first_user = conversation
            .iter()
            .find(|t| t.role.eq_ignore_ascii_case("user"))
            .ok_or_else(|| SkillError::Extraction("no user turn found in conversation".into()))?;

        let intent = extract_intent(&first_user.content);
        let name = derive_name(&intent);
        let description = summarize_conversation(conversation);
        let trigger_patterns = extract_trigger_patterns(&first_user.content);
        let tags = extract_tags(&first_user.content);
        let instructions = generate_instructions(conversation);
        let tool_sequence = extract_tool_sequence(conversation);

        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let (success_count, failure_count) = match outcome {
            SkillOutcome::Success => (1, 0),
            SkillOutcome::Failure => (0, 1),
            SkillOutcome::Partial => (0, 0),
        };

        let mut skill = Skill {
            id: id.clone(),
            name,
            description,
            trigger_patterns,
            instructions,
            created_at: now,
            updated_at: now,
            success_count,
            failure_count,
            confidence: 0.5,
            tags,
            source_conversation: Some(tool_sequence),
            pinned: false,
            state: SkillState::Active,
        };
        skill.recompute_confidence();

        self.skills.insert(id.clone(), skill.clone());
        Ok(skill)
    }

    /// Manually create a skill.
    pub fn create_from_pattern(
        &mut self,
        name: &str,
        description: &str,
        instructions: &str,
        triggers: Vec<String>,
    ) -> Result<Skill, SkillError> {
        if name.trim().is_empty() {
            return Err(SkillError::Extraction("skill name is required".into()));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let skill = Skill {
            id: id.clone(),
            name: name.to_string(),
            description: description.to_string(),
            trigger_patterns: triggers,
            instructions: instructions.to_string(),
            created_at: now,
            updated_at: now,
            success_count: 0,
            failure_count: 0,
            confidence: 0.5,
            tags: Vec::new(),
            source_conversation: None,
            pinned: false,
            state: SkillState::Active,
        };
        self.skills.insert(id.clone(), skill.clone());
        Ok(skill)
    }

    /// Look up a skill by id.
    pub fn get(&self, id: &str) -> Option<&Skill> {
        self.skills.get(id)
    }

    /// List all skills, unordered.
    pub fn list(&self) -> Vec<&Skill> {
        self.skills.values().collect()
    }

    /// Keyword search across name, description, tags, and trigger patterns.
    pub fn search(&self, query: &str) -> Vec<&Skill> {
        let q = query.to_lowercase();
        let terms: Vec<&str> = q.split_whitespace().collect();
        if terms.is_empty() {
            return Vec::new();
        }
        self.skills
            .values()
            .filter(|s| {
                terms.iter().all(|term| {
                    s.name.to_lowercase().contains(term)
                        || s.description.to_lowercase().contains(term)
                        || s.tags.iter().any(|t| t.to_lowercase().contains(term))
                        || s.trigger_patterns
                            .iter()
                            .any(|t| t.to_lowercase().contains(term))
                })
            })
            .collect()
    }

    /// Adjust confidence based on an outcome using a Bayesian-ish update:
    /// `confidence = (successes + 1) / (total + 2)`.
    pub fn update_confidence(&mut self, id: &str, success: bool) -> Result<(), SkillError> {
        let skill = self
            .skills
            .get_mut(id)
            .ok_or_else(|| SkillError::NotFound(id.to_string()))?;
        if success {
            skill.success_count += 1;
        } else {
            skill.failure_count += 1;
        }
        skill.updated_at = Utc::now();
        skill.recompute_confidence();
        Ok(())
    }

    /// Set the lifecycle state of a skill (Active/Stale/Archived).
    pub fn set_state(&mut self, id: &str, state: SkillState) -> Result<(), SkillError> {
        let skill = self
            .skills
            .get_mut(id)
            .ok_or_else(|| SkillError::NotFound(id.to_string()))?;
        skill.state = state;
        skill.updated_at = Utc::now();
        Ok(())
    }

    /// Pin or unpin a skill. Pinned skills bypass all auto-transitions.
    pub fn set_pinned(&mut self, id: &str, pinned: bool) -> Result<(), SkillError> {
        let skill = self
            .skills
            .get_mut(id)
            .ok_or_else(|| SkillError::NotFound(id.to_string()))?;
        skill.pinned = pinned;
        Ok(())
    }

    /// Merge a source skill into a target skill.
    ///
    /// Unions trigger_patterns and tags, concatenates instructions, sums
    /// success/failure counts, recomputes confidence, and removes the source
    /// skill. Returns an error if either id is missing or they are the same.
    pub fn merge_skill(&mut self, source_id: &str, target_id: &str) -> Result<(), SkillError> {
        if source_id == target_id {
            return Err(SkillError::Extraction(
                "cannot merge a skill into itself".into(),
            ));
        }
        // Take the source out first so we can mutably borrow the target.
        let source = self
            .skills
            .remove(source_id)
            .ok_or_else(|| SkillError::NotFound(source_id.to_string()))?;
        let target = self
            .skills
            .get_mut(target_id)
            .ok_or_else(|| SkillError::NotFound(target_id.to_string()))?;

        // Union trigger_patterns (preserve order, no duplicates).
        for pat in source.trigger_patterns {
            if !target.trigger_patterns.contains(&pat) {
                target.trigger_patterns.push(pat);
            }
        }

        // Union tags.
        for tag in source.tags {
            if !target.tags.contains(&tag) {
                target.tags.push(tag);
            }
        }

        // Concatenate instructions.
        if !source.instructions.is_empty() {
            if target.instructions.is_empty() {
                target.instructions = source.instructions;
            } else {
                target.instructions = format!("{}\n\n{}", target.instructions, source.instructions);
            }
        }

        // Sum counts and recompute confidence.
        target.success_count += source.success_count;
        target.failure_count += source.failure_count;
        target.recompute_confidence();
        target.updated_at = Utc::now();
        Ok(())
    }

    /// Update the instructions on an existing skill and refresh `updated_at`.
    pub fn update_instructions(&mut self, id: &str, instructions: &str) -> Result<(), SkillError> {
        let skill = self
            .skills
            .get_mut(id)
            .ok_or_else(|| SkillError::NotFound(id.to_string()))?;
        skill.instructions = instructions.to_string();
        skill.updated_at = Utc::now();
        Ok(())
    }

    /// Remove skills whose confidence is below `min_confidence`.
    ///
    /// Returns the ids of the pruned skills.
    pub fn prune(&mut self, min_confidence: f64) -> Vec<String> {
        let to_remove: Vec<String> = self
            .skills
            .iter()
            .filter(|(_, s)| s.confidence < min_confidence)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &to_remove {
            self.skills.remove(id);
        }
        to_remove
    }

    /// Persist all skills to `<skills_dir>/{id}.json`.
    pub fn save(&self) -> Result<(), SkillError> {
        std::fs::create_dir_all(&self.skills_dir)?;
        for skill in self.skills.values() {
            let path = self.skills_dir.join(format!("{}.json", skill.id));
            let json = serde_json::to_string_pretty(skill)?;
            std::fs::write(path, json)?;
        }
        Ok(())
    }

    /// Load all skills from disk into the engine, replacing in-memory state.
    ///
    /// Scans the primary `skills_dir` and any `extra_dirs` for both `.json`
    /// skill files and `SKILL.md` files (with YAML frontmatter). Directory-based
    /// skills (`{name}/SKILL.md`) are also discovered.
    pub fn load(&mut self) -> Result<(), SkillError> {
        self.skills.clear();
        let mut dirs = vec![self.skills_dir.clone()];
        dirs.extend(self.extra_dirs.iter().cloned());
        for dir in &dirs {
            if !dir.exists() {
                continue;
            }
            self.load_dir(dir)?;
        }
        Ok(())
    }

    fn load_dir(&mut self, dir: &Path) -> Result<(), SkillError> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    let skill = self.import_markdown_file(&skill_md)?;
                    self.skills.insert(skill.id.clone(), skill);
                }
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str());
            match ext {
                Some("json") => {
                    let data = std::fs::read_to_string(&path)?;
                    let skill: Skill = serde_json::from_str(&data)?;
                    self.skills.insert(skill.id.clone(), skill);
                }
                Some("md") => {
                    let skill = self.import_markdown_file(&path)?;
                    self.skills.insert(skill.id.clone(), skill);
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Import a SKILL.md file (YAML frontmatter + markdown body) into a `Skill`.
    pub fn import_markdown_file(&self, path: &Path) -> Result<Skill, SkillError> {
        let content = std::fs::read_to_string(path)?;
        Self::parse_skill_markdown(&content)
    }

    /// Parse SKILL.md content into a `Skill` struct.
    pub fn parse_skill_markdown(content: &str) -> Result<Skill, SkillError> {
        let (frontmatter_text, body) = split_frontmatter(content)?;
        let mut fm: SkillFrontmatter = if frontmatter_text.trim().is_empty() {
            SkillFrontmatter::default()
        } else {
            #[cfg(feature = "pi-compat")]
            {
                serde_yaml::from_str(&frontmatter_text)?
            }
            #[cfg(not(feature = "pi-compat"))]
            {
                parse_frontmatter_simple(&frontmatter_text)
            }
        };

        let id = fm
            .id
            .take()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let now = Utc::now();
        let created_at = fm
            .created_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(now);
        let updated_at = fm
            .updated_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(created_at);

        let (name, description, instructions, trigger_patterns) = parse_markdown_body(&body, &fm);

        let mut skill = Skill {
            id,
            name,
            description,
            trigger_patterns,
            instructions,
            created_at,
            updated_at,
            success_count: fm.success_count.unwrap_or(0),
            failure_count: fm.failure_count.unwrap_or(0),
            confidence: fm.confidence.unwrap_or(0.5),
            tags: fm.tags.take().unwrap_or_default(),
            source_conversation: None,
            pinned: false,
            state: SkillState::Active,
        };
        skill.recompute_confidence();
        Ok(skill)
    }

    /// Export a skill as a SKILL.md document with YAML frontmatter.
    pub fn export_markdown(&self, id: &str) -> Result<String, SkillError> {
        let skill = self
            .get(id)
            .ok_or_else(|| SkillError::NotFound(id.to_string()))?;
        let frontmatter = format!(
            "---\n\
             id: {id}\n\
             name: {name}\n\
             confidence: {confidence:.4}\n\
             success_count: {success}\n\
             failure_count: {failure}\n\
             created_at: {created}\n\
             updated_at: {updated}\n\
             tags: [{tags}]\n\
             ---\n",
            id = skill.id,
            name = skill.name,
            confidence = skill.confidence,
            success = skill.success_count,
            failure = skill.failure_count,
            created = skill.created_at.to_rfc3339(),
            updated = skill.updated_at.to_rfc3339(),
            tags = skill.tags.join(", "),
        );
        let triggers = skill
            .trigger_patterns
            .iter()
            .map(|t| format!("- {}", t))
            .collect::<Vec<_>>()
            .join("\n");
        let body = format!(
            "# {name}\n\n\
             {desc}\n\n\
             ## Triggers\n\n\
             {triggers}\n\n\
             ## Instructions\n\n\
             {instructions}\n",
            name = skill.name,
            desc = skill.description,
            triggers = if triggers.is_empty() {
                "- (none)".to_string()
            } else {
                triggers
            },
            instructions = skill.instructions,
        );
        Ok(format!("{}{}", frontmatter, body))
    }
}

/// Extract the user's intent from a message — the first sentence, trimmed.
fn extract_intent(content: &str) -> String {
    let trimmed = content.trim();
    let end = trimmed.find(['.', '!', '?', '\n']).unwrap_or(trimmed.len());
    let intent = trimmed[..end].trim();
    if intent.is_empty() {
        trimmed.to_string()
    } else {
        intent.to_string()
    }
}

/// Derive a short, slug-friendly name from an intent string.
fn derive_name(intent: &str) -> String {
    let words: Vec<&str> = intent.split_whitespace().take(6).collect();
    let joined = words.join("_");
    let cleaned: String = joined
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let collapsed = cleaned.trim_matches('_').to_string();
    if collapsed.is_empty() {
        "unnamed_skill".to_string()
    } else {
        collapsed
    }
}

/// Summarize a conversation into a single description line.
fn summarize_conversation(conversation: &[ConversationTurn]) -> String {
    let user_msgs: Vec<&str> = conversation
        .iter()
        .filter(|t| t.role.eq_ignore_ascii_case("user"))
        .map(|t| t.content.as_str())
        .collect();
    let tool_count: usize = conversation.iter().map(|t| t.tool_calls.len()).sum();
    let first = user_msgs.first().copied().unwrap_or("(no user message)");
    format!(
        "Skill distilled from a {}-turn conversation ({} tool calls). Intent: {}",
        conversation.len(),
        tool_count,
        truncate(first, 120),
    )
}

/// Extract trigger patterns from keywords in the user's message.
fn extract_trigger_patterns(content: &str) -> Vec<String> {
    let stop = [
        "the", "a", "an", "to", "of", "and", "or", "for", "in", "on", "is", "are", "be", "with",
        "that", "this", "it", "i", "you", "we", "please", "can", "could", "would", "should", "do",
        "does", "my", "me",
    ];
    let mut seen = std::collections::BTreeSet::new();
    let mut patterns = Vec::new();
    for word in content.split_whitespace() {
        let lower = word
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .to_lowercase();
        if lower.len() < 3 || stop.contains(&lower.as_str()) {
            continue;
        }
        if seen.insert(lower.clone()) {
            patterns.push(lower);
        }
        if patterns.len() >= 8 {
            break;
        }
    }
    if patterns.is_empty() {
        patterns.push("general".to_string());
    }
    patterns
}

/// Extract lowercase tags from the user's message (top keywords).
fn extract_tags(content: &str) -> Vec<String> {
    let mut tags = extract_trigger_patterns(content);
    tags.truncate(5);
    tags
}

/// Build a compact representation of the tool-call sequence across turns.
fn extract_tool_sequence(conversation: &[ConversationTurn]) -> String {
    let seq: Vec<&str> = conversation
        .iter()
        .flat_map(|t| t.tool_calls.iter().map(String::as_str))
        .collect();
    seq.join(" -> ")
}

/// Generate step-by-step instructions from what the agent did.
fn generate_instructions(conversation: &[ConversationTurn]) -> String {
    let mut steps: Vec<String> = Vec::new();
    let mut step_no = 1u32;
    for turn in conversation {
        if turn.role.eq_ignore_ascii_case("user") {
            steps.push(format!(
                "{}. Understand the request: {}",
                step_no,
                truncate(&turn.content, 100)
            ));
            step_no += 1;
        } else if turn.role.eq_ignore_ascii_case("assistant") {
            if !turn.tool_calls.is_empty() {
                let calls = turn.tool_calls.join(", ");
                steps.push(format!("{}. Use tools: {}", step_no, calls));
                step_no += 1;
            }
            if !turn.content.trim().is_empty() {
                steps.push(format!(
                    "{}. Respond: {}",
                    step_no,
                    truncate(&turn.content, 100)
                ));
                step_no += 1;
            }
        }
    }
    if steps.is_empty() {
        "No actionable steps could be extracted.".to_string()
    } else {
        steps.join("\n")
    }
}

/// Truncate a string to at most `max` chars, appending an ellipsis if cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut)
}

/// Split a SKILL.md document into (frontmatter_text, body_text).
/// Frontmatter is delimited by `---` lines at the start of the file.
fn split_frontmatter(content: &str) -> Result<(String, String), SkillError> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Ok((String::new(), content.to_string()));
    }
    let after_first = &trimmed[3..];
    let end = after_first
        .find("\n---")
        .ok_or_else(|| SkillError::Parse("frontmatter not closed".into()))?;
    let frontmatter = after_first[..end].trim().to_string();
    let rest_start = end + 4;
    let body = if rest_start < after_first.len() {
        after_first[rest_start..].trim_start().to_string()
    } else {
        String::new()
    };
    Ok((frontmatter, body))
}

/// Parse the markdown body to extract name, description, instructions, and triggers.
fn parse_markdown_body(body: &str, fm: &SkillFrontmatter) -> (String, String, String, Vec<String>) {
    let name = fm.name.clone().unwrap_or_else(|| {
        let h1 = body
            .lines()
            .find(|l| l.starts_with("# "))
            .map(|l| l[2..].trim().to_string())
            .unwrap_or_else(|| "unnamed_skill".to_string());
        h1
    });

    let description = fm.description.clone().unwrap_or_else(|| {
        body.lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .next()
            .map(|l| l.trim().to_string())
            .unwrap_or_default()
    });

    let trigger_patterns = fm
        .trigger_patterns
        .clone()
        .unwrap_or_else(|| extract_section_items(body, "Triggers"));

    let instructions = extract_section_body(body, "Instructions").unwrap_or_default();
    let instructions = if instructions.is_empty() {
        body.to_string()
    } else {
        instructions
    };

    (name, description, instructions, trigger_patterns)
}

/// Extract bullet items from a `## Section` in the markdown body.
fn extract_section_items(body: &str, section: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut in_section = false;
    for line in body.lines() {
        if line.starts_with("## ") {
            in_section = line[3..].trim().eq_ignore_ascii_case(section);
            continue;
        }
        if in_section && line.starts_with("- ") {
            items.push(line[2..].trim().to_string());
        }
    }
    items
}

/// Extract the text content under a `## Section` until the next section.
fn extract_section_body(body: &str, section: &str) -> Option<String> {
    let mut in_section = false;
    let mut collected: Vec<&str> = Vec::new();
    for line in body.lines() {
        if line.starts_with("## ") {
            if in_section {
                break;
            }
            in_section = line[3..].trim().eq_ignore_ascii_case(section);
            continue;
        }
        if in_section {
            collected.push(line);
        }
    }
    if collected.is_empty() {
        None
    } else {
        let text = collected
            .join("\n")
            .trim()
            .trim_start_matches(|c| c == '-' || c == ' ')
            .to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

/// Minimal frontmatter parser for when `serde_yaml` is not available.
/// Handles simple `key: value` and `key: [a, b, c]` lines.
fn parse_frontmatter_simple(text: &str) -> Result<SkillFrontmatter, SkillError> {
    let mut fm = SkillFrontmatter::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, val) = match line.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };
        match key {
            "id" => fm.id = Some(val.to_string()),
            "name" => fm.name = Some(val.to_string()),
            "description" => fm.description = Some(val.to_string()),
            "confidence" => {
                fm.confidence = val.parse().ok();
            }
            "success_count" => fm.success_count = val.parse().ok(),
            "failure_count" => fm.failure_count = val.parse().ok(),
            "tags" => fm.tags = Some(parse_yaml_list(val)),
            "trigger_patterns" => fm.trigger_patterns = Some(parse_yaml_list(val)),
            "category" => fm.category = Some(val.to_string()),
            "related_skills" => fm.related_skills = Some(parse_yaml_list(val)),
            _ => {}
        }
    }
    Ok(fm)
}

/// Parse a YAML inline list like `[a, b, c]` or `a, b, c`.
fn parse_yaml_list(val: &str) -> Vec<String> {
    let inner = val
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(val);
    inner
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// A simpler wrapper for auto-activating skills based on a prompt.
#[derive(Debug, Default, Clone)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a skill into the registry.
    pub fn register(&mut self, skill: Skill) {
        self.skills.push(skill);
    }

    /// Return skills whose trigger patterns match the prompt (case-insensitive).
    pub fn match_prompt(&self, prompt: &str) -> Vec<&Skill> {
        let p = prompt.to_lowercase();
        self.skills
            .iter()
            .filter(|s| {
                s.trigger_patterns
                    .iter()
                    .any(|t| p.contains(&t.to_lowercase()))
            })
            .collect()
    }

    /// Return instructions for matching skills, sorted by confidence (desc).
    pub fn auto_activate(&self, prompt: &str) -> Vec<String> {
        let mut matched: Vec<&Skill> = self.match_prompt(prompt);
        matched.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        matched.iter().map(|s| s.instructions.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_conversation() -> Vec<ConversationTurn> {
        vec![
            ConversationTurn {
                role: "user".into(),
                content: "Find the failing test and fix the assertion in parser.rs".into(),
                tool_calls: vec![],
            },
            ConversationTurn {
                role: "assistant".into(),
                content: "I'll search for the failing test.".into(),
                tool_calls: vec!["grep".into()],
            },
            ConversationTurn {
                role: "assistant".into(),
                content: "Now let me read the file.".into(),
                tool_calls: vec!["read".into()],
            },
            ConversationTurn {
                role: "assistant".into(),
                content: "Applying the fix.".into(),
                tool_calls: vec!["edit".into()],
            },
            ConversationTurn {
                role: "assistant".into(),
                content: "The assertion is fixed.".into(),
                tool_calls: vec![],
            },
        ]
    }

    #[test]
    fn test_create_from_pattern() {
        let mut engine = SkillEngine::new(PathBuf::from("/tmp/rx4-skills-test"));
        let skill = engine
            .create_from_pattern(
                "refactor_module",
                "Refactor a Rust module",
                "1. Identify module\n2. Extract traits",
                vec!["refactor".into(), "module".into()],
            )
            .expect("create_from_pattern");
        assert_eq!(skill.name, "refactor_module");
        assert_eq!(skill.trigger_patterns.len(), 2);
        assert_eq!(skill.confidence, 0.5);
        assert!(engine.get(&skill.id).is_some());
    }

    #[test]
    fn test_create_from_pattern_rejects_empty_name() {
        let mut engine = SkillEngine::new(PathBuf::from("/tmp/rx4-skills-test"));
        let res = engine.create_from_pattern("", "d", "i", vec![]);
        assert!(matches!(res, Err(SkillError::Extraction(_))));
    }

    #[test]
    fn test_create_from_conversation() {
        let mut engine = SkillEngine::new(PathBuf::from("/tmp/rx4-skills-test"));
        let conv = sample_conversation();
        let skill = engine
            .create_from_conversation(&conv, SkillOutcome::Success)
            .expect("create_from_conversation");
        assert!(!skill.trigger_patterns.is_empty());
        assert!(skill.trigger_patterns.iter().any(|t| t.contains("failing")));
        assert!(skill.instructions.contains("1."));
        assert!(skill.source_conversation.is_some());
        assert!(skill.source_conversation.as_ref().unwrap().contains("grep"));
        assert_eq!(skill.success_count, 1);
    }

    #[test]
    fn test_create_from_conversation_empty() {
        let mut engine = SkillEngine::new(PathBuf::from("/tmp/rx4-skills-test"));
        let res = engine.create_from_conversation(&[], SkillOutcome::Success);
        assert!(matches!(res, Err(SkillError::Extraction(_))));
    }

    #[test]
    fn test_confidence_bayesian() {
        let mut engine = SkillEngine::new(PathBuf::from("/tmp/rx4-skills-test"));
        let skill = engine
            .create_from_pattern("s", "d", "i", vec![])
            .expect("create");
        let id = skill.id.clone();
        engine.update_confidence(&id, true).expect("ok");
        engine.update_confidence(&id, true).expect("ok");
        engine.update_confidence(&id, false).expect("ok");
        let s = engine.get(&id).expect("exists");
        assert_eq!(s.success_count, 2);
        assert_eq!(s.failure_count, 1);
        let expected = (2.0 + 1.0) / (3.0 + 2.0);
        assert!((s.confidence - expected).abs() < 1e-9);
    }

    #[test]
    fn test_confidence_update_missing() {
        let mut engine = SkillEngine::new(PathBuf::from("/tmp/rx4-skills-test"));
        let res = engine.update_confidence("nope", true);
        assert!(matches!(res, Err(SkillError::NotFound(_))));
    }

    #[test]
    fn test_search() {
        let mut engine = SkillEngine::new(PathBuf::from("/tmp/rx4-skills-test"));
        engine
            .create_from_pattern(
                "grep_skill",
                "search code with grep",
                "run grep",
                vec!["grep".into()],
            )
            .expect("c1");
        engine
            .create_from_pattern(
                "edit_skill",
                "edit files safely",
                "run edit",
                vec!["edit".into()],
            )
            .expect("c2");
        let hits = engine.search("grep");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "grep_skill");
        let hits2 = engine.search("edit");
        assert_eq!(hits2.len(), 1);
        assert_eq!(hits2[0].name, "edit_skill");
    }

    #[test]
    fn test_prune() {
        let mut engine = SkillEngine::new(PathBuf::from("/tmp/rx4-skills-test"));
        let a = engine
            .create_from_pattern("low", "d", "i", vec![])
            .expect("a");
        let b = engine
            .create_from_pattern("high", "d", "i", vec![])
            .expect("b");
        for _ in 0..5 {
            engine.update_confidence(&b.id, true).expect("ok");
        }
        engine.update_confidence(&a.id, false).expect("ok");
        let pruned = engine.prune(0.5);
        assert!(pruned.contains(&a.id));
        assert!(!pruned.contains(&b.id));
        assert!(engine.get(&a.id).is_none());
        assert!(engine.get(&b.id).is_some());
    }

    #[test]
    fn test_export_markdown_frontmatter() {
        let mut engine = SkillEngine::new(PathBuf::from("/tmp/rx4-skills-test"));
        let skill = engine
            .create_from_pattern(
                "md_skill",
                "a skill",
                "do the thing",
                vec!["trigger".into()],
            )
            .expect("create");
        let md = engine.export_markdown(&skill.id).expect("export");
        assert!(md.starts_with("---\n"));
        assert!(md.contains("id: "));
        assert!(md.contains("name: md_skill"));
        assert!(md.contains("confidence:"));
        assert!(md.contains("# md_skill"));
        assert!(md.contains("## Triggers"));
        assert!(md.contains("- trigger"));
        assert!(md.contains("## Instructions"));
        assert!(md.contains("do the thing"));
    }

    #[test]
    fn test_export_markdown_missing() {
        let engine = SkillEngine::new(PathBuf::from("/tmp/rx4-skills-test"));
        let res = engine.export_markdown("missing");
        assert!(matches!(res, Err(SkillError::NotFound(_))));
    }

    #[test]
    fn test_registry_matching() {
        let mut engine = SkillEngine::new(PathBuf::from("/tmp/rx4-skills-test"));
        let skill = engine
            .create_from_pattern(
                "fix_test",
                "fix failing tests",
                "run cargo test",
                vec!["failing".into(), "test".into()],
            )
            .expect("create");
        let mut registry = SkillRegistry::new();
        registry.register(skill);
        let matches = registry.match_prompt("please fix the failing test in parser");
        assert_eq!(matches.len(), 1);
        let no_matches = registry.match_prompt("deploy the server");
        assert!(no_matches.is_empty());
    }

    #[test]
    fn test_auto_activate_sorted_by_confidence() {
        let mut registry = SkillRegistry::new();
        let mut low = Skill {
            id: "low".into(),
            name: "low".into(),
            description: "d".into(),
            trigger_patterns: vec!["deploy".into()],
            instructions: "low instructions".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            success_count: 0,
            failure_count: 5,
            confidence: 0.1,
            tags: vec![],
            source_conversation: None,
            pinned: false,
            state: SkillState::Active,
        };
        low.recompute_confidence();
        let mut high = Skill {
            id: "high".into(),
            name: "high".into(),
            description: "d".into(),
            trigger_patterns: vec!["deploy".into()],
            instructions: "high instructions".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            success_count: 10,
            failure_count: 0,
            confidence: 0.9,
            tags: vec![],
            source_conversation: None,
            pinned: false,
            state: SkillState::Active,
        };
        high.recompute_confidence();
        registry.register(low);
        registry.register(high);
        let activated = registry.auto_activate("deploy the app");
        assert_eq!(activated.len(), 2);
        assert_eq!(activated[0], "high instructions");
        assert_eq!(activated[1], "low instructions");
    }

    #[test]
    fn test_save_load_roundtrip() {
        let dir = tempdir().expect("tempdir");
        let mut engine = SkillEngine::new(dir.path().to_path_buf());
        let a = engine
            .create_from_pattern("alpha", "first skill", "step 1", vec!["a".into()])
            .expect("create a");
        let b = engine
            .create_from_pattern("beta", "second skill", "step 2", vec!["b".into()])
            .expect("create b");
        engine.save().expect("save");

        let mut engine2 = SkillEngine::new(dir.path().to_path_buf());
        engine2.load().expect("load");
        assert_eq!(engine2.list().len(), 2);
        assert!(engine2.get(&a.id).is_some());
        assert!(engine2.get(&b.id).is_some());
        assert_eq!(engine2.get(&a.id).expect("a").name, "alpha");
    }

    #[test]
    fn test_load_missing_dir_is_ok() {
        let mut engine = SkillEngine::new(PathBuf::from("/nonexistent/rx4/skills/xyz"));
        let res = engine.load();
        assert!(res.is_ok());
        assert!(engine.list().is_empty());
    }

    #[test]
    fn test_skill_outcome_serialization() {
        let s = serde_json::to_string(&SkillOutcome::Success).expect("ser");
        assert_eq!(s, "\"Success\"");
        let p: SkillOutcome = serde_json::from_str("\"Partial\"").expect("de");
        assert_eq!(p, SkillOutcome::Partial);
        let f: SkillOutcome = serde_json::from_str("\"Failure\"").expect("de");
        assert_eq!(f, SkillOutcome::Failure);
        let round: SkillOutcome =
            serde_json::from_str(&serde_json::to_string(&SkillOutcome::Failure).expect("ser"))
                .expect("round");
        assert_eq!(round, SkillOutcome::Failure);
    }

    #[test]
    fn test_parse_skill_markdown_with_frontmatter() {
        let md = "---\n\
                  name: deploy_skill\n\
                  description: Deploy the app\n\
                  tags: [deploy, prod]\n\
                  trigger_patterns: [deploy, release]\n\
                  ---\n\
                  # deploy_skill\n\n\
                  ## Triggers\n\n\
                  - deploy\n\
                  - release\n\n\
                  ## Instructions\n\n\
                  1. Build\n\
                  2. Ship\n";
        let skill = SkillEngine::parse_skill_markdown(md).expect("parse");
        assert_eq!(skill.name, "deploy_skill");
        assert_eq!(skill.description, "Deploy the app");
        assert!(skill.tags.contains(&"deploy".to_string()));
        assert!(skill.trigger_patterns.contains(&"deploy".to_string()));
        assert!(skill.instructions.contains("Build"));
        assert!(skill.instructions.contains("Ship"));
    }

    #[test]
    fn test_parse_skill_markdown_no_frontmatter() {
        let md = "# my_skill\n\nDo the thing.\n";
        let skill = SkillEngine::parse_skill_markdown(md).expect("parse");
        assert_eq!(skill.name, "my_skill");
        assert!(!skill.id.is_empty());
    }

    #[test]
    fn test_export_then_import_roundtrip() {
        let mut engine = SkillEngine::new(PathBuf::from("/tmp/rx4-skills-test"));
        let skill = engine
            .create_from_pattern(
                "roundtrip",
                "A skill for roundtrip testing",
                "1. Do thing\n2. Done",
                vec!["roundtrip".into()],
            )
            .expect("create");
        let md = engine.export_markdown(&skill.id).expect("export");
        let parsed = SkillEngine::parse_skill_markdown(&md).expect("import");
        assert_eq!(parsed.name, skill.name);
        assert_eq!(parsed.description, skill.description);
        assert_eq!(parsed.trigger_patterns, skill.trigger_patterns);
        assert_eq!(parsed.instructions, skill.instructions);
    }

    #[test]
    fn test_load_mixed_json_and_md() {
        let dir = tempdir().expect("tempdir");

        let json_skill = Skill {
            id: "json-1".to_string(),
            name: "json_skill".to_string(),
            description: "from json".to_string(),
            trigger_patterns: vec!["json".to_string()],
            instructions: "json instructions".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            success_count: 1,
            failure_count: 0,
            confidence: 0.67,
            tags: vec!["json".to_string()],
            source_conversation: None,
            pinned: false,
            state: SkillState::Active,
        };
        let json_path = dir.path().join("json-1.json");
        std::fs::write(
            &json_path,
            serde_json::to_string_pretty(&json_skill).unwrap(),
        )
        .unwrap();

        let md_content = "---\n\
                          name: md_skill\n\
                          description: from markdown\n\
                          ---\n\
                          # md_skill\n\n\
                          ## Instructions\n\n\
                          md instructions\n";
        std::fs::write(dir.path().join("md_skill.md"), md_content).unwrap();

        let mut engine = SkillEngine::new(dir.path().to_path_buf());
        engine.load().expect("load");
        assert_eq!(engine.list().len(), 2);
        assert!(engine.get("json-1").is_some());
        assert!(engine.list().iter().any(|s| s.name == "md_skill"));
    }

    #[test]
    fn test_load_directory_based_skill() {
        let dir = tempdir().expect("tempdir");
        let skill_dir = dir.path().join("my_dir_skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let md_content = "---\n\
                          name: dir_skill\n\
                          description: A directory-based skill\n\
                          ---\n\
                          # dir_skill\n\n\
                          ## Instructions\n\n\
                          Run from directory.\n";
        std::fs::write(skill_dir.join("SKILL.md"), md_content).unwrap();

        let mut engine = SkillEngine::new(dir.path().to_path_buf());
        engine.load().expect("load");
        assert_eq!(engine.list().len(), 1);
        assert_eq!(engine.list()[0].name, "dir_skill");
    }

    #[test]
    fn test_load_from_extra_dir() {
        let primary = tempdir().expect("primary");
        let extra = tempdir().expect("extra");

        let md_content = "---\n\
                          name: extra_skill\n\
                          description: From extra dir\n\
                          ---\n\
                          # extra_skill\n\n\
                          Extra body.\n";
        std::fs::write(extra.path().join("extra.md"), md_content).unwrap();

        let mut engine = SkillEngine::new(primary.path().to_path_buf());
        engine.add_extra_dir(extra.path().to_path_buf());
        engine.load().expect("load");
        assert_eq!(engine.list().len(), 1);
        assert_eq!(engine.list()[0].name, "extra_skill");
    }
}
