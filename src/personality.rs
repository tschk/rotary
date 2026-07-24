use std::sync::Arc;

use parking_lot::Mutex;
pub use zkr::{
    CalibrationRecord, ConversationEvent, ConversationHealth, Error as PersonalityError, MemoryDb,
    MindHypothesis, ObservationFinding, ObservationSeverity, PersonId, PersonaBlueprint,
    PersonaValidation, Personality as ZkrPersonality, RiskAssessment, RiskRecommendation,
    RouterResult, RouterRules, SignalSummary, SocialSignal, TenantId, TurnAction, TurnDecision,
    VoiceCard,
};

/// Async wrapper around `zkr::Personality` for use inside the `rx4` agent loop.
///
/// All personality memory is written to `zkr` evidence memory with
/// `feature_flag: "personality"` in a blocking thread, and relevant behavioral
/// context is retrieved to augment the system prompt before each run.
///
/// The router evaluates every incoming event through hard rules + learned
/// policy and returns a speak/silent/react/continue decision with behavioral
/// context. Signals are derived automatically from the event stream.
/// Theory-of-mind risk assessment evaluates candidate replies. Observability
/// analysis runs over conversation windows to produce health summaries.
#[derive(Clone)]
pub struct Personality {
    inner: Arc<Mutex<ZkrPersonality>>,
}

