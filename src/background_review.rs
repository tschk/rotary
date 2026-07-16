//! Background review loop — observe, distill, refine.
//!
//! After every conversation turn, fork a lightweight review that looks for
//! learning signals. Signals: user corrections (style, workflow, approach),
//! non-trivial techniques/workarounds, skills that were wrong/missing.
//! Prefer updating existing skills over creating new ones.
//!
//! Guardrails: don't capture environment-dependent failures, session-specific
//! errors, or one-off narratives. Inspired by Hermes Agent's self-improving
//! learning loop.

use crate::skill_engine::{ConversationTurn, SkillEngine, SkillError, SkillOutcome};

/// Configuration for the background review loop.
#[derive(Debug, Clone)]
pub struct BackgroundReviewConfig {
    /// Whether the background review loop is enabled.
    pub enabled: bool,
    /// Minimum number of turns in a conversation before a review is run.
    pub min_turns: usize,
    /// Soft cap on tokens spent on review (advisory; not enforced here).
    pub max_review_tokens: usize,
}

impl Default for BackgroundReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_turns: 3,
            max_review_tokens: 500,
        }
    }
}

/// The kind of learning signal detected in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewSignal {
    /// The user corrected the agent's style, workflow, or approach.
    UserCorrection,
    /// A non-trivial technique, workaround, or tool-usage pattern emerged.
    NonTrivialTechnique,
    /// No existing skill matched the conversation's intent.
    SkillGap,
    /// A skill that was consulted turned out wrong, missing, or outdated.
    SkillOutdated,
}

/// A single review result — one learning signal and its suggested action.
#[derive(Debug, Clone)]
pub struct ReviewResult {
    pub signal: ReviewSignal,
    pub skill_id: Option<String>,
    pub skill_name: String,
    pub suggested_instructions: String,
    pub confidence_delta: f64,
}

/// The background reviewer — holds a SkillEngine reference and config.
pub struct BackgroundReviewer<'a> {
    engine: &'a mut SkillEngine,
    config: BackgroundReviewConfig,
}

impl<'a> BackgroundReviewer<'a> {
    /// Create a new reviewer backed by the given SkillEngine.
    pub fn new(engine: &'a mut SkillEngine) -> Self {
        Self {
            engine,
            config: BackgroundReviewConfig::default(),
        }
    }

    /// Create a new reviewer with explicit config.
    pub fn with_config(engine: &'a mut SkillEngine, config: BackgroundReviewConfig) -> Self {
        Self { engine, config }
    }

    /// Returns a reference to the reviewer's config.
    pub fn config(&self) -> &BackgroundReviewConfig {
        &self.config
    }

