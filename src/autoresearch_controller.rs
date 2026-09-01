//! Isolated, host-driven autoresearch experiments.
//!
//! This module owns the mechanics of an experiment: a detached Git worktree,
//! checkpointing, bounded measurement and guard commands, rollback, durable
//! events, and an explicit final-patch handoff. It deliberately does not own
//! scheduling or hypothesis generation. A host supplies one hypothesis and an
//! async callback that applies it to the supplied worktree for each iteration.

use crate::autoresearch::MetricDirection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};

const MAX_COMMAND_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_MEASURE_TIMEOUT_SECONDS: u64 = 600;
const DEFAULT_CHECKS_TIMEOUT_SECONDS: u64 = 300;

/// A budget that bounds the controller, rather than the agent loop.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AutoresearchBudget {
    pub max_iterations: Option<usize>,
    pub max_duration_seconds: Option<u64>,
    pub max_cost_usd: Option<f64>,
    pub max_disk_bytes: Option<u64>,
}

/// Strict controller configuration. Correctness checks are required for a
/// controller even though the older [`crate::autoresearch::AutoresearchSession`]
/// permits host-supplied measurements without checks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutoresearchControllerConfig {
    pub name: String,
    pub metric_name: String,
    #[serde(default)]
    pub metric_unit: String,
    #[serde(default)]
    pub direction: MetricDirection,
    /// A deterministic command that prints `METRIC name=value`.
    pub measure_command: String,
    /// A required correctness command. Exit code zero is required.
    pub checks_command: String,
    #[serde(default = "default_measure_timeout_seconds")]
    pub measure_timeout_seconds: u64,
    #[serde(default = "default_checks_timeout_seconds")]
    pub checks_timeout_seconds: u64,
    #[serde(default)]
    pub warmup_runs: usize,
    #[serde(default = "default_measurement_runs")]
    pub measurement_runs: usize,
    /// Absolute improvement required to accept a candidate.
    #[serde(default)]
    pub min_improvement: f64,
    #[serde(default)]
    pub budget: AutoresearchBudget,
}

fn default_measurement_runs() -> usize {
    1
}

fn default_measure_timeout_seconds() -> u64 {
    DEFAULT_MEASURE_TIMEOUT_SECONDS
}

fn default_checks_timeout_seconds() -> u64 {
    DEFAULT_CHECKS_TIMEOUT_SECONDS
}

impl AutoresearchControllerConfig {
    pub fn new(
        name: impl Into<String>,
        metric_name: impl Into<String>,
        measure_command: impl Into<String>,
        checks_command: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            metric_name: metric_name.into(),
            metric_unit: String::new(),
            direction: MetricDirection::Lower,
            measure_command: measure_command.into(),
            checks_command: checks_command.into(),
            measure_timeout_seconds: DEFAULT_MEASURE_TIMEOUT_SECONDS,
            checks_timeout_seconds: DEFAULT_CHECKS_TIMEOUT_SECONDS,
            warmup_runs: 0,
            measurement_runs: 1,
            min_improvement: 0.0,
            budget: AutoresearchBudget::default(),
        }
    }

    fn validate(&self) -> Result<(), AutoresearchControllerError> {
        if self.name.trim().is_empty() {
            return Err(AutoresearchControllerError::InvalidConfig(
                "name must not be empty".into(),
            ));
        }
        if self.metric_name.trim().is_empty() {
            return Err(AutoresearchControllerError::InvalidConfig(
                "metric_name must not be empty".into(),
            ));
        }
        if self.measure_command.trim().is_empty() {
            return Err(AutoresearchControllerError::InvalidConfig(
                "measure_command must not be empty".into(),
            ));
        }
        if self.checks_command.trim().is_empty() {
            return Err(AutoresearchControllerError::InvalidConfig(
                "checks_command is required".into(),
            ));
        }
        if self.measure_timeout_seconds == 0 || self.checks_timeout_seconds == 0 {
            return Err(AutoresearchControllerError::InvalidConfig(
                "command timeouts must be greater than zero".into(),
            ));
        }
        if self.measurement_runs == 0 {
            return Err(AutoresearchControllerError::InvalidConfig(
                "measurement_runs must be greater than zero".into(),
            ));
        }
        if !self.min_improvement.is_finite() || self.min_improvement < 0.0 {
            return Err(AutoresearchControllerError::InvalidConfig(
                "min_improvement must be finite and non-negative".into(),
            ));
        }
        if self.budget.max_iterations == Some(0)
            || self.budget.max_duration_seconds == Some(0)
            || self.budget.max_cost_usd == Some(0.0)
            || self.budget.max_disk_bytes == Some(0)
        {
            return Err(AutoresearchControllerError::InvalidConfig(
                "budgets must be greater than zero when set".into(),
            ));
        }
        if self
            .budget
            .max_cost_usd
            .is_some_and(|cost| !cost.is_finite() || cost < 0.0)
        {
            return Err(AutoresearchControllerError::InvalidConfig(
                "max_cost_usd must be finite and non-negative".into(),
            ));
        }
        Ok(())
    }
}

/// A cancellation source shared by a controller and the host callback that
/// applies a hypothesis.
#[derive(Debug, Clone, Default)]
pub struct AutoresearchCancellation {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl AutoresearchCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::SeqCst) {
            self.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        let notified = self.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// One host-formed hypothesis. The controller executes exactly one such
/// hypothesis per [`AutoresearchController::run_iteration`] call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperimentHypothesis {
    pub id: String,
    pub description: String,
}

impl ExperimentHypothesis {
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
        }
    }
}

/// Cost attributable to the host's hypothesis application. rx4 does not
/// guess provider pricing; the host supplies the measured delta.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct HypothesisOutcome {
    #[serde(default)]
    pub cost_usd: f64,
}

/// The only workspace exposed to a hypothesis applier.
#[derive(Debug, Clone)]
pub struct ExperimentWorkspace {
    path: PathBuf,
    checkpoint: String,
    cancellation: AutoresearchCancellation,
}

impl ExperimentWorkspace {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn checkpoint(&self) -> &str {
        &self.checkpoint
    }

    pub fn cancellation(&self) -> AutoresearchCancellation {
        self.cancellation.clone()
    }
}

