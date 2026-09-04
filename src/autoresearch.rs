//! Opt-in autonomous experiment loops.
//!
//! The core loop is deliberately domain agnostic: a host supplies a
//! measurement command, the command emits `METRIC name=value` lines, and the
//! session records whether the result improved over the best accepted run.
//! Session state lives under `.auto/` so a host can resume after a restart or
//! context compaction.  The engine reports keep/discard decisions; committing
//! or reverting source changes remains a host policy decision.

use crate::agent::{ToolContext, ToolDefinition, ToolEffect, ToolRegistry, ToolResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::process::Command;
use tokio::sync::Mutex;

const AUTO_DIR: &str = ".auto";
const MAX_CAPTURE_BYTES: usize = 4 * 1024;
const MAX_CAPTURE_LINES: usize = 10;
const DEFAULT_TIMEOUT_SECONDS: u64 = 600;
const DEFAULT_CHECKS_TIMEOUT_SECONDS: u64 = 300;

/// Whether a larger or smaller metric is an improvement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricDirection {
    #[default]
    Lower,
    Higher,
}

/// Configuration for one autoresearch session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutoresearchConfig {
    pub name: String,
    pub metric_name: String,
    #[serde(default)]
    pub metric_unit: String,
    #[serde(default)]
    pub direction: MetricDirection,
    pub measure_command: String,
    #[serde(default)]
    pub checks_command: Option<String>,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_checks_timeout_seconds")]
    pub checks_timeout_seconds: u64,
    /// Maximum number of measurements. `None` means the host controls the
    /// lifecycle and the session may continue indefinitely.
    #[serde(default)]
    pub max_iterations: Option<usize>,
}

fn default_timeout_seconds() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

fn default_checks_timeout_seconds() -> u64 {
    DEFAULT_CHECKS_TIMEOUT_SECONDS
}

impl AutoresearchConfig {
    /// Create a session configuration with conservative command timeouts.
    pub fn new(
        name: impl Into<String>,
        metric_name: impl Into<String>,
        measure_command: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            metric_name: metric_name.into(),
            metric_unit: String::new(),
            direction: MetricDirection::Lower,
            measure_command: measure_command.into(),
            checks_command: None,
            timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
            checks_timeout_seconds: DEFAULT_CHECKS_TIMEOUT_SECONDS,
            max_iterations: None,
        }
    }

    fn validate(&self) -> Result<(), AutoresearchError> {
        if self.name.trim().is_empty() {
            return Err(AutoresearchError::InvalidConfig(
                "name must not be empty".into(),
            ));
        }
        if self.metric_name.trim().is_empty() {
            return Err(AutoresearchError::InvalidConfig(
                "metric_name must not be empty".into(),
            ));
        }
        if self.measure_command.trim().is_empty() {
            return Err(AutoresearchError::InvalidConfig(
                "measure_command must not be empty".into(),
            ));
        }
        if self.timeout_seconds == 0 {
            return Err(AutoresearchError::InvalidConfig(
                "timeout_seconds must be greater than zero".into(),
            ));
        }
        if self.checks_command.is_some() && self.checks_timeout_seconds == 0 {
            return Err(AutoresearchError::InvalidConfig(
                "checks_timeout_seconds must be greater than zero".into(),
            ));
        }
        if self.max_iterations == Some(0) {
            return Err(AutoresearchError::InvalidConfig(
                "max_iterations must be greater than zero when set".into(),
            ));
        }
        Ok(())
    }
}

/// Outcome recorded for a measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentStatus {
    Keep,
    Discard,
    Crash,
    ChecksFailed,
}

/// A measurement supplied by a host or produced by [`AutoresearchSession`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExperimentMeasurement {
    pub metric: Option<f64>,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    #[serde(default)]
    pub output: String,
    pub checks_passed: Option<bool>,
    #[serde(default)]
    pub checks_output: String,
}

