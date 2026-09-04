use crate::avo::{objective_f, LineageScore};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub claim_id: String,
    pub actor_id: String,
    pub note: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceLedger {
    pub entries: Vec<Evidence>,
}

impl EvidenceLedger {
    pub fn record(&mut self, evidence: Evidence) {
        self.entries.push(evidence);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtaskStatus {
    Open,
    Claimed,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subtask {
    pub id: String,
    pub parent_id: Option<String>,
    pub status: SubtaskStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtaskClaim {
    pub actor_id: String,
    pub target_id: String,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostAdjudication {
    Accept,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Recorded,
    RejectedChildMarkedParent,
    RejectedHost,
    RejectedUnknown,
}

fn is_descendant(tasks: &[Subtask], ancestor: &str, node: &str) -> bool {
    let mut current = node;
    let mut guard = 0usize;
    while guard < tasks.len() {
        let Some(task) = tasks.iter().find(|t| t.id == current) else {
            return false;
        };
        match task.parent_id.as_deref() {
            Some(parent) if parent == ancestor => return true,
            Some(parent) => current = parent,
            None => return false,
        }
        guard += 1;
    }
    false
}

pub fn claim_complete(
    tasks: &mut [Subtask],
    ledger: &mut EvidenceLedger,
    claim: SubtaskClaim,
    host: HostAdjudication,
) -> ClaimOutcome {
    if host != HostAdjudication::Accept {
        return ClaimOutcome::RejectedHost;
    }
    if !tasks.iter().any(|t| t.id == claim.target_id) {
        return ClaimOutcome::RejectedUnknown;
    }
    let downward = claim.actor_id == claim.target_id
        || is_descendant(tasks, &claim.actor_id, &claim.target_id);
    if !downward {
        return ClaimOutcome::RejectedChildMarkedParent;
    }
    if let Some(task) = tasks.iter_mut().find(|t| t.id == claim.target_id) {
        task.status = SubtaskStatus::Complete;
    }
    ledger.record(claim.evidence);
    ClaimOutcome::Recorded
}

pub fn avo_score_for_claim(incorrect: bool, quality: f64) -> LineageScore {
    LineageScore {
        id: "subtask".into(),
        p_t: 1.0,
        incorrect,
        quality: objective_f(incorrect, quality),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Vec<Subtask> {
        vec![
            Subtask {
                id: "parent".into(),
                parent_id: None,
                status: SubtaskStatus::Open,
            },
            Subtask {
                id: "child".into(),
                parent_id: Some("parent".into()),
                status: SubtaskStatus::Open,
            },
        ]
    }

    fn claim(actor: &str, target: &str) -> SubtaskClaim {
        SubtaskClaim {
            actor_id: actor.into(),
            target_id: target.into(),
            evidence: Evidence {
                claim_id: format!("{actor}->{target}"),
                actor_id: actor.into(),
                note: "done".into(),
            },
        }
    }

    #[test]
    fn child_cannot_mark_parent_complete() {
        let mut tasks = tree();
        let mut ledger = EvidenceLedger::default();
        let outcome = claim_complete(
            &mut tasks,
            &mut ledger,
            claim("child", "parent"),
            HostAdjudication::Accept,
        );
        assert_eq!(outcome, ClaimOutcome::RejectedChildMarkedParent);
        assert_eq!(tasks[0].status, SubtaskStatus::Open);
        assert!(ledger.entries.is_empty());
    }

    #[test]
    fn host_rejects_without_recording() {
        let mut tasks = tree();
        let mut ledger = EvidenceLedger::default();
        let outcome = claim_complete(
            &mut tasks,
            &mut ledger,
            claim("parent", "child"),
            HostAdjudication::Reject,
        );
        assert_eq!(outcome, ClaimOutcome::RejectedHost);
        assert!(ledger.entries.is_empty());
    }

    #[test]
    fn parent_can_mark_child_and_self() {
        let mut tasks = tree();
        let mut ledger = EvidenceLedger::default();
        assert_eq!(
            claim_complete(
                &mut tasks,
                &mut ledger,
                claim("parent", "child"),
                HostAdjudication::Accept
            ),
            ClaimOutcome::Recorded
        );
        assert_eq!(tasks[1].status, SubtaskStatus::Complete);
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(
            claim_complete(
                &mut tasks,
                &mut ledger,
                claim("child", "child"),
                HostAdjudication::Accept
            ),
            ClaimOutcome::Recorded
        );
    }
}