/// A median-aggregated measurement and its guard result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AggregatedMeasurement {
    pub metric: f64,
    pub samples: Vec<f64>,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    pub warmup_runs: usize,
    pub measurement_runs: usize,
    pub duration_ms: u64,
    pub checks_passed: bool,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub checks_output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineResult {
    pub commit: String,
    pub measurement: AggregatedMeasurement,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IterationStatus {
    Accepted,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutoresearchIteration {
    pub iteration: usize,
    pub hypothesis: ExperimentHypothesis,
    pub status: IterationStatus,
    pub checkpoint: String,
    pub candidate_commit: Option<String>,
    pub metric: Option<f64>,
    pub samples: Vec<f64>,
    pub improvement: Option<f64>,
    pub cost_usd: f64,
    pub duration_ms: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompletionReason {
    Explicit,
    Cancelled,
    MaxIterations,
    DurationBudget,
    CostBudget,
    DiskBudget,
    FinalPatchAccepted,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutoresearchCompletion {
    pub reason: CompletionReason,
    pub iterations: usize,
    pub accepted_iterations: usize,
    pub best_metric: Option<f64>,
    pub accepted_commit: String,
    pub total_cost_usd: f64,
}

/// An explicit diff handoff. Constructing or inspecting this value never
/// changes the user's workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalPatch {
    pub base_commit: String,
    pub accepted_commit: String,
    pub changed_files: Vec<String>,
    pub patch: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BudgetKind {
    MaxIterations,
    Duration,
    Cost,
    Disk,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutoresearchEvent {
    Baseline {
        commit: String,
        metric: f64,
        samples: Vec<f64>,
        duration_ms: u64,
    },
    Iteration {
        iteration: usize,
        hypothesis: ExperimentHypothesis,
        checkpoint: String,
    },
    Accepted {
        iteration: usize,
        commit: String,
        metric: f64,
        samples: Vec<f64>,
        improvement: f64,
        cost_usd: f64,
    },
    Rejected {
        iteration: usize,
        metric: Option<f64>,
        samples: Vec<f64>,
        reason: String,
    },
    Failed {
        iteration: usize,
        reason: String,
        rolled_back: bool,
    },
    Completed {
        completion: AutoresearchCompletion,
    },
}

pub type AutoresearchSubscriber = Arc<dyn Fn(&AutoresearchEvent) + Send + Sync>;

#[derive(Debug, Error)]
pub enum AutoresearchControllerError {
    #[error("invalid autoresearch controller configuration: {0}")]
    InvalidConfig(String),
    #[error("autoresearch controller io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("autoresearch controller json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("autoresearch git error: {0}")]
    Git(String),
    #[error("autoresearch command error: {0}")]
    Command(String),
    #[error("autoresearch controller was cancelled")]
    Cancelled,
    #[error("autoresearch {kind:?} budget exceeded")]
    BudgetExceeded { kind: BudgetKind },
    #[error("autoresearch baseline failed: {0}")]
    BaselineFailed(String),
    #[error("autoresearch controller is not ready: {0}")]
    NotReady(String),
    #[error("autoresearch controller is already completed")]
    Completed,
}

struct ProcessResult {
    output: String,
    exit_code: Option<i32>,
    timed_out: bool,
    cancelled: bool,
    duration_ms: u64,
}

struct Evaluation {
    measurement: AggregatedMeasurement,
}

struct IterationContext {
    iteration_number: usize,
    hypothesis: ExperimentHypothesis,
    checkpoint: String,
    cost_usd: f64,
    started: Instant,
}

impl IterationContext {
    fn new(
        iteration_number: usize,
        hypothesis: ExperimentHypothesis,
        checkpoint: String,
        cost_usd: f64,
        started: Instant,
    ) -> Self {
        Self {
            iteration_number,
            hypothesis,
            checkpoint,
            cost_usd,
            started,
        }
    }
}

/// The isolated experiment controller.
pub struct AutoresearchController {
    root: PathBuf,
    worktree: PathBuf,
    session_dir: PathBuf,
    events_path: PathBuf,
    iterations_path: PathBuf,
    config: AutoresearchControllerConfig,
    base_commit: String,
    accepted_commit: String,
    baseline: Option<BaselineResult>,
    iterations: Vec<AutoresearchIteration>,
    events: Vec<AutoresearchEvent>,
    subscribers: Vec<AutoresearchSubscriber>,
    command_sandbox: Option<Arc<crate::sandbox::SandboxManager>>,
    cancellation: AutoresearchCancellation,
    started_at: Instant,
    total_cost_usd: f64,
    completion: Option<AutoresearchCompletion>,
    closed: bool,
}

/// Shared SDK handle for a host that wants to expose the controller through an
/// agent/session object without changing the agent loop.
pub type AutoresearchControllerHandle = Arc<Mutex<AutoresearchController>>;

pub fn new_controller_handle(controller: AutoresearchController) -> AutoresearchControllerHandle {
    Arc::new(Mutex::new(controller))
}

impl AutoresearchController {
    /// Create a detached worktree from a clean repository HEAD. This writes
    /// only to a controller-owned temporary directory, not the real checkout.
    pub async fn new(
        root: impl Into<PathBuf>,
        config: AutoresearchControllerConfig,
    ) -> Result<Self, AutoresearchControllerError> {
        config.validate()?;
        let requested_root = root.into();
        let root = std::fs::canonicalize(&requested_root)?;
        if !root.is_dir() {
            return Err(AutoresearchControllerError::InvalidConfig(format!(
                "workspace is not a directory: {}",
                root.display()
            )));
        }
        let no_cancel = AutoresearchCancellation::new();
        let git_root = git_output(&root, &["rev-parse", "--show-toplevel"], &no_cancel).await?;
        let git_root = std::fs::canonicalize(git_root.trim())?;
        if git_root != root {
            return Err(AutoresearchControllerError::InvalidConfig(
                "controller root must be the Git repository root".into(),
            ));
        }
        let status = git_output(
            &root,
            &["status", "--porcelain=v1", "--untracked-files=all"],
            &no_cancel,
        )
        .await?;
        if !status.trim().is_empty() {
            return Err(AutoresearchControllerError::InvalidConfig(
                "workspace must be clean before starting an experiment".into(),
            ));
        }
        let base_commit = git_output(&root, &["rev-parse", "HEAD"], &no_cancel)
            .await?
            .trim()
            .to_string();
        if base_commit.is_empty() {
            return Err(AutoresearchControllerError::Git(
                "repository has no HEAD commit".into(),
            ));
        }

        let id = uuid::Uuid::new_v4().to_string();
        let sessions_root = std::env::temp_dir().join("rx4-autoresearch-sessions");
        create_private_dir(&sessions_root)?;
        let session_dir = sessions_root.join(&id);
        create_private_dir(&session_dir)?;
        let config_path = session_dir.join("config.json");
        std::fs::write(&config_path, serde_json::to_vec_pretty(&config)?)?;
        let worktrees_root = std::env::temp_dir().join("rx4-autoresearch-worktrees");
        create_private_dir(&worktrees_root)?;
        let worktree = worktrees_root.join(&id);
        let worktree_args = vec![
            "worktree".to_string(),
            "add".to_string(),
            "--detach".to_string(),
            worktree.to_string_lossy().into_owned(),
            base_commit.clone(),
        ];
        let result = run_process(
            "git",
            &worktree_args,
            &root,
            Duration::from_secs(60),
            Some(&no_cancel),
        )
        .await?;
        if result.cancelled || result.timed_out || result.exit_code != Some(0) {
            return Err(AutoresearchControllerError::Git(result.output));
        }
        set_private_permissions(&worktree)?;

        let events_path = session_dir.join("events.jsonl");
        let iterations_path = session_dir.join("iterations.jsonl");
        let controller = Self {
            root,
            worktree,
            session_dir,
            events_path,
            iterations_path,
            config,
            base_commit: base_commit.clone(),
            accepted_commit: base_commit,
            baseline: None,
            iterations: Vec::new(),
            events: Vec::new(),
            subscribers: Vec::new(),
            command_sandbox: None,
            cancellation: AutoresearchCancellation::new(),
            started_at: Instant::now(),
            total_cost_usd: 0.0,
            completion: None,
            closed: false,
        };
        if let Some(limit) = controller.config.budget.max_disk_bytes {
            if directory_size(&controller.session_dir)? + directory_size(&controller.worktree)?
                > limit
            {
                let _ = remove_worktree(&controller.root, &controller.worktree).await;
                return Err(AutoresearchControllerError::BudgetExceeded {
                    kind: BudgetKind::Disk,
                });
            }
        }
        Ok(controller)
    }

    pub fn config(&self) -> &AutoresearchControllerConfig {
        &self.config
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn workspace_path(&self) -> &Path {
        &self.worktree
    }

    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub fn log_path(&self) -> &Path {
        &self.events_path
    }

    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }

    pub fn accepted_commit(&self) -> &str {
        &self.accepted_commit
    }

    pub fn baseline(&self) -> Option<&BaselineResult> {
        self.baseline.as_ref()
    }

    pub fn iterations(&self) -> &[AutoresearchIteration] {
        &self.iterations
    }

    pub fn events(&self) -> &[AutoresearchEvent] {
        &self.events
    }

    pub fn total_cost_usd(&self) -> f64 {
        self.total_cost_usd
    }

    pub fn completion(&self) -> Option<&AutoresearchCompletion> {
        self.completion.as_ref()
    }

    pub fn cancellation_handle(&self) -> AutoresearchCancellation {
        self.cancellation.clone()
    }

    pub fn subscribe(&mut self, subscriber: impl Fn(&AutoresearchEvent) + Send + Sync + 'static) {
        self.subscribers.push(Arc::new(subscriber));
    }

    /// Install the host's command validation capability for measurement and
    /// guard commands. This is optional because rotary exposes capabilities;
    /// hosts choose the appropriate userspace/OS sandbox policy.
    pub fn set_command_sandbox(&mut self, sandbox: Arc<crate::sandbox::SandboxManager>) {
        self.command_sandbox = Some(sandbox);
    }

    pub fn clear_command_sandbox(&mut self) {
        self.command_sandbox = None;
    }

    /// Measure the initial checkpoint and emit the baseline event. Hosts can
    /// subscribe before calling this method so baseline failures are visible.
    pub async fn establish_baseline(
        &mut self,
    ) -> Result<BaselineResult, AutoresearchControllerError> {
        self.ensure_open()?;
        if let Some(baseline) = &self.baseline {
            return Ok(baseline.clone());
        }
        if let Err(error) = self.check_budget() {
            self.finish_for_error(&error)?;
            return Err(error);
        }
        let evaluation = match self.evaluate().await {
            Ok(evaluation) => evaluation,
            Err(error) => {
                let reason = error.to_string();
                self.emit(AutoresearchEvent::Failed {
                    iteration: 0,
                    reason: reason.clone(),
                    rolled_back: false,
                })?;
                self.finish(CompletionReason::Failed)?;
                return Err(AutoresearchControllerError::BaselineFailed(reason));
            }
        };
        if !evaluation.measurement.checks_passed {
            let reason = "baseline correctness checks failed".to_string();
            self.emit(AutoresearchEvent::Failed {
                iteration: 0,
                reason: reason.clone(),
                rolled_back: false,
            })?;
            self.finish(CompletionReason::Failed)?;
            return Err(AutoresearchControllerError::BaselineFailed(reason));
        }
        let baseline = BaselineResult {
            commit: self.accepted_commit.clone(),
            measurement: evaluation.measurement,
        };
        self.emit(AutoresearchEvent::Baseline {
            commit: baseline.commit.clone(),
            metric: baseline.measurement.metric,
            samples: baseline.measurement.samples.clone(),
            duration_ms: baseline.measurement.duration_ms,
        })?;
        self.baseline = Some(baseline.clone());
        Ok(baseline)
    }

    /// Apply, measure, guard, accept or rollback one hypothesis.
    pub async fn run_iteration<F, Fut>(
        &mut self,
        hypothesis: ExperimentHypothesis,
        apply: F,
    ) -> Result<AutoresearchIteration, AutoresearchControllerError>
    where
        F: FnOnce(ExperimentWorkspace) -> Fut,
        Fut: Future<Output = Result<HypothesisOutcome, String>>,
    {
        self.ensure_open()?;
        if self.baseline.is_none() {
            return Err(AutoresearchControllerError::NotReady(
                "establish_baseline must be called first".into(),
            ));
        }
        if hypothesis.id.trim().is_empty() || hypothesis.description.trim().is_empty() {
            return Err(AutoresearchControllerError::InvalidConfig(
                "hypothesis id and description must not be empty".into(),
            ));
        }
        if let Err(error) = self.check_budget() {
            self.finish_for_error(&error)?;
            return Err(error);
        }
        let iteration_number = self.iterations.len() + 1;
        let checkpoint = self.accepted_commit.clone();
        self.restore_checkpoint().await?;
        self.emit(AutoresearchEvent::Iteration {
            iteration: iteration_number,
            hypothesis: hypothesis.clone(),
            checkpoint: checkpoint.clone(),
        })?;
        let started = Instant::now();
        let workspace = ExperimentWorkspace {
            path: self.worktree.clone(),
            checkpoint: checkpoint.clone(),
            cancellation: self.cancellation.clone(),
        };
        let outcome = apply(workspace).await;
        let mut cost_usd = 0.0;
        let result = match outcome {
            Ok(outcome) if outcome.cost_usd.is_finite() && outcome.cost_usd >= 0.0 => {
                cost_usd = outcome.cost_usd;
                self.total_cost_usd += cost_usd;
                if self.cancellation.is_cancelled() {
                    self.fail_iteration(
                        IterationContext::new(
                            iteration_number,
                            hypothesis,
                            checkpoint,
                            cost_usd,
                            started,
                        ),
                        "cancelled while applying hypothesis".into(),
                        CompletionReason::Cancelled,
                    )
                    .await?
                } else if let Some(max) = self.config.budget.max_cost_usd {
                    if self.total_cost_usd > max {
                        self.fail_iteration(
                            IterationContext::new(
                                iteration_number,
                                hypothesis,
                                checkpoint,
                                cost_usd,
                                started,
                            ),
                            format!(
                                "cost budget exceeded: ${:.4} > ${max:.4}",
                                self.total_cost_usd
                            ),
                            CompletionReason::CostBudget,
                        )
                        .await?
                    } else {
                        self.prepare_and_evaluate(
                            iteration_number,
                            hypothesis,
                            checkpoint,
                            cost_usd,
                            started,
                        )
                        .await?
                    }
                } else {
                    self.prepare_and_evaluate(
                        iteration_number,
                        hypothesis,
                        checkpoint,
                        cost_usd,
                        started,
                    )
                    .await?
                }
            }
            Ok(_) => {
                self.fail_iteration(
                    IterationContext::new(
                        iteration_number,
                        hypothesis,
                        checkpoint,
                        cost_usd,
                        started,
                    ),
                    "hypothesis returned a non-finite or negative cost".into(),
                    CompletionReason::Failed,
                )
                .await?
            }
            Err(reason) => {
                let completion = if self.cancellation.is_cancelled() {
                    CompletionReason::Cancelled
                } else {
                    CompletionReason::Failed
                };
                self.fail_iteration(
                    IterationContext::new(
                        iteration_number,
                        hypothesis,
                        checkpoint,
                        cost_usd,
                        started,
                    ),
                    reason,
                    completion,
                )
                .await?
            }
        };
        if self.completion.is_none() {
            if self
                .config
                .budget
                .max_iterations
                .is_some_and(|max| self.iterations.len() >= max)
            {
                self.finish(CompletionReason::MaxIterations)?;
            } else if self
                .config
                .budget
                .max_cost_usd
                .is_some_and(|max| self.total_cost_usd >= max)
            {
                self.finish(CompletionReason::CostBudget)?;
            }
        }
        Ok(result)
    }

    async fn prepare_and_evaluate(
        &mut self,
        iteration_number: usize,
        hypothesis: ExperimentHypothesis,
        checkpoint: String,
        cost_usd: f64,
        started: Instant,
    ) -> Result<AutoresearchIteration, AutoresearchControllerError> {
        let candidate_commit = match self.commit_candidate(&hypothesis).await {
            Ok(Some(commit)) => commit,
            Ok(None) => {
                return self
                    .reject_iteration(
                        IterationContext::new(
                            iteration_number,
                            hypothesis,
                            checkpoint,
                            cost_usd,
                            started,
                        ),
                        None,
                        Vec::new(),
                        "hypothesis produced no Git changes".into(),
                    )
                    .await;
            }
            Err(error) => {
                let completion = if matches!(error, AutoresearchControllerError::Cancelled) {
                    CompletionReason::Cancelled
                } else {
                    CompletionReason::Failed
                };
                return self
                    .fail_iteration(
                        IterationContext::new(
                            iteration_number,
                            hypothesis,
                            checkpoint,
                            cost_usd,
                            started,
                        ),
                        error.to_string(),
                        completion,
                    )
                    .await;
            }
        };
        self.evaluate_candidate(
            iteration_number,
            hypothesis,
            checkpoint,
            candidate_commit,
            cost_usd,
            started,
        )
        .await
    }

    async fn evaluate_candidate(
        &mut self,
        iteration_number: usize,
        hypothesis: ExperimentHypothesis,
        checkpoint: String,
        candidate_commit: String,
        cost_usd: f64,
        started: Instant,
    ) -> Result<AutoresearchIteration, AutoresearchControllerError> {
        if let Err(error) = self.check_budget() {
            return self
                .fail_iteration(
                    IterationContext::new(
                        iteration_number,
                        hypothesis,
                        checkpoint,
                        cost_usd,
                        started,
                    ),
                    error.to_string(),
                    completion_for_error(&error),
                )
                .await;
        }
        let evaluation = match self.evaluate().await {
            Ok(evaluation) => evaluation,
            Err(error) => {
                return self
                    .fail_iteration(
                        IterationContext::new(
                            iteration_number,
                            hypothesis,
                            checkpoint,
                            cost_usd,
                            started,
                        ),
                        error.to_string(),
                        if matches!(error, AutoresearchControllerError::Cancelled) {
                            CompletionReason::Cancelled
                        } else {
                            CompletionReason::Failed
                        },
                    )
                    .await;
            }
        };
        if let Err(error) = self.check_budget() {
            return self
                .fail_iteration(
                    IterationContext::new(
                        iteration_number,
                        hypothesis,
                        checkpoint,
                        cost_usd,
                        started,
                    ),
                    error.to_string(),
                    completion_for_error(&error),
                )
                .await;
        }
        let metric = evaluation.measurement.metric;
        if !evaluation.measurement.checks_passed {
            return self
                .reject_iteration(
                    IterationContext::new(
                        iteration_number,
                        hypothesis,
                        checkpoint,
                        cost_usd,
                        started,
                    ),
                    Some(metric),
                    evaluation.measurement.samples.clone(),
                    "correctness checks failed".into(),
                )
                .await;
        }
        let Some(best) = self.best_metric() else {
            return Err(AutoresearchControllerError::NotReady(
                "baseline metric is missing".into(),
            ));
        };
        let improvement = improvement(self.config.direction, best, metric);
        if !is_improvement(
            self.config.direction,
            best,
            metric,
            self.config.min_improvement,
        ) {
            return self
                .reject_iteration(
                    IterationContext::new(
                        iteration_number,
                        hypothesis,
                        checkpoint,
                        cost_usd,
                        started,
                    ),
                    Some(metric),
                    evaluation.measurement.samples.clone(),
                    format!(
                        "metric did not improve by threshold: best={best}, candidate={metric}, improvement={improvement}"
                    ),
                )
                .await;
        }

        // Measurement and guard commands may create build artifacts or mutate
        // tracked files. Restore the provisional candidate commit before
        // publishing it so only hypothesis changes enter the final patch.
        self.restore_commit(&candidate_commit).await?;
        self.accepted_commit = candidate_commit.clone();
        let iteration = AutoresearchIteration {
            iteration: iteration_number,
            hypothesis,
            status: IterationStatus::Accepted,
            checkpoint,
            candidate_commit: Some(candidate_commit.clone()),
            metric: Some(metric),
            samples: evaluation.measurement.samples,
            improvement: Some(improvement),
            cost_usd,
            duration_ms: started.elapsed().as_millis() as u64,
            reason: "metric improved and correctness checks passed".into(),
        };
        append_json_line(&self.iterations_path, &iteration)?;
        self.iterations.push(iteration.clone());
        self.emit(AutoresearchEvent::Accepted {
            iteration: iteration_number,
            commit: candidate_commit,
            metric,
            samples: iteration.samples.clone(),
            improvement,
            cost_usd,
        })?;
        if self
            .config
            .budget
            .max_iterations
            .is_some_and(|max| self.iterations.len() >= max)
        {
            self.finish(CompletionReason::MaxIterations)?;
        }
        Ok(iteration)
    }

    async fn commit_candidate(
        &self,
        hypothesis: &ExperimentHypothesis,
    ) -> Result<Option<String>, AutoresearchControllerError> {
        let add = git_at(
            &self.worktree,
            &["add".to_string(), "-A".to_string()],
            Duration::from_secs(60),
            Some(&self.cancellation),
        )
        .await?;
        if add.cancelled || self.cancellation.is_cancelled() {
            return Err(AutoresearchControllerError::Cancelled);
        }
        if add.exit_code != Some(0) {
            return Err(AutoresearchControllerError::Git(format!(
                "git add failed: {}",
                add.output
            )));
        }
        let diff_check = git_at(
            &self.worktree,
            &[
                "diff".to_string(),
                "--cached".to_string(),
                "--quiet".to_string(),
            ],
            Duration::from_secs(60),
            Some(&self.cancellation),
        )
        .await?;
        if diff_check.cancelled || self.cancellation.is_cancelled() {
            return Err(AutoresearchControllerError::Cancelled);
        }
        if diff_check.exit_code == Some(0) {
            return Ok(None);
        }
        if diff_check.exit_code != Some(1) {
            return Err(AutoresearchControllerError::Git(format!(
                "git diff --cached failed: {}",
                diff_check.output
            )));
        }
        let message = format!(
            "autoresearch: {}",
            hypothesis
                .description
                .chars()
                .filter(|c| *c != '\n' && *c != '\r')
                .take(160)
                .collect::<String>()
        );
        let commit_args = vec![
            "-c".to_string(),
            "user.name=rx4 autoresearch".to_string(),
            "-c".to_string(),
            "user.email=rx4-autoresearch@localhost".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            message,
        ];
        let commit = git_at(
            &self.worktree,
            &commit_args,
            Duration::from_secs(60),
            Some(&self.cancellation),
        )
        .await?;
        if commit.cancelled || self.cancellation.is_cancelled() {
            return Err(AutoresearchControllerError::Cancelled);
        }
        if commit.exit_code != Some(0) {
            return Err(AutoresearchControllerError::Git(format!(
                "git commit failed: {}",
                commit.output
            )));
        }
        Ok(Some(
            git_output(&self.worktree, &["rev-parse", "HEAD"], &self.cancellation)
                .await?
                .trim()
                .to_string(),
        ))
    }

    async fn reject_iteration(
        &mut self,
        context: IterationContext,
        metric: Option<f64>,
        samples: Vec<f64>,
        reason: String,
    ) -> Result<AutoresearchIteration, AutoresearchControllerError> {
        let IterationContext {
            iteration_number,
            hypothesis,
            checkpoint,
            cost_usd,
            started,
        } = context;
        self.restore_checkpoint().await?;
        let iteration = AutoresearchIteration {
            iteration: iteration_number,
            hypothesis,
            status: IterationStatus::Rejected,
            checkpoint,
            candidate_commit: None,
            metric,
            samples,
            improvement: None,
            cost_usd,
            duration_ms: started.elapsed().as_millis() as u64,
            reason: reason.clone(),
        };
        append_json_line(&self.iterations_path, &iteration)?;
        self.iterations.push(iteration.clone());
        self.emit(AutoresearchEvent::Rejected {
            iteration: iteration_number,
            metric,
            samples: iteration.samples.clone(),
            reason,
        })?;
        Ok(iteration)
    }

    async fn fail_iteration(
        &mut self,
        context: IterationContext,
        reason: String,
        completion_reason: CompletionReason,
    ) -> Result<AutoresearchIteration, AutoresearchControllerError> {
        let IterationContext {
            iteration_number,
            hypothesis,
            checkpoint,
            cost_usd,
            started,
        } = context;
        self.restore_checkpoint().await?;
        let iteration = AutoresearchIteration {
            iteration: iteration_number,
            hypothesis,
            status: IterationStatus::Failed,
            checkpoint,
            candidate_commit: None,
            metric: None,
            samples: Vec::new(),
            improvement: None,
            cost_usd,
            duration_ms: started.elapsed().as_millis() as u64,
            reason: reason.clone(),
        };
        append_json_line(&self.iterations_path, &iteration)?;
        self.iterations.push(iteration.clone());
        self.emit(AutoresearchEvent::Failed {
            iteration: iteration_number,
            reason,
            rolled_back: true,
        })?;
        if !matches!(completion_reason, CompletionReason::Failed)
            || self.cancellation.is_cancelled()
        {
            self.finish(completion_reason)?;
        }
        Ok(iteration)
    }

    async fn evaluate(&mut self) -> Result<Evaluation, AutoresearchControllerError> {
        let started = Instant::now();

        self.run_warmups().await?;

        let (samples, aggregated_metrics, output) = self.run_measurements().await?;

        let (checks_passed, checks_output) = self.run_checks().await?;

        let metric = median(&mut samples.clone()).expect("measurement_runs was validated");
        Ok(Evaluation {
            measurement: AggregatedMeasurement {
                metric,
                samples,
                metrics: aggregated_metrics,
                warmup_runs: self.config.warmup_runs,
                measurement_runs: self.config.measurement_runs,
                duration_ms: started.elapsed().as_millis() as u64,
                checks_passed,
                output,
                checks_output,
            },
        })
    }

    async fn run_warmups(&self) -> Result<(), AutoresearchControllerError> {
        for _ in 0..self.config.warmup_runs {
            let result = self
                .run_shell(
                    &self.config.measure_command,
                    self.config.measure_timeout_seconds,
                )
                .await?;
            self.ensure_process_success(&result, "warmup measurement")?;
        }
        Ok(())
    }

    async fn run_measurements(
        &self,
    ) -> Result<(Vec<f64>, BTreeMap<String, f64>, String), AutoresearchControllerError> {
        let mut samples = Vec::with_capacity(self.config.measurement_runs);
        let mut metric_values: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        let mut output = String::new();
        for _ in 0..self.config.measurement_runs {
            let result = self
                .run_shell(
                    &self.config.measure_command,
                    self.config.measure_timeout_seconds,
                )
                .await?;
            self.ensure_process_success(&result, "measurement")?;
            let metrics = crate::autoresearch::parse_metrics(&result.output);
            let Some(metric) = metrics.get(&self.config.metric_name).copied() else {
                return Err(AutoresearchControllerError::Command(format!(
                    "measurement did not emit finite METRIC {}=...",
                    self.config.metric_name
                )));
            };
            samples.push(metric);
            for (name, value) in metrics {
                metric_values.entry(name).or_default().push(value);
            }
            output.push_str(&truncate_output(&result.output));
        }
        let aggregated_metrics = metric_values
            .into_iter()
            .map(|(name, mut values)| {
                (
                    name,
                    median(&mut values).expect("metric values are non-empty"),
                )
            })
            .collect();
        Ok((samples, aggregated_metrics, output))
    }

    async fn run_checks(&self) -> Result<(bool, String), AutoresearchControllerError> {
        let checks = self
            .run_shell(
                &self.config.checks_command,
                self.config.checks_timeout_seconds,
            )
            .await?;
        let checks_passed = !checks.timed_out && checks.exit_code == Some(0);
        let checks_output = truncate_output(&checks.output);
        Ok((checks_passed, checks_output))
    }

    fn ensure_process_success(
        &self,
        result: &ProcessResult,
        phase: &str,
    ) -> Result<(), AutoresearchControllerError> {
        if result.cancelled || self.cancellation.is_cancelled() {
            return Err(AutoresearchControllerError::Cancelled);
        }
        if result.timed_out {
            return Err(AutoresearchControllerError::Command(format!(
                "{phase} timed out after {}ms",
                result.duration_ms
            )));
        }
        if result.exit_code != Some(0) {
            return Err(AutoresearchControllerError::Command(format!(
                "{phase} exited with {:?}: {}",
                result.exit_code, result.output
            )));
        }
        Ok(())
    }

    async fn run_shell(
        &self,
        command: &str,
        timeout_seconds: u64,
    ) -> Result<ProcessResult, AutoresearchControllerError> {
        if self.cancellation.is_cancelled() {
            return Err(AutoresearchControllerError::Cancelled);
        }
        if let Some(sandbox) = &self.command_sandbox {
            sandbox
                .validate_path(&self.worktree, false)
                .map_err(|error| AutoresearchControllerError::Command(error.to_string()))?;
            sandbox
                .validate_command(command)
                .map_err(|error| AutoresearchControllerError::Command(error.to_string()))?;
        }
        let timeout = self.command_timeout(timeout_seconds)?;
        let mut shell_args = Vec::new();
        #[cfg(windows)]
        {
            shell_args.push("/C".to_string());
            shell_args.push(command.to_string());
            run_process(
                "cmd",
                &shell_args,
                &self.worktree,
                timeout,
                Some(&self.cancellation),
            )
            .await
        }
        #[cfg(not(windows))]
        {
            shell_args.push("-lc".to_string());
            shell_args.push(command.to_string());
            run_process(
                "bash",
                &shell_args,
                &self.worktree,
                timeout,
                Some(&self.cancellation),
            )
            .await
        }
    }

    fn command_timeout(
        &self,
        configured_seconds: u64,
    ) -> Result<Duration, AutoresearchControllerError> {
        let configured = Duration::from_secs(configured_seconds);
        let Some(max_seconds) = self.config.budget.max_duration_seconds else {
            return Ok(configured);
        };
        let elapsed = self.started_at.elapsed();
        let budget = Duration::from_secs(max_seconds);
        if elapsed >= budget {
            return Err(AutoresearchControllerError::BudgetExceeded {
                kind: BudgetKind::Duration,
            });
        }
        Ok(configured.min(budget - elapsed))
    }

    fn best_metric(&self) -> Option<f64> {
        self.baseline
            .as_ref()
            .map(|baseline| baseline.measurement.metric)
            .into_iter()
            .chain(
                self.iterations
                    .iter()
                    .filter(|iteration| iteration.status == IterationStatus::Accepted)
                    .filter_map(|iteration| iteration.metric),
            )
            .reduce(|best, candidate| match self.config.direction {
                MetricDirection::Lower => best.min(candidate),
                MetricDirection::Higher => best.max(candidate),
            })
    }

    fn check_budget(&self) -> Result<(), AutoresearchControllerError> {
        if self.cancellation.is_cancelled() {
            return Err(AutoresearchControllerError::Cancelled);
        }
        if self
            .config
            .budget
            .max_iterations
            .is_some_and(|max| self.iterations.len() >= max)
        {
            return Err(AutoresearchControllerError::BudgetExceeded {
                kind: BudgetKind::MaxIterations,
            });
        }
        if self
            .config
            .budget
            .max_duration_seconds
            .is_some_and(|max| self.started_at.elapsed() >= Duration::from_secs(max))
        {
            return Err(AutoresearchControllerError::BudgetExceeded {
                kind: BudgetKind::Duration,
            });
        }
        if self
            .config
            .budget
            .max_cost_usd
            .is_some_and(|max| self.total_cost_usd > max)
        {
            return Err(AutoresearchControllerError::BudgetExceeded {
                kind: BudgetKind::Cost,
            });
        }
        if let Some(max) = self.config.budget.max_disk_bytes {
            if directory_size(&self.session_dir)? + directory_size(&self.worktree)? > max {
                return Err(AutoresearchControllerError::BudgetExceeded {
                    kind: BudgetKind::Disk,
                });
            }
        }
        Ok(())
    }

    async fn restore_checkpoint(&self) -> Result<(), AutoresearchControllerError> {
        self.restore_commit(&self.accepted_commit).await
    }

    async fn restore_commit(&self, commit: &str) -> Result<(), AutoresearchControllerError> {
        let no_cancel = AutoresearchCancellation::new();
        let reset_args = vec![
            "reset".to_string(),
            "--hard".to_string(),
            commit.to_string(),
        ];
        let reset = git_at(
            &self.worktree,
            &reset_args,
            Duration::from_secs(60),
            Some(&no_cancel),
        )
        .await?;
        if reset.exit_code != Some(0) {
            return Err(AutoresearchControllerError::Git(reset.output));
        }
        let clean = git_at(
            &self.worktree,
            &["clean".to_string(), "-fdx".to_string()],
            Duration::from_secs(60),
            Some(&no_cancel),
        )
        .await?;
        if clean.exit_code != Some(0) {
            return Err(AutoresearchControllerError::Git(clean.output));
        }
        Ok(())
    }

    pub async fn final_patch(&self) -> Result<FinalPatch, AutoresearchControllerError> {
        let no_cancel = AutoresearchCancellation::new();
        let patch = git_output(
            &self.worktree,
            &[
                "diff",
                "--binary",
                &format!("{}..{}", self.base_commit, self.accepted_commit),
            ],
            &no_cancel,
        )
        .await?;
        let names = git_output(
            &self.worktree,
            &[
                "diff",
                "--name-only",
                &format!("{}..{}", self.base_commit, self.accepted_commit),
            ],
            &no_cancel,
        )
        .await?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect();
        Ok(FinalPatch {
            base_commit: self.base_commit.clone(),
            accepted_commit: self.accepted_commit.clone(),
            changed_files: names,
            patch,
        })
    }

    /// Apply the accepted patch to the user's real workspace. This is the
    /// only controller method that mutates that workspace, and it is an
    /// explicit host call. It refuses if HEAD or tracked/untracked state
    /// changed since controller creation.
    pub async fn accept_final_patch(&mut self) -> Result<FinalPatch, AutoresearchControllerError> {
        self.ensure_not_closed()?;
        let current_head = git_output(
            &self.root,
            &["rev-parse", "HEAD"],
            &AutoresearchCancellation::new(),
        )
        .await?;
        if current_head.trim() != self.base_commit {
            return Err(AutoresearchControllerError::Git(
                "real workspace HEAD changed since experiment start".into(),
            ));
        }
        let status = git_output(
            &self.root,
            &["status", "--porcelain=v1", "--untracked-files=all"],
            &AutoresearchCancellation::new(),
        )
        .await?;
        if !status.trim().is_empty() {
            return Err(AutoresearchControllerError::Git(
                "real workspace is no longer clean; refusing to apply patch".into(),
            ));
        }
        let final_patch = self.final_patch().await?;
        if final_patch.patch.is_empty() {
            if self.completion.is_none() {
                self.finish(CompletionReason::FinalPatchAccepted)?;
            }
            return Ok(final_patch);
        }
        let patch_path = self.session_dir.join("final.patch");
        std::fs::write(&patch_path, &final_patch.patch)?;
        let args = vec![
            "apply".to_string(),
            "--3way".to_string(),
            "--whitespace=nowarn".to_string(),
            patch_path.to_string_lossy().into_owned(),
        ];
        let result = git_at(
            &self.root,
            &args,
            Duration::from_secs(60),
            Some(&AutoresearchCancellation::new()),
        )
        .await?;
        if result.exit_code != Some(0) {
            return Err(AutoresearchControllerError::Git(result.output));
        }
        if self.completion.is_none() {
            self.finish(CompletionReason::FinalPatchAccepted)?;
        }
        Ok(final_patch)
    }

    pub fn complete(&mut self) -> Result<AutoresearchCompletion, AutoresearchControllerError> {
        self.ensure_open()?;
        self.finish(CompletionReason::Explicit)
    }

    /// Remove the detached worktree while preserving the append-only session
    /// directory and its logs.
    pub async fn close(&mut self) -> Result<(), AutoresearchControllerError> {
        if self.closed {
            return Ok(());
        }
        remove_worktree(&self.root, &self.worktree).await?;
        self.closed = true;
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), AutoresearchControllerError> {
        self.ensure_not_closed()?;
        if self.completion.is_some() {
            return Err(AutoresearchControllerError::Completed);
        }
        Ok(())
    }

    fn ensure_not_closed(&self) -> Result<(), AutoresearchControllerError> {
        if self.closed {
            return Err(AutoresearchControllerError::NotReady(
                "controller worktree has been closed".into(),
            ));
        }
        Ok(())
    }

    fn emit(&mut self, event: AutoresearchEvent) -> Result<(), AutoresearchControllerError> {
        append_json_line(&self.events_path, &event)?;
        self.events.push(event.clone());
        for subscriber in &self.subscribers {
            subscriber(&event);
        }
        Ok(())
    }

    fn finish(
        &mut self,
        reason: CompletionReason,
    ) -> Result<AutoresearchCompletion, AutoresearchControllerError> {
        if let Some(completion) = &self.completion {
            return Ok(completion.clone());
        }
        let completion = AutoresearchCompletion {
            reason,
            iterations: self.iterations.len(),
            accepted_iterations: self
                .iterations
                .iter()
                .filter(|iteration| iteration.status == IterationStatus::Accepted)
                .count(),
            best_metric: self.best_metric(),
            accepted_commit: self.accepted_commit.clone(),
            total_cost_usd: self.total_cost_usd,
        };
        self.completion = Some(completion.clone());
        self.emit(AutoresearchEvent::Completed {
            completion: completion.clone(),
        })?;
        Ok(completion)
    }

    fn finish_for_error(
        &mut self,
        error: &AutoresearchControllerError,
    ) -> Result<(), AutoresearchControllerError> {
        let reason = completion_for_error(error);
        self.finish(reason).map(|_| ())
    }
}

impl Drop for AutoresearchController {
    fn drop(&mut self) {
        // Async Git worktree removal cannot run from Drop. The session log is
        // intentionally retained; hosts should call close() at lifecycle end.
    }
}

fn completion_for_error(error: &AutoresearchControllerError) -> CompletionReason {
    match error {
        AutoresearchControllerError::Cancelled => CompletionReason::Cancelled,
        AutoresearchControllerError::BudgetExceeded { kind } => match kind {
            BudgetKind::MaxIterations => CompletionReason::MaxIterations,
            BudgetKind::Duration => CompletionReason::DurationBudget,
            BudgetKind::Cost => CompletionReason::CostBudget,
            BudgetKind::Disk => CompletionReason::DiskBudget,
        },
        _ => CompletionReason::Failed,
    }
}

fn improvement(direction: MetricDirection, best: f64, candidate: f64) -> f64 {
    match direction {
        MetricDirection::Lower => best - candidate,
        MetricDirection::Higher => candidate - best,
    }
}

fn is_improvement(direction: MetricDirection, best: f64, candidate: f64, threshold: f64) -> bool {
    let improvement = improvement(direction, best, candidate);
    improvement > 0.0 && improvement > threshold
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).expect("finite values are comparable"));
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[middle])
    } else {
        Some((values[middle - 1] + values[middle]) / 2.0)
    }
}

async fn run_process(
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
    cancellation: Option<&AutoresearchCancellation>,
) -> Result<ProcessResult, AutoresearchControllerError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| AutoresearchControllerError::Command(error.to_string()))?;
    let started = Instant::now();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AutoresearchControllerError::Command("stdout was not piped".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AutoresearchControllerError::Command("stderr was not piped".into()))?;
    let collect = async move {
        let stdout = collect_limited(stdout);
        let stderr = collect_limited(stderr);
        let (stdout, stderr, status) = tokio::join!(stdout, stderr, child.wait());
        (stdout, stderr, status)
    };
    tokio::pin!(collect);
    let result = if let Some(cancellation) = cancellation {
        tokio::select! {
            result = &mut collect => process_output(result, started, false, false),
            _ = tokio::time::sleep(timeout) => ProcessResult {
                output: "command timed out".into(),
                exit_code: None,
                timed_out: true,
                cancelled: false,
                duration_ms: started.elapsed().as_millis() as u64,
            },
            _ = cancellation.cancelled() => ProcessResult {
                output: "command cancelled".into(),
                exit_code: None,
                timed_out: false,
                cancelled: true,
                duration_ms: started.elapsed().as_millis() as u64,
            },
        }
    } else {
        tokio::select! {
            result = &mut collect => process_output(result, started, false, false),
            _ = tokio::time::sleep(timeout) => ProcessResult {
                output: "command timed out".into(),
                exit_code: None,
                timed_out: true,
                cancelled: false,
                duration_ms: started.elapsed().as_millis() as u64,
            },
        }
    };
    Ok(result)
}

