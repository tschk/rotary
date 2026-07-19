use super::engine::Skill;
#[cfg(all(test, feature = "skills"))]
use crate::embeddings::cosine_similarity;
#[cfg(feature = "skills")]
use crate::embeddings::{EmbedError, EmbeddingClient, SemanticSearch};

/// A simpler wrapper for auto-activating skills based on a prompt.
#[derive(Debug, Clone)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
    #[cfg(feature = "skills")]
    semantic: Option<SemanticSearch>,
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            skills: Vec::new(),
            #[cfg(feature = "skills")]
            semantic: None,
        }
    }

    /// Enable semantic ranking via embeddings (falls back to keyword match when unavailable).
    #[cfg(feature = "skills")]
    pub fn with_semantic_client(&mut self, client: EmbeddingClient) {
        self.semantic = Some(SemanticSearch::new(client));
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

    /// Return instructions for matching skills, ranked by semantic similarity when
    /// configured, otherwise by keyword trigger match; confidence breaks ties.
    pub fn auto_activate(&self, prompt: &str) -> Vec<String> {
        self.auto_activate_sync(prompt)
    }

    /// Async activation: embedding similarity when online, keyword fallback on error.
    #[cfg(feature = "skills")]
    pub async fn auto_activate_async(&mut self, prompt: &str) -> Vec<String> {
        let skills_snapshot: Vec<Skill> = self.skills.clone();
        if let Some(semantic) = self.semantic.as_mut() {
            match Self::rank_semantic_for_skills(prompt, &skills_snapshot, semantic).await {
                Ok(ids) if !ids.is_empty() => {
                    return Self::instructions_for_skills(&skills_snapshot, &ids)
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        self.auto_activate_sync(prompt)
    }

    fn auto_activate_sync(&self, prompt: &str) -> Vec<String> {
        let mut matched: Vec<&Skill> = self.match_prompt(prompt);
        matched.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        matched.iter().map(|s| s.instructions.clone()).collect()
    }

    #[cfg(feature = "skills")]
    async fn rank_semantic_for_skills(
        prompt: &str,
        skills: &[Skill],
        semantic: &mut SemanticSearch,
    ) -> Result<Vec<String>, EmbedError> {
        const MIN_SIM: f32 = 0.35;
        let refs: Vec<&Skill> = skills.iter().collect();
        let mut scored = semantic.search_skills(prompt, &refs, refs.len()).await?;
        scored.retain(|(_, sim)| *sim >= MIN_SIM);
        scored.sort_by(|a, b| {
            let ca = skills
                .iter()
                .find(|s| s.id == a.0)
                .map(|s| s.confidence)
                .unwrap_or(0.0);
            let cb = skills
                .iter()
                .find(|s| s.id == b.0)
                .map(|s| s.confidence)
                .unwrap_or(0.0);
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| cb.partial_cmp(&ca).unwrap_or(std::cmp::Ordering::Equal))
        });
        Ok(scored.into_iter().map(|(id, _)| id).collect())
    }

    #[cfg(feature = "skills")]
    fn instructions_for_skills(skills: &[Skill], ids: &[String]) -> Vec<String> {
        ids.iter()
            .filter_map(|id| skills.iter().find(|s| &s.id == id))
            .map(|s| s.instructions.clone())
            .collect()
    }

    /// Deterministic local vectors for unit tests (no network).
    #[cfg(all(test, feature = "skills"))]
    pub(crate) fn auto_activate_semantic_local(&self, prompt: &str) -> Vec<String> {
        let prompt_vec = local_embed(prompt);
        let mut scored: Vec<(f32, f64, String)> = self
            .skills
            .iter()
            .map(|s| {
                let sim = cosine_similarity(&prompt_vec, &local_embed(&s.description));
                (sim, s.confidence, s.instructions.clone())
            })
            .filter(|(sim, _, _)| *sim >= 0.2)
            .collect();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        scored.into_iter().map(|(_, _, instr)| instr).collect()
    }
}

#[cfg(all(test, feature = "skills"))]
fn local_embed(text: &str) -> Vec<f32> {
    let mut v = [0.0f32; 8];
    for (i, b) in text.as_bytes().iter().enumerate() {
        v[i % 8] += f32::from(*b) / 255.0;
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v.to_vec()
}

#[cfg(test)]
mod tests {
    use super::super::engine::{
        ConfidencePrior, ConversationTurn, SkillEngine, SkillError, SkillOutcome, SkillState,
    };
    use super::*;
    use chrono::Utc;
    use std::path::{Path, PathBuf};
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
        let expected = ConfidencePrior::default().mean(2, 1);
        assert!((s.confidence - expected).abs() < 1e-9);
    }

    #[test]
    fn test_confidence_prior_hyperparameters() {
        let prior = ConfidencePrior {
            alpha: 2.0,
            beta: 8.0,
        };
        assert!((prior.mean(0, 0) - 0.2).abs() < 1e-9);
        assert!((prior.mean(8, 2) - 0.5).abs() < 1e-9);
        let mut skill = Skill {
            id: "x".into(),
            name: "n".into(),
            description: "d".into(),
            trigger_patterns: vec![],
            instructions: "i".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            success_count: 3,
            failure_count: 1,
            confidence: 0.0,
            tags: vec![],
            source_conversation: None,
            pinned: false,
            state: SkillState::Active,
        };
        skill.recompute_confidence_with_prior(prior);
        assert!((skill.confidence - prior.mean(3, 1)).abs() < 1e-9);
    }

    #[cfg(feature = "skills")]
    #[test]
    fn test_semantic_local_ranks_deploy_skill() {
        let mut registry = SkillRegistry::new();
        let deploy = Skill {
            id: "d".into(),
            name: "deploy".into(),
            description: "how do I deploy to production kubernetes cluster".into(),
            trigger_patterns: vec!["zzz".into()],
            instructions: "deploy steps".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            success_count: 1,
            failure_count: 0,
            confidence: 0.9,
            tags: vec![],
            source_conversation: None,
            pinned: false,
            state: SkillState::Active,
        };
        let other = Skill {
            id: "o".into(),
            name: "lint".into(),
            description: "format rust source code with rustfmt cli".into(),
            trigger_patterns: vec!["zzz".into()],
            instructions: "lint steps".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            success_count: 0,
            failure_count: 0,
            confidence: 0.1,
            tags: vec![],
            source_conversation: None,
            pinned: false,
            state: SkillState::Active,
        };
        registry.register(other);
        registry.register(deploy);
        let activated = registry.auto_activate_semantic_local("how do I deploy to production");
        assert!(!activated.is_empty());
        assert_eq!(activated[0], "deploy steps");
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
