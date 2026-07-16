//! Skill curator — lifecycle management for skills.
//!
//! Inspired by Hermes Agent's curator: a background orchestrator that
//! auto-transitions skill lifecycle states (`Active → Stale → Archived`).
//! Pinned skills bypass all auto-transitions. Archived skills are never
//! auto-deleted, only archived. Consolidation merges overlapping skills
//! into class-level umbrellas (OFF by default).
//!
//! Unlike a cron daemon, the curator is inactivity-triggered: callers invoke
//! `audit` / `apply_suggestions` when the agent has been idle.

use crate::skill_engine::{SkillEngine, SkillError, SkillState};

/// Configuration for the skill curator.
///
/// The host decides *when* to run `audit` — this config only controls
/// the thresholds and behavior of the audit itself.
#[derive(Debug, Clone)]
pub struct CuratorConfig {
    /// Days of inactivity before a skill is marked stale.
    pub stale_after_days: u64,
    /// Days of inactivity before a skill is archived.
    pub archive_after_days: u64,
    /// Whether to suggest consolidating overlapping skills (OFF by default).
    pub consolidate: bool,
    /// Maximum number of suggestions produced per audit run.
    pub max_suggestions_per_run: usize,
}

impl Default for CuratorConfig {
    fn default() -> Self {
        Self {
            stale_after_days: 30,
            archive_after_days: 90,
            consolidate: false,
            max_suggestions_per_run: 3,
        }
    }
}

/// The kind of improvement a curator suggestion recommends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestionKind {
    /// Archive the skill (set state to Archived).
    Archive,
    /// Mark the skill stale (set state to Stale).
    MarkStale,
    /// Consolidate the skill into another skill (target id).
    Consolidate(String),
    /// Update the skill's description (requires LLM; only logged).
    UpdateDescription,
}

/// A single curator suggestion for a skill.
#[derive(Debug, Clone)]
pub struct CuratorSuggestion {
    pub skill_id: String,
    pub skill_name: String,
    pub kind: SuggestionKind,
    pub reason: String,
}

/// The skill curator reviews skills and produces lifecycle suggestions.
#[derive(Debug, Clone)]
pub struct SkillCurator {
    pub config: CuratorConfig,
}

impl Default for SkillCurator {
    fn default() -> Self {
        Self::new(CuratorConfig::default())
    }
}

impl SkillCurator {
    /// Create a new curator with the given config.
    pub fn new(config: CuratorConfig) -> Self {
        Self { config }
    }