async fn collect_limited<R: AsyncRead + Unpin>(mut reader: R) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let remaining = MAX_COMMAND_OUTPUT_BYTES.saturating_sub(output.len());
                output.extend_from_slice(&buffer[..read.min(remaining)]);
            }
        }
    }
    output
}

fn process_output(
    result: (Vec<u8>, Vec<u8>, std::io::Result<std::process::ExitStatus>),
    started: Instant,
    timed_out: bool,
    cancelled: bool,
) -> ProcessResult {
    let (stdout, stderr, status) = result;
    let mut combined = stdout;
    if !stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with(b"\n") {
            combined.push(b'\n');
        }
        combined.extend(stderr);
    }
    let output = String::from_utf8_lossy(&combined).into_owned();
    ProcessResult {
        output,
        exit_code: status.ok().and_then(|status| status.code()),
        timed_out,
        cancelled,
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

async fn git_at(
    cwd: &Path,
    args: &[String],
    timeout: Duration,
    cancellation: Option<&AutoresearchCancellation>,
) -> Result<ProcessResult, AutoresearchControllerError> {
    run_process("git", args, cwd, timeout, cancellation).await
}

async fn git_output(
    cwd: &Path,
    args: &[&str],
    cancellation: &AutoresearchCancellation,
) -> Result<String, AutoresearchControllerError> {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = git_at(cwd, &args, Duration::from_secs(60), Some(cancellation)).await?;
    if result.cancelled {
        return Err(AutoresearchControllerError::Cancelled);
    }
    if result.timed_out || result.exit_code != Some(0) {
        return Err(AutoresearchControllerError::Git(result.output));
    }
    Ok(result.output)
}

async fn remove_worktree(root: &Path, worktree: &Path) -> Result<(), AutoresearchControllerError> {
    if !worktree.exists() {
        return Ok(());
    }
    let args = vec![
        "worktree".to_string(),
        "remove".to_string(),
        "--force".to_string(),
        worktree.to_string_lossy().into_owned(),
    ];
    let result = git_at(
        root,
        &args,
        Duration::from_secs(60),
        Some(&AutoresearchCancellation::new()),
    )
    .await?;
    if result.exit_code != Some(0) {
        return Err(AutoresearchControllerError::Git(result.output));
    }
    Ok(())
}

fn append_json_line<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), AutoresearchControllerError> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    file.sync_data()?;
    Ok(())
}

