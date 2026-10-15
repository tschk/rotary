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
    /// Foreground-bound delivery is host opt-in. Background semantic
    /// interaction is the default; moving the pointer or seizing focus
    /// requires the same explicit host flag Praefectus itself requires for
    /// global input, so agents cannot fall back to cursor control silently.
    pub fn foreground_input_allowed() -> bool {
        std::env::var("PRAEFECTUS_ALLOW_GLOBAL_INPUT").is_ok_and(|value| value == "1")
    }

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
        let interaction_mode = interaction_mode(&action, &target);
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
            interaction_mode,
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

/// Refuses foreground-bound delivery unless the host opted in. Background
/// semantic interaction stays available; this gate only covers the routes
/// that move the pointer or seize focus.
pub(crate) fn require_background_route(
    foreground_allowed: bool,
    action: &str,
) -> Result<(), String> {
    if foreground_allowed {
        return Ok(());
    }
    Err(format!(
        "{action} is foreground-bound and disabled by default; observe with cu_see and act on semantic element tags (background-safe), or ask the host to set PRAEFECTUS_ALLOW_GLOBAL_INPUT=1"
    ))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// Routes request `background_only` whenever the delivery route can keep the
/// desktop untouched, and `interactive` for pointer- and foreground-bound
/// actions. Target-addressed semantic actions never move the cursor, and on
/// macOS input aimed at a fenced element is delivered to that element's
/// process, so those routes are background-capable; other platforms have no
/// per-process delivery route, so their input synthesis stays interactive.
/// The executor owns the final decision: Praefectus refuses a background
/// request its runtime route cannot honor, before any effect.
fn interaction_mode(action: &Action, target: &TargetRef) -> InteractionMode {
    let element_target = matches!(target, TargetRef::Element { .. });
    match action {
        Action::Invoke
        | Action::SetValue { .. }
        | Action::SelectText { .. }
        | Action::PerformSecondaryAction { .. } => InteractionMode::BackgroundOnly,
        Action::TypeText { .. }
        | Action::Press { .. }
        | Action::Paste { .. }
        | Action::Hotkey { .. }
        | Action::Scroll { .. }
            if element_target && cfg!(target_os = "macos") =>
        {
            InteractionMode::BackgroundOnly
        }
        _ => InteractionMode::Interactive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use praefectus::semantic::SemanticTargetRef;
    use praefectus::{MouseButton, WindowOperation};

    fn element_target() -> TargetRef {
        TargetRef::Element {
            target: SemanticTargetRef {
                observation_id: "1".repeat(64),
                generation: 1,
                provenance_hash: "2".repeat(64),
                element_id: "3".repeat(64),
                fingerprint_hash: "4".repeat(64),
            },
        }
    }

    #[test]
    fn foreground_bound_routes_are_refused_without_host_opt_in() {
        let refusal = require_background_route(false, "coordinate clicks")
            .expect_err("foreground route must refuse without opt-in");
        assert!(refusal.contains("PRAEFECTUS_ALLOW_GLOBAL_INPUT=1"));
        assert!(refusal.contains("cu_see"));
        require_background_route(true, "coordinate clicks")
            .expect("host opt-in must allow the foreground route");
    }

    #[test]
    fn background_capable_routes_request_background_mode() {
        let expected_focused_input = if cfg!(target_os = "macos") {
            InteractionMode::BackgroundOnly
        } else {
            InteractionMode::Interactive
        };
        assert_eq!(
            interaction_mode(&Action::Invoke, &element_target()),
            InteractionMode::BackgroundOnly
        );
        assert_eq!(
            interaction_mode(
                &Action::SetValue {
                    value: "value".to_string()
                },
                &element_target()
            ),
            InteractionMode::BackgroundOnly
        );
        assert_eq!(
            interaction_mode(
                &Action::TypeText {
                    text: "text".to_string(),
                    clear: false,
                    press_return: false,
                    delay_ms: None,
                },
                &element_target()
            ),
            expected_focused_input
        );
    }

    #[test]
    fn pointer_and_foreground_routes_stay_interactive() {
        let coordinate_target = TargetRef::Coordinates {
            x: 1,
            y: 2,
            display_id: "main".to_string(),
            display_geometry_hash: "0".repeat(64),
            snapshot_id: "snapshot".to_string(),
            snapshot_content_hash: "0".repeat(64),
        };
        assert_eq!(
            interaction_mode(
                &Action::Click {
                    button: MouseButton::Left,
                    count: 1,
                    allow_coordinate_fallback: false,
                },
                &coordinate_target
            ),
            InteractionMode::Interactive
        );
        assert_eq!(
            interaction_mode(
                &Action::Window {
                    operation: WindowOperation::Focus,
                    app: None,
                    title: None,
                },
                &TargetRef::None
            ),
            InteractionMode::Interactive
        );
        assert_eq!(
            interaction_mode(
                &Action::Open {
                    target: "https://example.com".to_string(),
                    app: None,
                    no_focus: true,
                },
                &TargetRef::None
            ),
            InteractionMode::Interactive
        );
    }
}