    /// Analyze a conversation for learning signals.
    ///
    /// Detects:
    /// - User corrections: user messages containing "no", "wrong", "actually",
    ///   "instead", "should have", "not like that" → UserCorrection.
    /// - Non-trivial techniques: assistant turns with 3+ tool calls that
    ///   succeeded (outcome is Success) → NonTrivialTechnique.
    /// - Skill gaps: if no existing skill matched the conversation intent
    ///   (no trigger patterns overlap the first user message) → SkillGap.
    ///
    /// For each signal, either suggest updating an existing skill (if trigger
    /// patterns match) or creating a new one.
    pub fn review_conversation(
        &mut self,
        conversation: &[ConversationTurn],
        outcome: SkillOutcome,
    ) -> Result<Vec<ReviewResult>, SkillError> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }
        if conversation.len() < self.config.min_turns {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();

        // Detect user corrections.
        for turn in conversation {
            if !turn.role.eq_ignore_ascii_case("user") {
                continue;
            }
            if !is_user_correction(&turn.content) {
                continue;
            }
            let matched = self.match_skill_for_turn(turn);
            let (skill_id, skill_name, instructions, delta) = match matched {
                Some(skill) => (
                    Some(skill.id.clone()),
                    skill.name.clone(),
                    format!(
                        "User correction detected: {}. Update approach to avoid repeating the \
                         undesired behavior.",
                        truncate_content(&turn.content, 200)
                    ),
                    -0.1,
                ),
                None => (
                    None,
                    derive_skill_name(&turn.content),
                    format!(
                        "User correction detected: {}. Capture the preferred approach as a new \
                         skill.",
                        truncate_content(&turn.content, 200)
                    ),
                    0.0,
                ),
            };
            results.push(ReviewResult {
                signal: ReviewSignal::UserCorrection,
                skill_id,
                skill_name,
                suggested_instructions: instructions,
                confidence_delta: delta,
            });
        }

        // Detect non-trivial techniques: assistant turns with 3+ tool calls
        // that succeeded.
        if outcome == SkillOutcome::Success {
            for turn in conversation {
                if !turn.role.eq_ignore_ascii_case("assistant") {
                    continue;
                }
                if turn.tool_calls.len() < 3 {
                    continue;
                }
                let matched = self.match_skill_for_conversation(conversation);
                let (skill_id, skill_name, instructions, delta) = match matched {
                    Some(skill) => (
                        Some(skill.id.clone()),
                        skill.name.clone(),
                        format!(
                            "Non-trivial technique: tool sequence [{}]. Reinforce existing \
                             skill.",
                            turn.tool_calls.join(" -> ")
                        ),
                        0.1,
                    ),
                    None => (
                        None,
                        derive_skill_name_from_intent(conversation),
                        format!(
                            "Non-trivial technique: tool sequence [{}]. Capture as a new skill.",
                            turn.tool_calls.join(" -> ")
                        ),
                        0.0,
                    ),
                };
                results.push(ReviewResult {
                    signal: ReviewSignal::NonTrivialTechnique,
                    skill_id,
                    skill_name,
                    suggested_instructions: instructions,
                    confidence_delta: delta,
                });
            }
        }

        // Detect skill gaps: if no existing skill matched the conversation intent.
        if self.match_skill_for_conversation(conversation).is_none() {
            let first_user = conversation
                .iter()
                .find(|t| t.role.eq_ignore_ascii_case("user"))
                .map(|t| t.content.as_str())
                .unwrap_or("");
            if !first_user.is_empty() {
                results.push(ReviewResult {
                    signal: ReviewSignal::SkillGap,
                    skill_id: None,
                    skill_name: derive_skill_name(first_user),
                    suggested_instructions: format!(
                        "No existing skill matched the intent: {}. Create a new skill from this \
                         conversation.",
                        truncate_content(first_user, 200)
                    ),
                    confidence_delta: 0.0,
                });
            }
        }

        Ok(results)
    }

    /// Apply review results to the skill engine.
    ///
    /// - UserCorrection: update_confidence on the matched skill (false = failure).
    /// - NonTrivialTechnique: create_from_pattern if no match, or
    ///   update_confidence (true).
    /// - SkillGap: create_from_conversation.
    /// - SkillOutdated: update instructions on the existing skill.
    pub fn apply_review(&mut self, reviews: &[ReviewResult]) -> Result<(), SkillError> {
        for review in reviews {
            match review.signal {
                ReviewSignal::UserCorrection => {
                    if let Some(id) = &review.skill_id {
                        self.engine.update_confidence(id, false)?;
                    }
                }
                ReviewSignal::NonTrivialTechnique => {
                    if let Some(id) = &review.skill_id {
                        self.engine.update_confidence(id, true)?;
                    } else {
                        self.engine.create_from_pattern(
                            &review.skill_name,
                            &review.suggested_instructions,
                            &review.suggested_instructions,
                            vec![review.skill_name.to_lowercase()],
                        )?;
                    }
                }
                ReviewSignal::SkillGap => {
                    // create_from_conversation needs the original conversation;
                    // we don't have it here, so create_from_pattern as a fallback.
                    self.engine.create_from_pattern(
                        &review.skill_name,
                        &review.suggested_instructions,
                        &review.suggested_instructions,
                        vec![review.skill_name.to_lowercase()],
                    )?;
                }
                ReviewSignal::SkillOutdated => {
                    if let Some(id) = &review.skill_id {
                        self.engine
                            .update_instructions(id, &review.suggested_instructions)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Find an existing skill whose trigger patterns match a single turn's content.
    fn match_skill_for_turn(&self, turn: &ConversationTurn) -> Option<crate::skill_engine::Skill> {
        let content = turn.content.to_lowercase();
        self.engine
            .list()
            .into_iter()
            .find(|skill| {
                skill
                    .trigger_patterns
                    .iter()
                    .any(|p| content.contains(&p.to_lowercase()))
            })
            .cloned()
    }

    /// Find an existing skill whose trigger patterns match the conversation intent.
    fn match_skill_for_conversation(
        &self,
        conversation: &[ConversationTurn],
    ) -> Option<crate::skill_engine::Skill> {
        let first_user = conversation
            .iter()
            .find(|t| t.role.eq_ignore_ascii_case("user"))?;
        self.match_skill_for_turn(first_user)
    }
}

/// Check whether a user message is a correction.
fn is_user_correction(content: &str) -> bool {
    let lower = content.to_lowercase();
    const MARKERS: &[&str] = &[
        "no,",
        "no ",
        "wrong",
        "actually",
        "instead",
        "should have",
        "not like that",
        "don't",
        "do not",
        "stop doing",
        "i didn't want",
        "that's not",
    ];
    MARKERS.iter().any(|m| lower.contains(m))
}

/// Derive a short skill name from a content string.
fn derive_skill_name(content: &str) -> String {
    let words: Vec<&str> = content.split_whitespace().take(4).collect();
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

/// Derive a skill name from the first user message in a conversation.
fn derive_skill_name_from_intent(conversation: &[ConversationTurn]) -> String {
    let first_user = conversation
        .iter()
        .find(|t| t.role.eq_ignore_ascii_case("user"))
        .map(|t| t.content.as_str())
        .unwrap_or("");
    derive_skill_name(first_user)
}

/// Truncate a string to at most `max` chars, appending an ellipsis if cut.
fn truncate_content(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_engine() -> SkillEngine {
        SkillEngine::new(PathBuf::from("/tmp/rx4-bg-review-test"))
    }

    fn conversation_with_correction() -> Vec<ConversationTurn> {
        vec![
            ConversationTurn {
                role: "user".into(),
                content: "Refactor the parser module".into(),
                tool_calls: vec![],
            },
            ConversationTurn {
                role: "assistant".into(),
                content: "I'll rewrite everything from scratch.".into(),
                tool_calls: vec!["read".into()],
            },
            ConversationTurn {
                role: "user".into(),
                content: "No, wrong approach. Instead, extract traits first.".into(),
                tool_calls: vec![],
            },
            ConversationTurn {
                role: "assistant".into(),
                content: "Understood, extracting traits.".into(),
                tool_calls: vec!["edit".into()],
            },
        ]
    }

    fn conversation_with_technique() -> Vec<ConversationTurn> {
        vec![
            ConversationTurn {
                role: "user".into(),
                content: "Find and fix the failing test in parser".into(),
                tool_calls: vec![],
            },
            ConversationTurn {
                role: "assistant".into(),
                content: "Searching.".into(),
                tool_calls: vec!["grep".into(), "read".into(), "edit".into()],
            },
            ConversationTurn {
                role: "assistant".into(),
                content: "Fixed.".into(),
                tool_calls: vec![],
            },
        ]
    }

    #[test]
    fn test_review_detects_user_correction() {
        let mut engine = test_engine();
        let mut reviewer = BackgroundReviewer::with_config(
            &mut engine,
            BackgroundReviewConfig {
                enabled: true,
                min_turns: 3,
                max_review_tokens: 500,
            },
        );
        let conv = conversation_with_correction();
        let results = reviewer
            .review_conversation(&conv, SkillOutcome::Failure)
            .expect("review");
        assert!(results
            .iter()
            .any(|r| r.signal == ReviewSignal::UserCorrection));
    }

    #[test]
    fn test_review_detects_technique() {
        let mut engine = test_engine();
        let mut reviewer = BackgroundReviewer::with_config(
            &mut engine,
            BackgroundReviewConfig {
                enabled: true,
                min_turns: 3,
                max_review_tokens: 500,
            },
        );
        let conv = conversation_with_technique();
        let results = reviewer
            .review_conversation(&conv, SkillOutcome::Success)
            .expect("review");
        assert!(results
            .iter()
            .any(|r| r.signal == ReviewSignal::NonTrivialTechnique));
    }

    #[test]
    fn test_apply_review_updates_confidence() {
        let mut engine = test_engine();
        let skill = engine
            .create_from_pattern(
                "fix_test",
                "fix failing tests",
                "run cargo test",
                vec!["failing".into(), "test".into()],
            )
            .expect("create");
        let skill_id = skill.id.clone();
        let initial_confidence = skill.confidence;

        let reviews = vec![ReviewResult {
            signal: ReviewSignal::UserCorrection,
            skill_id: Some(skill_id.clone()),
            skill_name: "fix_test".into(),
            suggested_instructions: "Update approach.".into(),
            confidence_delta: -0.1,
        }];

        let mut reviewer = BackgroundReviewer::with_config(
            &mut engine,
            BackgroundReviewConfig {
                enabled: true,
                min_turns: 1,
                max_review_tokens: 500,
            },
        );
        reviewer.apply_review(&reviews).expect("apply");

        let updated = reviewer.engine.get(&skill_id).expect("exists");
        assert_eq!(updated.failure_count, 1);
        assert!(updated.confidence < initial_confidence);
    }

    #[test]
    fn test_review_disabled_returns_empty() {
        let mut engine = test_engine();
        let mut reviewer = BackgroundReviewer::new(&mut engine);
        let conv = conversation_with_correction();
        let results = reviewer
            .review_conversation(&conv, SkillOutcome::Failure)
            .expect("review");
        assert!(results.is_empty());
    }

    #[test]
    fn test_review_below_min_turns_returns_empty() {
        let mut engine = test_engine();
        let mut reviewer = BackgroundReviewer::with_config(
            &mut engine,
            BackgroundReviewConfig {
                enabled: true,
                min_turns: 10,
                max_review_tokens: 500,
            },
        );
        let conv = conversation_with_correction();
        let results = reviewer
            .review_conversation(&conv, SkillOutcome::Failure)
            .expect("review");
        assert!(results.is_empty());
    }

    #[test]
    fn test_review_detects_skill_gap() {
        let mut engine = test_engine();
        let mut reviewer = BackgroundReviewer::with_config(
            &mut engine,
            BackgroundReviewConfig {
                enabled: true,
                min_turns: 3,
                max_review_tokens: 500,
            },
        );
        let conv = conversation_with_technique();
        let results = reviewer
            .review_conversation(&conv, SkillOutcome::Partial)
            .expect("review");
        assert!(results.iter().any(|r| r.signal == ReviewSignal::SkillGap));
    }

    #[test]
    fn test_apply_review_creates_skill_for_gap() {
        let mut engine = test_engine();
        let reviews = vec![ReviewResult {
            signal: ReviewSignal::SkillGap,
            skill_id: None,
            skill_name: "new_skill".into(),
            suggested_instructions: "Do the thing.".into(),
            confidence_delta: 0.0,
        }];
        let mut reviewer = BackgroundReviewer::with_config(
            &mut engine,
            BackgroundReviewConfig {
                enabled: true,
                min_turns: 1,
                max_review_tokens: 500,
            },
        );
        reviewer.apply_review(&reviews).expect("apply");
        assert_eq!(reviewer.engine.list().len(), 1);
    }

    #[test]
    fn test_apply_review_updates_instructions_for_outdated() {
        let mut engine = test_engine();
        let skill = engine
            .create_from_pattern(
                "deploy",
                "deploy the app",
                "1. Build\n2. Ship",
                vec!["deploy".into()],
            )
            .expect("create");
        let skill_id = skill.id.clone();
        let reviews = vec![ReviewResult {
            signal: ReviewSignal::SkillOutdated,
            skill_id: Some(skill_id.clone()),
            skill_name: "deploy".into(),
            suggested_instructions: "1. Build\n2. Test\n3. Ship".into(),
            confidence_delta: 0.0,
        }];
        let mut reviewer = BackgroundReviewer::with_config(
            &mut engine,
            BackgroundReviewConfig {
                enabled: true,
                min_turns: 1,
                max_review_tokens: 500,
            },
        );
        reviewer.apply_review(&reviews).expect("apply");
        let updated = reviewer.engine.get(&skill_id).expect("exists");
        assert!(updated.instructions.contains("Test"));
    }
}