    /// Review all skills and produce suggestions, respecting
    /// `max_suggestions_per_run`.
    ///
    /// - Pinned skills are skipped.
    /// - Archived skills are skipped.
    /// - `updated_at` older than `archive_after_days` → Archive.
    /// - `updated_at` older than `stale_after_days` and state is Active → MarkStale.
    /// - Empty description → UpdateDescription.
    /// - If `consolidate` is enabled: overlapping trigger_patterns → Consolidate.
    pub fn audit(&self, engine: &SkillEngine) -> Vec<CuratorSuggestion> {
        let mut suggestions: Vec<CuratorSuggestion> = Vec::new();
        let now = chrono::Utc::now();
        let skills = engine.list();

        for skill in &skills {
            if suggestions.len() >= self.config.max_suggestions_per_run {
                break;
            }
            // Pinned skills bypass all auto-transitions.
            if skill.pinned {
                continue;
            }
            // Archived skills are never re-suggested.
            if skill.state == SkillState::Archived {
                continue;
            }

            let age_days = now
                .signed_duration_since(skill.updated_at)
                .num_days()
                .max(0) as u64;

            // Archive takes priority over stale.
            if age_days >= self.config.archive_after_days {
                suggestions.push(CuratorSuggestion {
                    skill_id: skill.id.clone(),
                    skill_name: skill.name.clone(),
                    kind: SuggestionKind::Archive,
                    reason: format!(
                        "Not updated in {} days (archive_after_days = {})",
                        age_days, self.config.archive_after_days
                    ),
                });
                continue;
            }

            if age_days >= self.config.stale_after_days && skill.state == SkillState::Active {
                suggestions.push(CuratorSuggestion {
                    skill_id: skill.id.clone(),
                    skill_name: skill.name.clone(),
                    kind: SuggestionKind::MarkStale,
                    reason: format!(
                        "Not updated in {} days (stale_after_days = {})",
                        age_days, self.config.stale_after_days
                    ),
                });
                continue;
            }

            if skill.description.trim().is_empty() {
                suggestions.push(CuratorSuggestion {
                    skill_id: skill.id.clone(),
                    skill_name: skill.name.clone(),
                    kind: SuggestionKind::UpdateDescription,
                    reason: "Skill has an empty description".to_string(),
                });
                continue;
            }
        }

        // Consolidation pass: find skills with overlapping trigger_patterns.
        if self.config.consolidate && suggestions.len() < self.config.max_suggestions_per_run {
            let overlap = self.find_overlapping(&skills);
            for (source, target) in overlap {
                if suggestions.len() >= self.config.max_suggestions_per_run {
                    break;
                }
                // Skip if either is already suggested for archive/stale.
                if suggestions
                    .iter()
                    .any(|s| s.skill_id == source.id || s.skill_id == target.id)
                {
                    continue;
                }
                if source.pinned || target.pinned {
                    continue;
                }
                if source.state == SkillState::Archived || target.state == SkillState::Archived {
                    continue;
                }
                suggestions.push(CuratorSuggestion {
                    skill_id: source.id.clone(),
                    skill_name: source.name.clone(),
                    kind: SuggestionKind::Consolidate(target.id.clone()),
                    reason: format!(
                        "Overlapping trigger patterns with '{}' — merge into umbrella",
                        target.name
                    ),
                });
            }
        }

        suggestions
    }

    /// Find pairs of skills with overlapping trigger_patterns.
    /// Returns `(source, target)` pairs where source should merge into target.
    fn find_overlapping<'a>(
        &self,
        skills: &[&'a crate::skill_engine::Skill],
    ) -> Vec<(
        &'a crate::skill_engine::Skill,
        &'a crate::skill_engine::Skill,
    )> {
        let mut pairs = Vec::new();
        for i in 0..skills.len() {
            for j in (i + 1)..skills.len() {
                let a = skills[i];
                let b = skills[j];
                let overlap = a
                    .trigger_patterns
                    .iter()
                    .filter(|t| b.trigger_patterns.iter().any(|o| o == *t))
                    .count();
                if overlap > 0 {
                    // Merge the lower-confidence skill into the higher one.
                    let (source, target) = if a.confidence >= b.confidence {
                        (b, a)
                    } else {
                        (a, b)
                    };
                    pairs.push((source, target));
                }
            }
        }
        pairs
    }

