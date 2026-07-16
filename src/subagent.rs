//! Subagent: isolated agent execution contexts with optional git worktree
//! isolation.
//!
//! A subagent is a lightweight, independently-scoped agent run spawned by a
//! parent agent or host. Each subagent has its own config (model, tools,
//! permissions) and produces a [`SubagentResult`] when it completes.
//!
//! The execution itself is mocked in this implementation — `spawn` records the
//! subagent and immediately transitions it to [`SubagentStatus::Completed`]
//! with a stub result. A real implementation would wire into `Agent::prompt`
//! and drive the loop to completion on a background task.

use crate::permissions::PermissionMode;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

/// Errors raised by subagent lifecycle operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SubagentError {
    #[error("subagent spawn failed: {0}")]
    SpawnFailed(String),
    #[error("subagent not found: {0}")]
    NotFound(String),
    #[error("subagent already running: {0}")]
    AlreadyRunning(String),
    #[error("git error: {0}")]
    GitError(String),
}

/// Lifecycle state of a subagent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    /// Created but not yet started.
    Pending,
    /// Currently executing.
    Running,
    /// Finished successfully.
    Completed,
    /// Terminated with an error.
    Failed,
    /// Cancelled by the parent.
    Cancelled,
}

/// Declarative configuration for a subagent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentConfig {
    /// Human-readable name for the subagent.
    pub name: String,
    /// Optional system prompt override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Optional model override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Maximum agent loop steps before stopping.
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,
    /// Tool allowlist — if set, only these tools are available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    /// Tool denylist — these tools are always unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied_tools: Option<Vec<String>>,
    /// Permission mode override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    /// When true, the subagent runs inside a git worktree at
    /// `.rx4/worktrees/{id}` instead of the parent workspace.
    #[serde(default)]
    pub workspace_isolation: bool,
}

fn default_max_steps() -> usize {
    25
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            name: "subagent".to_string(),
            system_prompt: None,
            model: None,
            max_steps: default_max_steps(),
            allowed_tools: None,
            denied_tools: None,
            permission_mode: None,
            workspace_isolation: false,
        }
    }
}

/// The outcome of a completed subagent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentResult {
    /// Final text output produced by the subagent.
    pub output: String,
    /// Paths modified by the subagent (relative to its workspace).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_modified: Vec<String>,
    /// Number of tool calls performed.
    pub tool_calls: usize,
    /// Error message if the subagent failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SubagentResult {
    fn mock(prompt: &str) -> Self {
        Self {
            output: format!("mock subagent output for prompt: {prompt}"),
            files_modified: vec![],
            tool_calls: 0,
            error: None,
        }
    }
}

/// Internal shared state for a single subagent.
#[derive(Debug)]
struct SubagentState {
    id: String,
    name: String,
    status: SubagentStatus,
    result: Option<SubagentResult>,
    worktree_path: Option<PathBuf>,
    spawned_at: DateTime<Utc>,
}

/// Handle to a spawned subagent. Cheap to clone — all state is shared.
#[derive(Debug, Clone)]
pub struct SubagentHandle {
    id: String,
    name: String,
    state: Arc<Mutex<SubagentState>>,
}

impl SubagentHandle {
    /// The unique identifier of this subagent.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The human-readable name of this subagent.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Current lifecycle status.
    pub fn status(&self) -> SubagentStatus {
        self.state.lock().status
    }

    /// The result if the subagent has completed, otherwise `None`.
    pub fn result(&self) -> Option<SubagentResult> {
        self.state.lock().result.clone()
    }

    /// The filesystem path of the git worktree if isolation was requested.
    pub fn worktree_path(&self) -> Option<PathBuf> {
        self.state.lock().worktree_path.clone()
    }

    /// Block asynchronously until the subagent completes and return its
    /// result.
    ///
    /// Because the mock execution is synchronous, this yields once and then
    /// returns the already-populated result.
    #[cfg(feature = "ipc")]
    pub async fn wait(&self) -> SubagentResult {
        tokio::task::yield_now().await;
        loop {
            {
                let guard = self.state.lock();
                if matches!(
                    guard.status,
                    SubagentStatus::Completed
                        | SubagentStatus::Failed
                        | SubagentStatus::Cancelled
                ) {
                    return guard.result.clone().unwrap_or_else(|| SubagentResult {
                        output: String::new(),
                        files_modified: vec![],
                        tool_calls: 0,
                        error: Some("no result recorded".to_string()),
                    });
                }
            }
            tokio::task::yield_now().await;
        }
    }