/// One append-only session log entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExperimentRun {
    pub run: usize,
    pub commit: Option<String>,
    pub metric: Option<f64>,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    pub status: ExperimentStatus,
    pub description: String,
    pub timestamp: DateTime<Utc>,
    pub duration_ms: u64,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub checks_output: String,
}

/// Errors from session setup, persistence, or command execution.
#[derive(Debug, thiserror::Error)]
pub enum AutoresearchError {
    #[error("invalid autoresearch configuration: {0}")]
    InvalidConfig(String),
    #[error("autoresearch io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("autoresearch json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("autoresearch log line {line} is invalid: {message}")]
    InvalidLog { line: usize, message: String },
    #[error("autoresearch iteration limit reached: {0}")]
    MaxIterations(usize),
    #[error("autoresearch command could not start: {0}")]
    Command(String),
}

/// A resumable experiment session rooted at a workspace.
pub struct AutoresearchSession {
    root: PathBuf,
    config: AutoresearchConfig,
    runs: Vec<ExperimentRun>,
}

impl AutoresearchSession {
    /// Initialize a session and create its `.auto/` files if they do not yet
    /// exist. Existing logs are preserved so initialization is resumable.
    pub fn init(
        root: impl Into<PathBuf>,
        config: AutoresearchConfig,
    ) -> Result<Self, AutoresearchError> {
        config.validate()?;
        let root = root.into();
        if !root.is_dir() {
            return Err(AutoresearchError::InvalidConfig(format!(
                "workspace is not a directory: {}",
                root.display()
            )));
        }
        let auto_dir = root.join(AUTO_DIR);
        std::fs::create_dir_all(&auto_dir)?;
        std::fs::write(
            auto_dir.join("config.json"),
            serde_json::to_vec_pretty(&config)?,
        )?;
        write_if_missing(&auto_dir.join("prompt.md"), &prompt_template(&config))?;
        write_if_missing(
            &auto_dir.join("measure.sh"),
            &script_template(&config.measure_command),
        )?;
        if let Some(checks) = &config.checks_command {
            write_if_missing(&auto_dir.join("checks.sh"), &script_template(checks))?;
        }

        let runs = load_runs(&auto_dir.join("log.jsonl"))?;
        Ok(Self { root, config, runs })
    }

    /// Reopen a previously initialized session.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, AutoresearchError> {
        let root = root.into();
        let auto_dir = root.join(AUTO_DIR);
        let config: AutoresearchConfig =
            serde_json::from_slice(&std::fs::read(auto_dir.join("config.json"))?)?;
        config.validate()?;
        let runs = load_runs(&auto_dir.join("log.jsonl"))?;
        Ok(Self { root, config, runs })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> &AutoresearchConfig {
        &self.config
    }

    pub fn runs(&self) -> &[ExperimentRun] {
        &self.runs
    }

    pub fn run_count(&self) -> usize {
        self.runs.len()
    }

    /// Best accepted metric in the current log.
    pub fn best_metric(&self) -> Option<f64> {
        self.runs
            .iter()
            .filter(|run| run.status == ExperimentStatus::Keep)
            .filter_map(|run| run.metric)
            .reduce(|best, metric| match self.config.direction {
                MetricDirection::Lower => best.min(metric),
                MetricDirection::Higher => best.max(metric),
            })
    }