    /// Apply a set of suggestions to the engine.
    ///
    /// - `Archive`: set state to `Archived`.
    /// - `MarkStale`: set state to `Stale`.
    /// - `UpdateDescription`: skipped (requires LLM; only logged).
    /// - `Consolidate(target)`: merge source skill into target.
    pub fn apply_suggestions(
        &self,
        engine: &mut SkillEngine,
        suggestions: &[CuratorSuggestion],
    ) -> Result<(), SkillError> {
        for s in suggestions {
            match &s.kind {
                SuggestionKind::Archive => {
                    tracing::info!(
                        "[curator] Archiving skill '{}' ({}): {}",
                        s.skill_name,
                        s.skill_id,
                        s.reason
                    );
                    engine.set_state(&s.skill_id, SkillState::Archived)?;
                }
                SuggestionKind::MarkStale => {
                    tracing::info!(
                        "[curator] Marking skill '{}' ({}) stale: {}",
                        s.skill_name,
                        s.skill_id,
                        s.reason
                    );
                    engine.set_state(&s.skill_id, SkillState::Stale)?;
                }
                SuggestionKind::UpdateDescription => {
                    tracing::info!(
                        "[curator] Skill '{}' ({}) needs description update (requires LLM): {}",
                        s.skill_name,
                        s.skill_id,
                        s.reason
                    );
                    // Requires LLM — skip for now, just log.
                }
                SuggestionKind::Consolidate(target_id) => {
                    tracing::info!(
                        "[curator] Consolidating skill '{}' ({}) into '{}': {}",
                        s.skill_name,
                        s.skill_id,
                        target_id,
                        s.reason
                    );
                    engine.merge_skill(&s.skill_id, target_id)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_engine::{Skill, SkillEngine};
    use chrono::{Duration, Utc};
    use std::path::PathBuf;

    fn make_skill_with_age(name: &str, description: &str, days_old: i64) -> Skill {
        let now = Utc::now();
        Skill {
            id: name.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            trigger_patterns: vec![],
            instructions: "do the thing".to_string(),
            created_at: now - Duration::days(days_old),
            updated_at: now - Duration::days(days_old),
            success_count: 1,
            failure_count: 0,
            confidence: 0.5,
            tags: vec![],
            source_conversation: None,
            pinned: false,
            state: SkillState::Active,
        }
    }

    fn engine_with(skills: Vec<Skill>) -> SkillEngine {
        let mut engine = SkillEngine::new(PathBuf::from("/tmp/rx4-curator-test"));
        for s in skills {
            engine.insert_skill_for_test(s);
        }
        engine
    }

    #[test]
    fn test_audit_archives_old_skill() {
        // 100 days old — exceeds archive_after_days (90).
        let skill = make_skill_with_age("ancient", "an old skill", 100);
        let engine = engine_with(vec![skill]);
        let curator = SkillCurator::default();
        let suggestions = curator.audit(&engine);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].kind, SuggestionKind::Archive);
        assert_eq!(suggestions[0].skill_name, "ancient");

        // Apply and verify state.
        let mut engine = engine_with(vec![make_skill_with_age("ancient", "an old skill", 100)]);
        curator
            .apply_suggestions(&mut engine, &suggestions)
            .expect("apply");
        let s = engine.get("ancient").expect("exists");
        assert_eq!(s.state, SkillState::Archived);
    }

    #[test]
    fn test_audit_marks_stale() {
        // 45 days old — exceeds stale_after_days (30) but not archive_after_days (90).
        let skill = make_skill_with_age("stale_one", "a skill", 45);
        let engine = engine_with(vec![skill]);
        let curator = SkillCurator::default();
        let suggestions = curator.audit(&engine);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].kind, SuggestionKind::MarkStale);

        let mut engine = engine_with(vec![make_skill_with_age("stale_one", "a skill", 45)]);
        curator
            .apply_suggestions(&mut engine, &suggestions)
            .expect("apply");
        let s = engine.get("stale_one").expect("exists");
        assert_eq!(s.state, SkillState::Stale);
    }

    #[test]
    fn test_audit_skips_pinned() {
        // Old enough to archive, but pinned.
        let mut skill = make_skill_with_age("pinned_old", "a skill", 100);
        skill.pinned = true;
        let engine = engine_with(vec![skill]);
        let curator = SkillCurator::default();
        let suggestions = curator.audit(&engine);
        assert!(suggestions.is_empty(), "pinned skills should be skipped");
    }

    #[test]
    fn test_audit_skips_archived() {
        let mut skill = make_skill_with_age("already_archived", "a skill", 100);
        skill.state = SkillState::Archived;
        let engine = engine_with(vec![skill]);
        let curator = SkillCurator::default();
        let suggestions = curator.audit(&engine);
        assert!(suggestions.is_empty(), "archived skills should be skipped");
    }

