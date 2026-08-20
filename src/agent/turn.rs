use super::Agent;
#[cfg(feature = "skills")]
use super::Event;
#[cfg(feature = "personality")]
use tracing::{debug, info, warn};

pub(super) fn append_active_skills(
    base: Option<String>,
    active_skills: Option<&str>,
) -> Option<String> {
    let Some(active_skills) = active_skills else {
        return base;
    };
    Some(match base {
        Some(base) => format!("{base}\n\n# Active Skills\n\n{active_skills}"),
        None => format!("# Active Skills\n\n{active_skills}"),
    })
}

impl Agent {
    pub(super) fn activate_skills_for_prompt(&mut self, safe_text: &str) -> Option<String> {
        #[cfg(feature = "skills")]
        {
            if let Some(reg) = &self.skill_registry {
                let matched = reg.match_prompt(safe_text);
                for skill in &matched {
                    self.emit(Event::SkillActivated {
                        id: skill.id.clone(),
                        name: skill.name.clone(),
                    });
                }
                let activated: Vec<String> = matched
                    .into_iter()
                    .map(|skill| skill.instructions.clone())
                    .collect();
                return (!activated.is_empty()).then(|| activated.join("\n\n---\n\n"));
            }
        }
        #[cfg(not(feature = "skills"))]
        let _ = safe_text;
        None
    }

    pub(super) async fn before_prompt_hooks(&self, safe_text: &str) {
        #[cfg(feature = "personality")]
        if let Some(pers) = &self.personality {
            let event = crate::personality::ConversationEvent {
                epoch: 0,
                participant: "user".to_string(),
                event_kind: "message".to_string(),
                content: safe_text.chars().take(500).collect(),
            };
            match pers.route_event(&event).await {
                Ok(result) => {
                    debug!(
                        "personality router: {:?} via {} (confidence {}bps) — {}",
                        result.decision.action,
                        result.decision.strategy,
                        result.decision.confidence_basis_points,
                        result.decision.rationale
                    );
                }
                Err(error) => {
                    warn!("personality routing failed: {error}");
                }
            }
        }
        #[cfg(not(feature = "personality"))]
        let _ = safe_text;
    }

    pub(super) async fn after_prompt_hooks(&mut self) {
        #[cfg(feature = "personality")]
        if let Some(pers) = &self.personality {
            let scope = format!(
                "prompt-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            match pers.analyze_conversation(&scope).await {
                Ok(health) => {
                    if !health.findings.is_empty() {
                        info!(
                            "personality observability: {} findings for {} (balance={:.2}, error_rate={:.2})",
                            health.findings.len(),
                            health.scope,
                            health.participation_balance,
                            health.error_rate
                        );
                    }
                }
                Err(error) => {
                    warn!("personality observability analysis failed: {error}");
                }
            }
        }

        #[cfg(feature = "graph-memory")]
        if let Some(graph) = self.graph_memory.as_mut() {
            let turns: Vec<crate::graph_memory::ConversationTurn> = self
                .messages
                .read()
                .iter()
                .map(|m| crate::graph_memory::ConversationTurn {
                    role: m.role.to_string(),
                    content: m.content.clone(),
                })
                .collect();
            let extracted = crate::graph_memory::ConversationExtractor::new().extract(&turns);
            for node in extracted.nodes {
                graph.add_node(node);
            }
            for edge in extracted.edges {
                let _ = graph.add_edge(edge);
            }
            if self.auto_dream {
                let _ = crate::dream_scheduler::DreamScheduler::new().run_cycle(graph);
            }
        }

        #[cfg(feature = "skills")]
        if let Some(engine) = self.skill_engine.as_mut() {
            let turns: Vec<crate::skill_engine::ConversationTurn> = self
                .messages
                .read()
                .iter()
                .map(|m| crate::skill_engine::ConversationTurn {
                    role: m.role.to_string(),
                    content: m.content.clone(),
                    tool_calls: Vec::new(),
                })
                .collect();
            let mut reviewer = crate::background_review::BackgroundReviewer::new(engine);
            if let Ok(reviews) =
                reviewer.review_conversation(&turns, crate::skill_engine::SkillOutcome::Success)
            {
                let _ = reviewer.apply_review(&reviews);
            }
        }
    }
}