    /// Execute the configured measurement and record the resulting decision.
    /// The command's output is intentionally truncated before it is persisted
    /// or returned so repeated runs do not inflate agent context.
    pub async fn run_experiment(
        &mut self,
        description: impl Into<String>,
    ) -> Result<ExperimentRun, AutoresearchError> {
        self.ensure_capacity()?;
        let measurement = run_command(
            &self.root,
            &session_script_command("measure.sh"),
            self.config.timeout_seconds,
        )
        .await?;

        let parsed = parse_metrics(&measurement.output);
        let metric = parsed.get(&self.config.metric_name).copied();
        let mut checks_output = String::new();
        let checks_passed =
            if measurement.exit_code == Some(0) && !measurement.timed_out && metric.is_some() {
                if self.config.checks_command.is_some() {
                    let checks = run_command(
                        &self.root,
                        &session_script_command("checks.sh"),
                        self.config.checks_timeout_seconds,
                    )
                    .await?;
                    checks_output = checks.output;
                    Some(checks.exit_code == Some(0) && !checks.timed_out)
                } else {
                    None
                }
            } else {
                None
            };

        let measurement = ExperimentMeasurement {
            metric,
            metrics: parsed,
            duration_ms: measurement.duration_ms,
            exit_code: measurement.exit_code,
            timed_out: measurement.timed_out,
            output: measurement.output,
            checks_passed,
            checks_output,
        };
        let commit = current_commit(&self.root).await;
        self.record_measurement(description, measurement, commit)
    }

    /// Record a measurement obtained by a host-owned runner.
    pub fn record_measurement(
        &mut self,
        description: impl Into<String>,
        measurement: ExperimentMeasurement,
        commit: Option<String>,
    ) -> Result<ExperimentRun, AutoresearchError> {
        self.ensure_capacity()?;
        let status = if measurement.exit_code != Some(0) || measurement.timed_out {
            ExperimentStatus::Crash
        } else {
            match measurement.metric {
                None => ExperimentStatus::Crash,
                Some(_) if measurement.checks_passed == Some(false) => {
                    ExperimentStatus::ChecksFailed
                }
                Some(metric) if self.is_improvement(metric) => ExperimentStatus::Keep,
                Some(_) => ExperimentStatus::Discard,
            }
        };
        let run = ExperimentRun {
            run: self.runs.len() + 1,
            commit,
            metric: measurement.metric,
            metrics: measurement.metrics,
            status,
            description: description.into(),
            timestamp: Utc::now(),
            duration_ms: measurement.duration_ms,
            output: truncate_tail(&measurement.output),
            checks_output: truncate_tail(&measurement.checks_output),
        };
        append_run(&self.root.join(AUTO_DIR).join("log.jsonl"), &run)?;
        self.runs.push(run.clone());
        Ok(run)
    }

    /// Record a metric supplied by a host or an external benchmark runner.
    pub fn record_metric(
        &mut self,
        description: impl Into<String>,
        metric: f64,
        mut metrics: BTreeMap<String, f64>,
        commit: Option<String>,
    ) -> Result<ExperimentRun, AutoresearchError> {
        if !metric.is_finite() {
            return Err(AutoresearchError::InvalidConfig(
                "metric must be finite".into(),
            ));
        }
        metrics.insert(self.config.metric_name.clone(), metric);
        self.record_measurement(
            description,
            ExperimentMeasurement {
                metric: Some(metric),
                metrics,
                duration_ms: 0,
                exit_code: Some(0),
                timed_out: false,
                output: String::new(),
                checks_passed: None,
                checks_output: String::new(),
            },
            commit,
        )
    }

    fn ensure_capacity(&self) -> Result<(), AutoresearchError> {
        if let Some(max) = self.config.max_iterations {
            if self.runs.len() >= max {
                return Err(AutoresearchError::MaxIterations(max));
            }
        }
        Ok(())
    }

    fn is_improvement(&self, metric: f64) -> bool {
        let Some(best) = self.best_metric() else {
            return true;
        };
        match self.config.direction {
            MetricDirection::Lower => metric < best,
            MetricDirection::Higher => metric > best,
        }
    }
}

/// Shared state for opt-in autoresearch tools.
pub type AutoresearchHandle = Arc<Mutex<Option<AutoresearchSession>>>;

pub fn new_handle() -> AutoresearchHandle {
    Arc::new(Mutex::new(None))
}