    #[test]
    fn test_audit_suggests_update_description() {
        // Recent skill but empty description.
        let skill = make_skill_with_age("nodesc", "", 1);
        let engine = engine_with(vec![skill]);
        let curator = SkillCurator::default();
        let suggestions = curator.audit(&engine);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].kind, SuggestionKind::UpdateDescription);
    }

    #[test]
    fn test_audit_respects_max_suggestions() {
        let mut skills = Vec::new();
        for i in 0..10 {
            skills.push(make_skill_with_age(&format!("old_{i}"), "a skill", 100));
        }
        let engine = engine_with(skills);
        let curator = SkillCurator::default();
        let suggestions = curator.audit(&engine);
        assert_eq!(suggestions.len(), curator.config.max_suggestions_per_run);
    }

    #[test]
    fn test_audit_consolidate_finds_overlap() {
        let mut a = make_skill_with_age("skill_a", "does a", 1);
        a.trigger_patterns = vec!["deploy".into(), "release".into()];
        a.confidence = 0.8;
        let mut b = make_skill_with_age("skill_b", "does b", 1);
        b.trigger_patterns = vec!["deploy".into(), "ship".into()];
        b.confidence = 0.5;
        let engine = engine_with(vec![a, b]);
        let config = CuratorConfig {
            consolidate: true,
            max_suggestions_per_run: 10,
            ..Default::default()
        };
        let curator = SkillCurator::new(config);
        let suggestions = curator.audit(&engine);
        let consolidate = suggestions
            .iter()
            .find(|s| matches!(s.kind, SuggestionKind::Consolidate(_)));
        assert!(consolidate.is_some(), "should suggest consolidation");
        if let Some(s) = consolidate {
            // Lower-confidence skill_b should merge into skill_a.
            assert_eq!(s.skill_name, "skill_b");
            if let SuggestionKind::Consolidate(target) = &s.kind {
                assert_eq!(target, "skill_a");
            } else {
                panic!("expected Consolidate");
            }
        }
    }

    #[test]
    fn test_merge_skill_combines() {
        let mut a = make_skill_with_age("merge_a", "does a", 1);
        a.trigger_patterns = vec!["deploy".into(), "release".into()];
        a.instructions = "step A".to_string();
        a.tags = vec!["ops".into()];
        a.success_count = 3;
        a.failure_count = 1;

        let mut b = make_skill_with_age("merge_b", "does b", 1);
        b.trigger_patterns = vec!["deploy".into(), "ship".into()];
        b.instructions = "step B".to_string();
        b.tags = vec!["ops".into(), "ci".into()];
        b.success_count = 2;
        b.failure_count = 1;

        let mut engine = engine_with(vec![a, b]);
        engine.merge_skill("merge_b", "merge_a").expect("merge");

        // Source removed.
        assert!(engine.get("merge_b").is_none());
        let target = engine.get("merge_a").expect("target exists");
        // Union of trigger_patterns.
        assert!(target.trigger_patterns.contains(&"deploy".to_string()));
        assert!(target.trigger_patterns.contains(&"release".to_string()));
        assert!(target.trigger_patterns.contains(&"ship".to_string()));
        // Concatenated instructions.
        assert!(target.instructions.contains("step A"));
        assert!(target.instructions.contains("step B"));
        // Union of tags.
        assert!(target.tags.contains(&"ops".to_string()));
        assert!(target.tags.contains(&"ci".to_string()));
        // Summed counts.
        assert_eq!(target.success_count, 5);
        assert_eq!(target.failure_count, 2);
        // Recomputed confidence: (5 + 1) / (7 + 2) = 6/9 = 0.6667
        let expected = (5.0 + 1.0) / (7.0 + 2.0);
        assert!((target.confidence - expected).abs() < 1e-9);
    }

    #[test]
    fn test_merge_skill_rejects_self_merge() {
        let skill = make_skill_with_age("solo", "a skill", 1);
        let mut engine = engine_with(vec![skill]);
        let res = engine.merge_skill("solo", "solo");
        assert!(matches!(res, Err(SkillError::Extraction(_))));
    }

    #[test]
    fn test_set_pinned() {
        let skill = make_skill_with_age("pinme", "a skill", 1);
        let mut engine = engine_with(vec![skill]);
        engine.set_pinned("pinme", true).expect("pin");
        assert!(engine.get("pinme").expect("exists").pinned);
        engine.set_pinned("pinme", false).expect("unpin");
        assert!(!engine.get("pinme").expect("exists").pinned);
    }

    #[test]
    fn test_set_state() {
        let skill = make_skill_with_age("stateme", "a skill", 1);
        let mut engine = engine_with(vec![skill]);
        engine.set_state("stateme", SkillState::Stale).expect("set");
        assert_eq!(
            engine.get("stateme").expect("exists").state,
            SkillState::Stale
        );
    }
}