    /// Synchronous variant of [`wait`](Self::wait) for non-async callers.
    pub fn wait_sync(&self) -> SubagentResult {
        let guard = self.state.lock();
        guard.result.clone().unwrap_or_else(|| SubagentResult {
            output: String::new(),
            files_modified: vec![],
            tool_calls: 0,
            error: Some("no result recorded".to_string()),
        })
    }

    fn transition(&self, status: SubagentStatus, result: Option<SubagentResult>) {
        let mut guard = self.state.lock();
        guard.status = status;
        if result.is_some() {
            guard.result = result;
        }
    }
}

/// Manages the lifecycle of a collection of subagents.
#[derive(Debug, Default)]
pub struct SubagentManager {
    subagents: HashMap<String, SubagentHandle>,
}

impl SubagentManager {
    /// Create an empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn a new subagent with the given config and prompt.
    ///
    /// If `config.workspace_isolation` is true, a git worktree is created at
    /// `.rx4/worktrees/{id}` inside `parent_workspace` before the subagent
    /// starts. The worktree is cleaned up when the subagent completes.
    ///
    /// Execution is mocked — the subagent transitions directly to
    /// [`SubagentStatus::Completed`] with a stub result.
    pub fn spawn(
        &mut self,
        config: SubagentConfig,
        prompt: &str,
        parent_workspace: &Path,
    ) -> Result<SubagentHandle, SubagentError> {
        let id = Uuid::new_v4().to_string();
        let worktree_path = if config.workspace_isolation {
            Some(self.create_worktree(&id, parent_workspace)?)
        } else {
            None
        };

        let state = Arc::new(Mutex::new(SubagentState {
            id: id.clone(),
            name: config.name.clone(),
            status: SubagentStatus::Pending,
            result: None,
            worktree_path: worktree_path.clone(),
            spawned_at: Utc::now(),
        }));
        let handle = SubagentHandle {
            id: id.clone(),
            name: config.name.clone(),
            state,
        };

        handle.transition(SubagentStatus::Running, None);
        let result = SubagentResult::mock(prompt);
        handle.transition(SubagentStatus::Completed, Some(result));

        if let Some(path) = worktree_path {
            let _ = self.cleanup_worktree(&path);
        }

        self.subagents.insert(id, handle.clone());
        Ok(handle)
    }

    /// List all known subagent handles.
    pub fn list(&self) -> Vec<&SubagentHandle> {
        self.subagents.values().collect()
    }

    /// Look up a subagent by id.
    pub fn get(&self, id: &str) -> Option<&SubagentHandle> {
        self.subagents.get(id)
    }

    /// Cancel a running or pending subagent.
    pub fn cancel(&mut self, id: &str) -> Result<(), SubagentError> {
        let handle = self
            .subagents
            .get(id)
            .ok_or_else(|| SubagentError::NotFound(id.to_string()))?;
        {
            let guard = handle.state.lock();
            if matches!(guard.status, SubagentStatus::Completed | SubagentStatus::Failed) {
                return Ok(());
            }
            if matches!(guard.status, SubagentStatus::Cancelled) {
                return Ok(());
            }
        }
        handle.transition(SubagentStatus::Cancelled, None);
        Ok(())
    }

    /// Wait for all subagents to complete and return their results in
    /// insertion order.
    #[cfg(feature = "ipc")]
    pub async fn wait_all(&self) -> Vec<SubagentResult> {
        let mut results = Vec::with_capacity(self.subagents.len());
        for handle in self.subagents.values() {
            results.push(handle.wait().await);
        }
        results
    }

    /// Synchronous variant of [`wait_all`](Self::wait_all).
    pub fn wait_all_sync(&self) -> Vec<SubagentResult> {
        self.subagents
            .values()
            .map(|h| h.wait_sync())
            .collect()
    }

    fn create_worktree(&self, id: &str, parent_workspace: &Path) -> Result<PathBuf, SubagentError> {
        let worktrees_dir = parent_workspace.join(".rx4").join("worktrees");
        std::fs::create_dir_all(&worktrees_dir)
            .map_err(|e| SubagentError::GitError(format!("create worktrees dir: {e}")))?;
        let path = worktrees_dir.join(id);
        std::fs::create_dir_all(&path)
            .map_err(|e| SubagentError::GitError(format!("create worktree dir: {e}")))?;
        Ok(path)
    }

