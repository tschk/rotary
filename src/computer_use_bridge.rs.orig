use ed25519_dalek::{Signer, SigningKey};
use praefectus::{
    canonical_authority_bytes, default_ledger_path, normalized_action_hash, Action, ActionRequest,
    AuthorityGrant, CancellationToken, Ed25519AuthorityVerifier, Engine, InteractionMode,
    NativeExecutor, SafetyClass, SignedAuthority, TargetRef, Terminal, VerificationPolicy,
    PROTOCOL_VERSION,
};
use rand_core::OsRng;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct ComputerUseBridge {
    engine: Engine<NativeExecutor>,
    observer: NativeExecutor,
    signer: SigningKey,
    observation: parking_lot::Mutex<Option<praefectus::semantic::SemanticObservation>>,
}

impl ComputerUseBridge {
    pub fn new() -> Result<Self, String> {
        let signer = SigningKey::generate(&mut OsRng);
        let verifier = Ed25519AuthorityVerifier::new([(
            "rx4".to_string(),
            "computer-use".to_string(),
            "1".to_string(),
            signer.verifying_key(),
        )])
        .map_err(|error| error.to_string())?;
        Ok(Self {
            engine: Engine::new(NativeExecutor::default(), default_ledger_path(), verifier),
            observer: NativeExecutor::default(),
            signer,
            observation: parking_lot::Mutex::new(None),
        })
    }

    pub fn observer(&self) -> &NativeExecutor {
        &self.observer
    }

    pub fn set_observation(&self, observation: praefectus::semantic::SemanticObservation) {
        *self.observation.lock() = Some(observation);
    }

    pub fn observation(&self) -> Option<praefectus::semantic::SemanticObservation> {
        self.observation.lock().clone()
    }

    pub fn execute(
        &self,
        action: Action,
        target: TargetRef,
        verification: VerificationPolicy,
        safety: SafetyClass,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        let deadline_at_ms = now_ms().saturating_add(30_000);
        let operation_id = uuid::Uuid::new_v4().simple().to_string();
        let mut request = ActionRequest {
            protocol_version: PROTOCOL_VERSION,
            action_version: 1,
            target_version: 1,
            verification_version: 1,
            operation_id: operation_id.clone(),
            subject: "rx4-host".to_string(),
            session_id: "rx4-computer-use".to_string(),
            authority: SignedAuthority {
                grant: AuthorityGrant {
                    protocol_version: PROTOCOL_VERSION,
                    issuer: "rx4".to_string(),
                    key_id: "computer-use".to_string(),
                    operation_id,
                    subject: "rx4-host".to_string(),
                    session_id: "rx4-computer-use".to_string(),
                    risk: safety,
                    expires_at_ms: deadline_at_ms,
                    policy_generation: "1".to_string(),
                    action_hash: "0".repeat(64),
                },
                signature: "0".repeat(128),
            },
            action,
            target,
            interaction_mode: InteractionMode::Interactive,
            deadline_at_ms,
            verification,
            safety,
        };
        request.authority.grant.action_hash =
            normalized_action_hash(&request).map_err(|error| error.to_string())?;
        request.authority.signature = hex::encode(
            self.signer
                .sign(
                    &canonical_authority_bytes(&request.authority.grant)
                        .map_err(|error| error.to_string())?,
                )
                .to_bytes(),
        );
        let report = self
            .engine
            .execute(&request, cancellation)
            .map_err(|error| error.to_string())?;
        let terminal = report
            .acknowledgements
            .last()
            .and_then(|acknowledgement| match &acknowledgement.state {
                praefectus::AckState::Terminal { terminal } => Some(terminal.as_ref()),
                _ => None,
            })
            .ok_or_else(|| "computer-use action did not reach a terminal state".to_string())?;
        match terminal {
            Terminal::Succeeded { .. } => {
                serde_json::to_value(terminal).map_err(|error| error.to_string())
            }
            _ => Err(serde_json::to_string(terminal).unwrap_or_else(|_| {
                "computer-use action failed without a serializable result".to_string()
            })),
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}