/// Register the autoresearch tools without adding them to rotary's default
/// tool loadout. Hosts can opt in when a measurable optimization task exists.
pub fn register_tools(registry: &ToolRegistry, handle: AutoresearchHandle) {
    let init_handle = Arc::clone(&handle);
    registry.register(
        ToolDefinition::new_boxed(
            "init_experiment",
            "Initialize an opt-in autoresearch session under .auto/.",
            r#"{"type":"object","properties":{"name":{"type":"string"},"metric_name":{"type":"string"},"metric_unit":{"type":"string"},"direction":{"type":"string","enum":["lower","higher"]},"measure_command":{"type":"string"},"checks_command":{"type":"string"},"timeout_seconds":{"type":"integer"},"checks_timeout_seconds":{"type":"integer"},"max_iterations":{"type":"integer"}},"required":["name","metric_name","measure_command"]}"#,
            Box::new(move |ctx, args| {
                let handle = Arc::clone(&init_handle);
                Box::pin(async move { execute_init(handle, ctx, args).await })
            }),
        )
        .with_effect(ToolEffect::Write),
    );

    let run_handle = Arc::clone(&handle);
    registry.register(
        ToolDefinition::new_boxed(
            "run_experiment",
            "Run the configured autoresearch measurement, parse METRIC lines, and record keep/discard status.",
            r#"{"type":"object","properties":{"description":{"type":"string"}},"required":["description"]}"#,
            Box::new(move |_ctx, args| {
                let handle = Arc::clone(&run_handle);
                Box::pin(async move { execute_run(handle, args).await })
            }),
        )
        .with_effect(ToolEffect::Process),
    );

    let log_handle = Arc::clone(&handle);
    registry.register(
        ToolDefinition::new_boxed(
            "log_experiment",
            "Record a host-provided autoresearch metric and let the session classify it against the best run.",
            r#"{"type":"object","properties":{"description":{"type":"string"},"metric":{"type":"number"},"metrics":{"type":"object"},"commit":{"type":"string"}},"required":["description","metric"]}"#,
            Box::new(move |_ctx, args| {
                let handle = Arc::clone(&log_handle);
                Box::pin(async move { execute_log(handle, args).await })
            }),
        )
        .with_effect(ToolEffect::Write),
    );

    registry.register(
        ToolDefinition::new_boxed(
            "autoresearch_status",
            "Show the current autoresearch metric, best run, and recent results.",
            r#"{"type":"object","properties":{}}"#,
            Box::new(move |_ctx, _args| {
                let handle = Arc::clone(&handle);
                Box::pin(async move { execute_status(handle).await })
            }),
        )
        .with_effect(ToolEffect::Read),
    );
}

