//! AVO helpers: scored lineage, two-part objective, commit-if-better, stall.
//!
//! Engine capability only. Never commits to `main`/`master`. Never git-pushes.
//! `commit-if-better` refuses unless the caller is on a non-main branch and the
//! candidate objective is strictly greater than the best so far.

use std::fmt;

/// Scored lineage snapshot at step `t` (P_t).
#[derive(Debug, Clone, PartialEq)]
pub struct LineageScore {
    pub id: String,
    /// Posterior-style mass for this lineage (not required to sum to 1).
    pub p_t: f64,
    /// When true, [`objective_f`] is 0 regardless of quality.
    pub incorrect: bool,
    /// Second part of `f` (quality / utility). Ignored when `incorrect`.
    pub quality: f64,
}

/// Two-part objective: incorrect ⇒ 0, else the quality term.
pub fn objective_f(incorrect: bool, quality: f64) -> f64 {
    if incorrect {
        0.0
    } else {
        quality
    }
}

impl LineageScore {
    pub fn f(&self) -> f64 {
        objective_f(self.incorrect, self.quality)
    }
}

/// Softmax-style P_t over raw lineage logits.
pub fn lineage_p_t(logits: &[f64], temperature: f64) -> Vec<f64> {
    if logits.is_empty() {
        return Vec::new();
    }
    let t = if temperature.abs() < f64::EPSILON {
        1.0
    } else {
        temperature
    };
    let inf_count = logits
        .iter()
        .filter(|z| z.is_infinite() && **z > 0.0)
        .count();
    if inf_count > 0 {
        let mass = 1.0 / inf_count as f64;
        return logits
            .iter()
            .map(|z| {
                if z.is_infinite() && *z > 0.0 {
                    mass
                } else {
                    0.0
                }
            })
            .collect();
    }
    let finite: Vec<f64> = logits.iter().copied().filter(|z| z.is_finite()).collect();
    if finite.is_empty() {
        return vec![0.0; logits.len()];
    }
    let max = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = logits
        .iter()
        .map(|z| {
            if z.is_finite() {
                ((z - max) / t).exp()
            } else {
                0.0
            }
        })
        .collect();
    let sum: f64 = exps.iter().sum();
    if sum == 0.0 || !sum.is_finite() {
        return vec![0.0; logits.len()];
    }
    exps.into_iter().map(|e| e / sum).collect()
}

/// Why a commit was refused or accepted.
#[derive(Debug, Clone, PartialEq)]
pub enum CommitDecision {
    Accept { previous_best: f64, candidate: f64 },
    RejectNotBetter { best: f64, candidate: f64 },
    RefuseMain { branch: String },
    RejectNonFinite { best: f64, candidate: f64 },
}

impl fmt::Display for CommitDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Accept {
                previous_best,
                candidate,
            } => write!(f, "accept {candidate} > {previous_best}"),
            Self::RejectNotBetter { best, candidate } => {
                write!(f, "reject {candidate} <= {best}")
            }
            Self::RefuseMain { branch } => {
                write!(f, "refuse commit on protected branch {branch}")
            }
            Self::RejectNonFinite { best, candidate } => {
                write!(f, "reject non-finite score {candidate} vs {best}")
            }
        }
    }
}

pub fn is_protected_branch(branch: &str) -> bool {
    let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    matches!(branch, "main" | "master")
}

/// Commit-if-better: refuse main/master; accept only when `f(new) > f(best)`.
pub fn commit_if_better(
    branch: &str,
    best: &LineageScore,
    candidate: &LineageScore,
) -> CommitDecision {
    if is_protected_branch(branch) {
        return CommitDecision::RefuseMain {
            branch: branch.to_string(),
        };
    }
    let b = best.f();
    let c = candidate.f();
    if !b.is_finite() || !c.is_finite() {
        return CommitDecision::RejectNonFinite {
            best: b,
            candidate: c,
        };
    }
    if c > b {
        CommitDecision::Accept {
            previous_best: b,
            candidate: c,
        }
    } else {
        CommitDecision::RejectNotBetter {
            best: b,
            candidate: c,
        }
    }
}