    fn cleanup_worktree(&self, path: &Path) -> Result<(), SubagentError> {
        std::fs::remove_dir_all(path)
            .map_err(|e| SubagentError::GitError(format!("remove worktree dir: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(name: &str) -> SubagentConfig {
        SubagentConfig {
            name: name.to_string(),
            ..SubagentConfig::default()
        }
    }

    #[test]
    fn config_default_uses_max_steps_25() {
        let c = SubagentConfig::default();
        assert_eq!(c.max_steps, 25);
        assert_eq!(c.name, "subagent");
        assert!(!c.workspace_isolation);
    }

    #[test]
    fn config_construction_with_overrides() {
        let c = SubagentConfig {
            name: "reviewer".to_string(),
            system_prompt: Some("you review code".to_string()),
            model: Some("gpt-4o".to_string()),
            max_steps: 10,
            allowed_tools: Some(vec!["read".to_string()]),
            denied_tools: Some(vec!["bash".to_string()]),
            permission_mode: Some(PermissionMode::ReadOnly),
            workspace_isolation: true,
        };
        assert_eq!(c.name, "reviewer");
        assert_eq!(c.max_steps, 10);
        assert!(c.workspace_isolation);
        assert_eq!(c.allowed_tools.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn manager_spawn_returns_handle_with_result() {
        let mut mgr = SubagentManager::new();
        let handle = mgr
            .spawn(config("worker"), "do the thing", Path::new("."))
            .expect("spawn");
        assert_eq!(handle.name(), "worker");
        assert_eq!(handle.status(), SubagentStatus::Completed);
        let result = handle.result().expect("result");
        assert!(result.output.contains("do the thing"));
        assert_eq!(result.tool_calls, 0);
    }

    #[test]
    fn manager_list_and_get() {
        let mut mgr = SubagentManager::new();
        let h1 = mgr.spawn(config("a"), "p1", Path::new(".")).expect("spawn");
        let h2 = mgr.spawn(config("b"), "p2", Path::new(".")).expect("spawn");
        assert_eq!(mgr.list().len(), 2);
        assert!(mgr.get(h1.id()).is_some());
        assert!(mgr.get(h2.id()).is_some());
        assert!(mgr.get("nope").is_none());
    }

    #[test]
    fn manager_cancel_pending_or_running() {
        let mut mgr = SubagentManager::new();
        let handle = mgr.spawn(config("c"), "p", Path::new(".")).expect("spawn");
        mgr.cancel(handle.id()).expect("cancel");
        let status = handle.status();
        assert!(
            matches!(status, SubagentStatus::Cancelled | SubagentStatus::Completed),
            "got {status:?}"
        );
    }

    #[test]
    fn manager_cancel_missing_returns_not_found() {
        let mut mgr = SubagentManager::new();
        let err = mgr.cancel("missing").unwrap_err();
        assert!(matches!(err, SubagentError::NotFound(_)));
    }

    #[test]
    fn status_transitions_cover_all_variants() {
        let state = Arc::new(Mutex::new(SubagentState {
            id: "x".to_string(),
            name: "x".to_string(),
            status: SubagentStatus::Pending,
            result: None,
            worktree_path: None,
            spawned_at: Utc::now(),
        }));
        let handle = SubagentHandle {
            id: "x".to_string(),
            name: "x".to_string(),
            state,
        };
        assert_eq!(handle.status(), SubagentStatus::Pending);
        handle.transition(SubagentStatus::Running, None);
        assert_eq!(handle.status(), SubagentStatus::Running);
        handle.transition(
            SubagentStatus::Completed,
            Some(SubagentResult::mock("done")),
        );
        assert_eq!(handle.status(), SubagentStatus::Completed);
        assert!(handle.result().is_some());
    }

    #[test]
    fn wait_all_sync_returns_results() {
        let mut mgr = SubagentManager::new();
        mgr.spawn(config("a"), "p1", Path::new(".")).expect("spawn");
        mgr.spawn(config("b"), "p2", Path::new(".")).expect("spawn");
        let results = mgr.wait_all_sync();
        assert_eq!(results.len(), 2);
        for r in &results {
            assert!(!r.output.is_empty());
        }
    }

    #[test]
    fn workspace_isolation_creates_and_cleans_worktree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut mgr = SubagentManager::new();
        let cfg = SubagentConfig {
            name: "iso".to_string(),
            workspace_isolation: true,
            ..SubagentConfig::default()
        };
        let handle = mgr.spawn(cfg, "p", tmp.path()).expect("spawn");
        let worktree = handle.worktree_path();
        assert!(worktree.is_some());
        assert!(!worktree.unwrap().exists(), "worktree should be cleaned up");
    }

    #[test]
    fn subagent_error_display() {
        assert_eq!(
            SubagentError::NotFound("x".to_string()).to_string(),
            "subagent not found: x"
        );
        assert_eq!(
            SubagentError::SpawnFailed("boom".to_string()).to_string(),
            "subagent spawn failed: boom"
        );
        assert_eq!(
            SubagentError::AlreadyRunning("y".to_string()).to_string(),
            "subagent already running: y"
        );
        assert_eq!(
            SubagentError::GitError("bad".to_string()).to_string(),
            "git error: bad"
        );
    }
}