#[derive(Debug, Deserialize)]
struct InitArgs {
    name: String,
    metric_name: String,
    #[serde(default)]
    metric_unit: String,
    #[serde(default)]
    direction: MetricDirection,
    measure_command: String,
    #[serde(default)]
    checks_command: Option<String>,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
    #[serde(default = "default_checks_timeout_seconds")]
    checks_timeout_seconds: u64,
    #[serde(default)]
    max_iterations: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RunArgs {
    description: String,
}

#[derive(Debug, Deserialize)]
struct LogArgs {
    description: String,
    metric: f64,
    #[serde(default)]
    metrics: BTreeMap<String, f64>,
    #[serde(default)]
    commit: Option<String>,
}

async fn execute_init(
    handle: AutoresearchHandle,
    ctx: Arc<ToolContext>,
    args: String,
) -> ToolResult {
    let args: InitArgs = match serde_json::from_str(&args) {
        Ok(args) => args,
        Err(error) => return ToolResult::err("init_experiment", format!("invalid json: {error}")),
    };
    let config = AutoresearchConfig {
        name: args.name,
        metric_name: args.metric_name,
        metric_unit: args.metric_unit,
        direction: args.direction,
        measure_command: args.measure_command,
        checks_command: args.checks_command,
        timeout_seconds: args.timeout_seconds,
        checks_timeout_seconds: args.checks_timeout_seconds,
        max_iterations: args.max_iterations,
    };
    match AutoresearchSession::init(&ctx.workspace_root, config) {
        Ok(session) => {
            let status = session_status(&session);
            *handle.lock().await = Some(session);
            ToolResult::ok("init_experiment", status)
        }
        Err(error) => ToolResult::err("init_experiment", error.to_string()),
    }
}

async fn execute_run(handle: AutoresearchHandle, args: String) -> ToolResult {
    let args: RunArgs = match serde_json::from_str(&args) {
        Ok(args) => args,
        Err(error) => return ToolResult::err("run_experiment", format!("invalid json: {error}")),
    };
    let mut guard = handle.lock().await;
    let Some(session) = guard.as_mut() else {
        return ToolResult::err(
            "run_experiment",
            "no autoresearch session; call init_experiment first",
        );
    };
    match session.run_experiment(args.description).await {
        Ok(run) => tool_json("run_experiment", &run),
        Err(error) => ToolResult::err("run_experiment", error.to_string()),
    }
}

async fn execute_log(handle: AutoresearchHandle, args: String) -> ToolResult {
    let args: LogArgs = match serde_json::from_str(&args) {
        Ok(args) => args,
        Err(error) => return ToolResult::err("log_experiment", format!("invalid json: {error}")),
    };
    let mut guard = handle.lock().await;
    let Some(session) = guard.as_mut() else {
        return ToolResult::err(
            "log_experiment",
            "no autoresearch session; call init_experiment first",
        );
    };
    match session.record_metric(args.description, args.metric, args.metrics, args.commit) {
        Ok(run) => tool_json("log_experiment", &run),
        Err(error) => ToolResult::err("log_experiment", error.to_string()),
    }
}

async fn execute_status(handle: AutoresearchHandle) -> ToolResult {
    let guard = handle.lock().await;
    let Some(session) = guard.as_ref() else {
        return ToolResult::ok("autoresearch_status", r#"{"active":false}"#);
    };
    ToolResult::ok("autoresearch_status", session_status(session))
}

fn tool_json<T: Serialize>(id: &str, value: &T) -> ToolResult {
    match serde_json::to_string(value) {
        Ok(json) => ToolResult::ok(id, json),
        Err(error) => ToolResult::err(id, error.to_string()),
    }
}

fn session_status(session: &AutoresearchSession) -> String {
    serde_json::json!({
        "active": true,
        "name": session.config.name,
        "metric_name": session.config.metric_name,
        "metric_unit": session.config.metric_unit,
        "direction": session.config.direction,
        "run_count": session.run_count(),
        "best_metric": session.best_metric(),
        "recent_runs": session.runs().iter().rev().take(5).collect::<Vec<_>>(),
    })
    .to_string()
}

#[derive(Debug)]
struct CommandOutput {
    output: String,
    exit_code: Option<i32>,
    timed_out: bool,
    duration_ms: u64,
}

async fn run_command(
    root: &Path,
    command: &str,
    timeout_seconds: u64,
) -> Result<CommandOutput, AutoresearchError> {
    let child = shell_command(command, root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| AutoresearchError::Command(error.to_string()))?;
    let started = Instant::now();
    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_seconds),
        child.wait_with_output(),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            return Ok(CommandOutput {
                output: "command timed out".into(),
                exit_code: None,
                timed_out: true,
                duration_ms: started.elapsed().as_millis() as u64,
            });
        }
    };
    let mut combined = output.stdout;
    if !output.stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with(b"\n") {
            combined.push(b'\n');
        }
        combined.extend(output.stderr);
    }
    Ok(CommandOutput {
        output: String::from_utf8_lossy(&combined).into_owned(),
        exit_code: output.status.code(),
        timed_out: false,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

fn shell_command(command: &str, root: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]).current_dir(root);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("bash");
        cmd.args(["-lc", command]).current_dir(root);
        cmd
    }
}