impl Personality {
    pub fn new(db: MemoryDb, tenant_id: TenantId, person_id: PersonId) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ZkrPersonality::new(db, tenant_id, person_id))),
        }
    }

    pub fn with_rules(self, rules: RouterRules) -> Self {
        {
            let mut guard = self.inner.lock();
            guard.set_rules(rules);
        }
        self
    }

    /// Route an incoming event through the turn router.
    ///
    /// Evaluates hard rules (mentions, commands, rate limits, consecutive
    /// turns) then a learned policy, returns a decision with behavioral
    /// context, and records the decision + derives signals automatically.
    pub async fn route_event(
        &self,
        event: &ConversationEvent,
    ) -> Result<RouterResult, PersonalityError> {
        let inner = Arc::clone(&self.inner);
        let event = event.clone();
        tokio::task::spawn_blocking(move || inner.lock().route_event(&event))
            .await
            .map_err(|e| PersonalityError::Invalid(e.to_string()))?
    }

    /// Record a turn-taking decision.
    pub async fn record_turn(&self, decision: &TurnDecision) -> Result<(), PersonalityError> {
        let inner = Arc::clone(&self.inner);
        let decision = decision.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock();
            guard.record_turn_decision(&decision).map(|_| ())
        })
        .await
        .map_err(|e| PersonalityError::Invalid(e.to_string()))?
    }

    /// Record a raw conversation event. Signals are derived automatically.
    pub async fn record_event(&self, event: &ConversationEvent) -> Result<(), PersonalityError> {
        let inner = Arc::clone(&self.inner);
        let event = event.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock();
            guard.record_event(&event).map(|_| ())
        })
        .await
        .map_err(|e| PersonalityError::Invalid(e.to_string()))?
    }

    /// Record a derived social signal.
    pub async fn record_signal(&self, signal: &SocialSignal) -> Result<(), PersonalityError> {
        let inner = Arc::clone(&self.inner);
        let signal = signal.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock();
            guard.record_signal(&signal).map(|_| ())
        })
        .await
        .map_err(|e| PersonalityError::Invalid(e.to_string()))?
    }

    /// Compute a signal summary for a participant.
    pub async fn signal_summary(&self, participant: &str) -> SignalSummary {
        let inner = Arc::clone(&self.inner);
        let participant = participant.to_string();
        let participant_clone = participant.clone();
        tokio::task::spawn_blocking(move || inner.lock().signal_summary(&participant))
            .await
            .unwrap_or_else(|_| SignalSummary {
                participant: participant_clone,
                ..Default::default()
            })
    }

    /// Store or update a voice card.
    pub async fn store_voice_card(&self, card: &VoiceCard) -> Result<(), PersonalityError> {
        let inner = Arc::clone(&self.inner);
        let card = card.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock();
            guard.store_voice_card(&card).map(|_| ())
        })
        .await
        .map_err(|e| PersonalityError::Invalid(e.to_string()))?
    }

    /// Record a theory-of-mind hypothesis.
    pub async fn record_hypothesis(&self, hyp: &MindHypothesis) -> Result<(), PersonalityError> {
        let inner = Arc::clone(&self.inner);
        let hyp = hyp.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock();
            guard.record_hypothesis(&hyp).map(|_| ())
        })
        .await
        .map_err(|e| PersonalityError::Invalid(e.to_string()))?
    }

    /// Assess the risk of a candidate reply toward a participant.
    pub async fn assess_risk(&self, participant: &str, candidate: &str) -> RiskAssessment {
        let inner = Arc::clone(&self.inner);
        let participant = participant.to_string();
        let candidate = candidate.to_string();
        tokio::task::spawn_blocking(move || inner.lock().assess_risk(&participant, &candidate))
            .await
            .unwrap_or_else(|_| RiskAssessment::default())
    }

    /// Record a calibration result comparing a prediction to the real outcome.
    pub async fn record_calibration(
        &self,
        record: &CalibrationRecord,
    ) -> Result<(), PersonalityError> {
        let inner = Arc::clone(&self.inner);
        let record = record.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock();
            guard.record_calibration(&record).map(|_| ())
        })
        .await
        .map_err(|e| PersonalityError::Invalid(e.to_string()))?
    }

    /// Validate a persona blueprint.
    pub fn validate_persona(persona: &PersonaBlueprint) -> PersonaValidation {
        ZkrPersonality::validate_persona(persona)
    }

    /// Store a persona blueprint.
    pub async fn store_persona(&self, persona: &PersonaBlueprint) -> Result<(), PersonalityError> {
        let inner = Arc::clone(&self.inner);
        let persona = persona.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock();
            guard.store_persona(&persona).map(|_| ())
        })
        .await
        .map_err(|e| PersonalityError::Invalid(e.to_string()))?
    }

    /// Analyze a conversation window and produce a health summary.
    ///
    /// Runs observability analysis over the recent event buffer, computing
    /// participation balance, error rate, and generating evidence-cited
    /// findings with recommendations.
    pub async fn analyze_conversation(
        &self,
        scope: &str,
    ) -> Result<ConversationHealth, PersonalityError> {
        let inner = Arc::clone(&self.inner);
        let scope = scope.to_string();
        tokio::task::spawn_blocking(move || inner.lock().analyze_conversation(&scope))
            .await
            .map_err(|e| PersonalityError::Invalid(e.to_string()))?
    }

    /// Record an observability finding.
    pub async fn record_finding(
        &self,
        finding: &ObservationFinding,
    ) -> Result<(), PersonalityError> {
        let inner = Arc::clone(&self.inner);
        let finding = finding.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock();
            guard.record_finding(&finding).map(|_| ())
        })
        .await
        .map_err(|e| PersonalityError::Invalid(e.to_string()))?
    }

    /// Retrieve personality context and augment the base system prompt.
    pub async fn augment(&self, query: &str, base: &str) -> Result<String, PersonalityError> {
        let inner = Arc::clone(&self.inner);
        let query = query.to_string();
        let base = base.to_string();
        tokio::task::spawn_blocking(move || inner.lock().augment_prompt(&query, &base))
            .await
            .map_err(|e| PersonalityError::Invalid(e.to_string()))?
    }

    /// Retrieve personality context excerpts relevant to `query`.
    pub async fn context(&self, query: &str, limit: u32) -> Result<Vec<String>, PersonalityError> {
        let inner = Arc::clone(&self.inner);
        let query = query.to_string();
        tokio::task::spawn_blocking(move || {
            let guard = inner.lock();
            guard.turn_context(&query, limit)
        })
        .await
        .map_err(|e| PersonalityError::Invalid(e.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ids() -> (TenantId, PersonId) {
        (TenantId("t1".into()), PersonId("p1".into()))
    }

    #[tokio::test]
    async fn async_router_replies_to_mention() {
        let tmp = tempfile::tempdir().unwrap();
        let db = MemoryDb::open(tmp.path().join("personality.db")).unwrap();
        let (tenant_id, person_id) = test_ids();
        let personality = Personality::new(db, tenant_id, person_id);

        let result = personality
            .route_event(&ConversationEvent {
                epoch: 1,
                participant: "user".into(),
                event_kind: "message".into(),
                content: "hey @agent help".into(),
            })
            .await
            .unwrap();

        assert_eq!(result.decision.action, TurnAction::Speak);
        assert_eq!(result.decision.strategy, "direct_reply");
    }

    #[tokio::test]
    async fn async_router_stays_silent_for_unaddressed() {
        let tmp = tempfile::tempdir().unwrap();
        let db = MemoryDb::open(tmp.path().join("personality.db")).unwrap();
        let (tenant_id, person_id) = test_ids();
        let personality = Personality::new(db, tenant_id, person_id);

        let result = personality
            .route_event(&ConversationEvent {
                epoch: 1,
                participant: "user".into(),
                event_kind: "message".into(),
                content: "just chatting".into(),
            })
            .await
            .unwrap();

        assert_eq!(result.decision.action, TurnAction::StaySilent);
    }

    #[tokio::test]
    async fn async_signal_summary_works() {
        let tmp = tempfile::tempdir().unwrap();
        let db = MemoryDb::open(tmp.path().join("personality.db")).unwrap();
        let (tenant_id, person_id) = test_ids();
        let personality = Personality::new(db, tenant_id, person_id);

        for i in 1..=3 {
            personality
                .record_event(&ConversationEvent {
                    epoch: i,
                    participant: "user".into(),
                    event_kind: "message".into(),
                    content: format!("msg {i}"),
                })
                .await
                .unwrap();
        }

        let summary = personality.signal_summary("user").await;
        assert_eq!(summary.message_count, 3);
    }

    #[tokio::test]
    async fn async_risk_assessment_works() {
        let tmp = tempfile::tempdir().unwrap();
        let db = MemoryDb::open(tmp.path().join("personality.db")).unwrap();
        let (tenant_id, person_id) = test_ids();
        let personality = Personality::new(db, tenant_id, person_id);

        for i in 1..=5 {
            personality
                .record_event(&ConversationEvent {
                    epoch: i,
                    participant: "user".into(),
                    event_kind: "message".into(),
                    content: format!("msg {i}"),
                })
                .await
                .unwrap();
        }

        let risk = personality
            .assess_risk("user", "Here is a helpful answer.")
            .await;
        assert_eq!(risk.recommendation, RiskRecommendation::Proceed);
    }

    #[tokio::test]
    async fn async_analyze_conversation_works() {
        let tmp = tempfile::tempdir().unwrap();
        let db = MemoryDb::open(tmp.path().join("personality.db")).unwrap();
        let (tenant_id, person_id) = test_ids();
        let personality = Personality::new(db, tenant_id, person_id);

        for i in 1..=10 {
            personality
                .record_event(&ConversationEvent {
                    epoch: i,
                    participant: if i % 2 == 0 { "user" } else { "agent" }.to_string(),
                    event_kind: "message".into(),
                    content: format!("msg {i}"),
                })
                .await
                .unwrap();
        }

        let health = personality.analyze_conversation("thread-1").await.unwrap();
        assert_eq!(health.total_turns, 10);
        assert!(health.participation_balance > 0.8);
    }

    #[tokio::test]
    async fn async_persona_validation_works() {
        let persona = PersonaBlueprint {
            name: "test".into(),
            traits: vec!["patient".into()],
            system_prompt: "You are test.".into(),
            constraints: vec![],
            citations: vec![],
        };
        let validation = Personality::validate_persona(&persona);
        assert!(validation.valid);
    }

    #[tokio::test]
    async fn async_augment_works() {
        let tmp = tempfile::tempdir().unwrap();
        let db = MemoryDb::open(tmp.path().join("personality.db")).unwrap();
        let (tenant_id, person_id) = test_ids();
        let personality = Personality::new(db, tenant_id, person_id);

        let augmented = personality
            .augment("nothing here", "Base prompt.")
            .await
            .unwrap();
        assert_eq!(augmented, "Base prompt.");
    }

    #[tokio::test]
    async fn async_concurrent_access() {
        let tmp = tempfile::tempdir().unwrap();
        let db = MemoryDb::open(tmp.path().join("personality.db")).unwrap();
        let (tenant_id, person_id) = test_ids();
        let personality = Personality::new(db, tenant_id, person_id);

        let p1 = personality.clone();
        let p2 = personality.clone();

        let h1 = tokio::spawn(async move {
            p1.record_event(&ConversationEvent {
                epoch: 1,
                participant: "user".into(),
                event_kind: "message".into(),
                content: "from task 1".into(),
            })
            .await
        });

        let h2 = tokio::spawn(async move {
            p2.record_event(&ConversationEvent {
                epoch: 2,
                participant: "user".into(),
                event_kind: "message".into(),
                content: "from task 2".into(),
            })
            .await
        });

        h1.await.unwrap().unwrap();
        h2.await.unwrap().unwrap();
    }
}