/// Stall when improvement stays below `epsilon` for `patience` attempts.
#[derive(Debug, Clone, PartialEq)]
pub struct StallDetector {
    pub patience: usize,
    pub epsilon: f64,
    best: f64,
    stale: usize,
}

impl StallDetector {
    pub fn new(patience: usize, epsilon: f64) -> Self {
        Self {
            patience,
            epsilon,
            best: f64::NEG_INFINITY,
            stale: 0,
        }
    }

    /// Record `f` for a candidate. Returns true when stalled.
    /// Always agrees with [`Self::is_stalled`] after the update (including
    /// `patience == 0`).
    pub fn observe(&mut self, f: f64) -> bool {
        if f > self.best + self.epsilon {
            self.best = f;
            self.stale = 0;
        } else {
            self.stale += 1;
        }
        self.is_stalled()
    }

    pub fn is_stalled(&self) -> bool {
        self.stale >= self.patience
    }

    pub fn best(&self) -> f64 {
        self.best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incorrect_zeros_objective() {
        assert_eq!(objective_f(true, 9.0), 0.0);
        assert_eq!(objective_f(false, 9.0), 9.0);
    }

    #[test]
    fn p_t_sums_to_one() {
        let p = lineage_p_t(&[1.0, 1.0, 1.0], 1.0);
        let s: f64 = p.iter().sum();
        assert!((s - 1.0).abs() < 1e-9);
    }

    #[test]
    fn commit_if_better_refuses_main() {
        let best = LineageScore {
            id: "a".into(),
            p_t: 0.4,
            incorrect: false,
            quality: 1.0,
        };
        let cand = LineageScore {
            id: "b".into(),
            p_t: 0.6,
            incorrect: false,
            quality: 2.0,
        };
        assert!(matches!(
            commit_if_better("main", &best, &cand),
            CommitDecision::RefuseMain { .. }
        ));
        assert!(matches!(
            commit_if_better("feat/x", &best, &cand),
            CommitDecision::Accept { .. }
        ));
    }

    #[test]
    fn stall_detects_flat_line() {
        let mut s = StallDetector::new(3, 0.01);
        assert!(!s.observe(1.0));
        assert!(!s.observe(1.0));
        assert!(!s.observe(1.0));
        assert!(s.observe(1.0));
        assert!(s.is_stalled());
    }

    fn score(quality: f64) -> LineageScore {
        LineageScore {
            id: "x".into(),
            p_t: 1.0,
            incorrect: false,
            quality,
        }
    }

    #[test]
    fn commit_if_better_rejects_non_finite() {
        let best = score(10.0);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(
                commit_if_better("feat/x", &best, &score(bad)),
                CommitDecision::RejectNonFinite { .. }
            ));
            assert!(matches!(
                commit_if_better("feat/x", &score(bad), &score(11.0)),
                CommitDecision::RejectNonFinite { .. }
            ));
        }
    }

    #[test]
    fn plus_inf_lineage_keeps_dominant_mass() {
        let p = lineage_p_t(&[f64::INFINITY, 0.0], 1.0);
        assert!((p[0] - 1.0).abs() < 1e-12);
        assert!(p[1].abs() < 1e-12);
        let nan_p = lineage_p_t(&[f64::NAN, 1.0], 1.0);
        assert!(nan_p[0].abs() < 1e-12);
        assert!((nan_p[1] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn protects_canonical_main_ref() {
        let best = score(1.0);
        let cand = score(2.0);
        assert!(is_protected_branch("refs/heads/main"));
        assert!(is_protected_branch("refs/heads/master"));
        assert!(matches!(
            commit_if_better("refs/heads/main", &best, &cand),
            CommitDecision::RefuseMain { .. }
        ));
    }

    #[test]
    fn zero_patience_observe_matches_is_stalled() {
        let mut s = StallDetector::new(0, 0.01);
        assert!(s.is_stalled());
        assert!(s.observe(1.0));
        assert!(s.is_stalled());
    }
}