async fn current_commit(root: &Path) -> Option<String> {
    let result = run_command(root, "git rev-parse --short HEAD", 5)
        .await
        .ok()?;
    if result.exit_code != Some(0) || result.timed_out {
        return None;
    }
    let commit = result.output.trim().to_string();
    (!commit.is_empty()).then_some(commit)
}

/// Parse structured metric lines. The last finite value wins for duplicate
/// names, matching the useful behavior of benchmark scripts that refine a
/// measurement after a warm-up pass.
pub fn parse_metrics(output: &str) -> BTreeMap<String, f64> {
    let mut metrics = BTreeMap::new();
    for line in output.lines() {
        let Some(rest) = line.strip_prefix("METRIC ") else {
            continue;
        };
        let Some((name, value)) = rest.trim().split_once('=') else {
            continue;
        };
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'))
            || matches!(name, "__proto__" | "constructor" | "prototype")
        {
            continue;
        }
        if let Ok(value) = value.trim().parse::<f64>() {
            if value.is_finite() {
                metrics.insert(name.to_string(), value);
            }
        }
    }
    metrics
}

fn write_if_missing(path: &Path, contents: &str) -> Result<(), AutoresearchError> {
    if !path.exists() {
        std::fs::write(path, contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(path)?.permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions)?;
        }
    }
    Ok(())
}

fn prompt_template(config: &AutoresearchConfig) -> String {
    let direction = match config.direction {
        MetricDirection::Lower => "lower",
        MetricDirection::Higher => "higher",
    };
    format!(
        "# Autoresearch: {}\n\n## Objective\nOptimize the workload while preserving correctness.\n\n## Metrics\n- **Primary**: {} ({}, {} is better)\n\n## How to Run\n`./.auto/measure.sh` — emits `METRIC name=value` lines.\n\n## Files in Scope\nDescribe the files the host may modify before starting experiments.\n\n## Constraints\nRun the checks before accepting a change. Keep the context and each experiment focused.\n\n## What's Been Tried\nThe session log is in `.auto/log.jsonl`; add durable findings here as the loop learns.\n",
        config.name, config.metric_name, config.metric_unit, direction
    )
}

fn script_template(command: &str) -> String {
    format!("#!/usr/bin/env bash\nset -euo pipefail\n\n{command}\n")
}

fn session_script_command(name: &str) -> String {
    #[cfg(windows)]
    {
        format!("bash .auto/{name}")
    }
    #[cfg(not(windows))]
    {
        format!("./.auto/{name}")
    }
}

fn append_run(path: &Path, run: &ExperimentRun) -> Result<(), AutoresearchError> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    serde_json::to_writer(&mut file, run)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn load_runs(path: &Path) -> Result<Vec<ExperimentRun>, AutoresearchError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = std::fs::read_to_string(path)?;
    let mut runs = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        runs.push(
            serde_json::from_str(line).map_err(|error| AutoresearchError::InvalidLog {
                line: index + 1,
                message: error.to_string(),
            })?,
        );
    }
    Ok(runs)
}