fn truncate_output(output: &str) -> String {
    if output.len() <= MAX_COMMAND_OUTPUT_BYTES {
        return output.to_string();
    }
    let mut start = output.len() - MAX_COMMAND_OUTPUT_BYTES;
    while start < output.len() && !output.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &output[start..])
}

fn directory_size(path: &Path) -> Result<u64, std::io::Error> {
    if !path.exists() {
        return Ok(0);
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    let mut total = 0_u64;
    for entry in std::fs::read_dir(path)? {
        total = total.saturating_add(directory_size(&entry?.path())?);
    }
    Ok(total)
}

fn create_private_dir(path: &Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(path)?;
    set_private_permissions(path)
}

fn set_private_permissions(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_handles_even_and_odd_noisy_samples() {
        let mut odd = [10.0, 2.0, 8.0];
        assert_eq!(median(&mut odd), Some(8.0));
        let mut even = [10.0, 2.0, 8.0, 4.0];
        assert_eq!(median(&mut even), Some(6.0));
    }

    #[test]
    fn threshold_is_absolute_and_direction_aware() {
        assert!(is_improvement(MetricDirection::Lower, 10.0, 7.0, 2.0));
        assert!(!is_improvement(MetricDirection::Lower, 10.0, 8.0, 2.0));
        assert!(!is_improvement(MetricDirection::Lower, 10.0, 9.0, 2.0));
        assert!(is_improvement(MetricDirection::Higher, 10.0, 13.0, 2.0));
        assert!(!is_improvement(MetricDirection::Higher, 10.0, 12.0, 2.0));
        assert!(!is_improvement(MetricDirection::Higher, 10.0, 11.0, 2.0));
    }

    #[test]
    fn cancellation_is_observable() {
        let cancellation = AutoresearchCancellation::new();
        assert!(!cancellation.is_cancelled());
        cancellation.cancel();
        assert!(cancellation.is_cancelled());
    }
}
