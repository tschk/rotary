use std::sync::Arc;

use parking_lot::Mutex;
pub use zkr::{
    ConversationEvent, Error as PersonalityError, MemoryDb, MindHypothesis, ObservationFinding,
    ObservationSeverity, PersonId, PersonaBlueprint, Personality as ZkrPersonality, SocialSignal,
    TenantId, TurnAction, TurnDecision, VoiceCard,
};

/// Async wrapper around `zkr::Personality` for use inside the `rx4` agent loop.
///
/// All personality memory is written to `zkr` evidence memory with
/// `feature_flag: "personality"` in a blocking thread, and relevant behavioral
/// context is retrieved to augment the system prompt before each run.
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

    /// Record a raw conversation event.
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