fn truncate_tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().rev().take(MAX_CAPTURE_LINES).collect();
    let mut result = lines.into_iter().rev().collect::<Vec<_>>().join("\n");
    if text.ends_with('\n') {
        result.push('\n');
    }
    if result.len() > MAX_CAPTURE_BYTES {
        let mut start = result.len() - MAX_CAPTURE_BYTES;
        while start < result.len() && !result.is_char_boundary(start) {
            start += 1;
        }
        result = format!("…{}", &result[start..]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_finite_metrics_and_last_duplicate_wins() {
        let metrics = parse_metrics(
            "noise\nMETRIC total_ms=10\nMETRIC total_ms=8.5\nMETRIC phase_a=2\nMETRIC bad=NaN\n",
        );
        assert_eq!(metrics.get("total_ms"), Some(&8.5));
        assert_eq!(metrics.get("phase_a"), Some(&2.0));
        assert!(!metrics.contains_key("bad"));
    }

    #[test]
    fn lower_and_higher_directions_classify_runs() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = AutoresearchConfig::new("speed", "ms", "echo METRIC ms=1");
        config.max_iterations = Some(3);
        let mut session = AutoresearchSession::init(dir.path(), config).unwrap();
        let first = session
            .record_metric("baseline", 10.0, BTreeMap::new(), None)
            .unwrap();
        let second = session
            .record_metric("faster", 9.0, BTreeMap::new(), None)
            .unwrap();
        assert_eq!(first.status, ExperimentStatus::Keep);
        assert_eq!(second.status, ExperimentStatus::Keep);
        assert_eq!(session.best_metric(), Some(9.0));

        let higher_dir = tempfile::tempdir().unwrap();
        let mut higher_config = AutoresearchConfig::new("quality", "score", "echo METRIC score=1");
        higher_config.direction = MetricDirection::Higher;
        let mut higher_session =
            AutoresearchSession::init(higher_dir.path(), higher_config).unwrap();
        higher_session
            .record_metric("baseline", 9.0, BTreeMap::new(), None)
            .unwrap();
        let third = higher_session
            .record_metric("higher", 10.0, BTreeMap::new(), None)
            .unwrap();
        assert_eq!(third.status, ExperimentStatus::Keep);
    }

    #[test]
    fn sessions_resume_from_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let config = AutoresearchConfig::new("speed", "ms", "echo METRIC ms=1");
        let mut session = AutoresearchSession::init(dir.path(), config.clone()).unwrap();
        session
            .record_metric("baseline", 10.0, BTreeMap::new(), Some("abc123".into()))
            .unwrap();
        let reopened = AutoresearchSession::open(dir.path()).unwrap();
        assert_eq!(reopened.config(), &config);
        assert_eq!(reopened.run_count(), 1);
        assert_eq!(reopened.best_metric(), Some(10.0));
        assert_eq!(reopened.runs()[0].commit.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn runs_command_and_records_metric() {
        let dir = tempfile::tempdir().unwrap();
        let config = AutoresearchConfig::new(
            "speed",
            "total_ms",
            "printf 'noise\\nMETRIC total_ms=7\\nMETRIC phase_ms=3\\n'",
        );
        let mut session = AutoresearchSession::init(dir.path(), config).unwrap();
        let run = session.run_experiment("baseline").await.unwrap();
        assert_eq!(run.status, ExperimentStatus::Keep);
        assert_eq!(run.metric, Some(7.0));
        assert_eq!(run.metrics.get("phase_ms"), Some(&3.0));
        assert_eq!(run.metrics.get("total_ms"), Some(&7.0));
    }

    #[tokio::test]
    async fn failed_checks_are_recorded_without_accepting_the_metric() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = AutoresearchConfig::new("speed", "ms", "printf 'METRIC ms=7\\n'");
        config.checks_command = Some("exit 1".into());
        let mut session = AutoresearchSession::init(dir.path(), config).unwrap();
        let run = session.run_experiment("broken checks").await.unwrap();
        assert_eq!(run.status, ExperimentStatus::ChecksFailed);
        assert_eq!(session.best_metric(), None);
    }

    #[test]
    fn max_iterations_is_host_visible() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = AutoresearchConfig::new("speed", "ms", "echo METRIC ms=1");
        config.max_iterations = Some(1);
        let mut session = AutoresearchSession::init(dir.path(), config).unwrap();
        session
            .record_metric("baseline", 1.0, BTreeMap::new(), None)
            .unwrap();
        assert!(matches!(
            session.record_metric("next", 0.5, BTreeMap::new(), None),
            Err(AutoresearchError::MaxIterations(1))
        ));
    }
}
