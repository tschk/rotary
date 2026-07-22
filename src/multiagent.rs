//! Multi-agent coordination: role-based agent profiles, team spawning, and
//! an optional in-memory event bus.
//!
//! Built on top of [`crate::subagent`]. The coordinator registers reusable
//! [`AgentProfile`]s and spawns teams of agents that run in parallel via the
//! underlying [`SubagentManager`].

use crate::subagent::{SubagentConfig, SubagentHandle, SubagentManager, SubagentResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Errors raised by multi-agent coordination operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MultiAgentError {
    #[error("agent profile not found: {0}")]
    ProfileNotFound(String),
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("coordination error: {0}")]
    CoordinationError(String),
}

/// The functional role of an agent within a team. Each role carries a
/// different default tool set and permission posture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Orchestrates other agents and aggregates results.
    Coordinator,
    /// Performs implementation work.
    Worker,
    /// Reviews output produced by workers.
    Reviewer,
    /// Gathers information without modifying the workspace.
    Researcher,
}

impl AgentRole {
    /// Default tool allowlist for this role.
    pub fn default_tools(self) -> Vec<String> {
        match self {
            AgentRole::Coordinator => vec!["list_subagents".to_string(), "spawn".to_string()],
            AgentRole::Worker => vec![
                "read".to_string(),
                "write".to_string(),
                "edit".to_string(),
                "bash".to_string(),
            ],
            AgentRole::Reviewer => vec!["read".to_string(), "diff".to_string()],
            AgentRole::Researcher => vec!["read".to_string(), "search".to_string()],
        }
    }

    /// Default permission mode for this role.
    pub fn default_permission_mode(self) -> crate::permissions::PermissionMode {
        match self {
            AgentRole::Coordinator | AgentRole::Worker => {
                crate::permissions::PermissionMode::WorkspaceWrite
            }
            AgentRole::Reviewer | AgentRole::Researcher => {
                crate::permissions::PermissionMode::ReadOnly
            }
        }
    }

    /// Build a default [`SubagentConfig`] for this role with the given name.
    pub fn default_config(self, name: &str) -> SubagentConfig {
        SubagentConfig {
            name: name.to_string(),
            allowed_tools: Some(self.default_tools()),
            denied_tools: None,
            permission_mode: Some(self.default_permission_mode()),
            ..SubagentConfig::default()
        }
    }
}

/// A reusable agent profile combining a name, role, and config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    /// Profile name used to look it up for spawning.
    pub name: String,
    /// The role this profile fulfills.
    pub role: AgentRole,
    /// The full subagent configuration.
    pub config: SubagentConfig,
}

impl AgentProfile {
    /// Create a new profile from a role, deriving a default config.
    pub fn for_role(name: &str, role: AgentRole) -> Self {
        let config = role.default_config(name);
        Self {
            name: name.to_string(),
            role,
            config,
        }
    }
}

/// A single task assigned to a team member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamTask {
    /// Display name for the task / agent instance.
    pub name: String,
    /// The prompt to execute.
    pub prompt: String,
    /// Optional registered profile name. If `None`, a default profile for a
    /// [`AgentRole::Worker`] is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
}

/// The result of a single team member's execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamResult {
    /// The task name.
    pub name: String,
    /// The subagent result.
    pub result: SubagentResult,
    /// Wall-clock duration from spawn to completion.
    #[serde(with = "duration_secs")]
    pub duration: Duration,
}

mod duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(d.as_secs_f64())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let secs = f64::deserialize(d)?;
        if !secs.is_finite() || secs < 0.0 {
            return Err(serde::de::Error::custom(format!(
                "invalid duration: {secs} (must be finite and non-negative)"
            )));
        }
        Ok(Duration::from_secs_f64(secs))
    }
}

/// Events emitted on the coordinator's optional event bus.
#[cfg(feature = "ipc")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoordinatorEvent {
    AgentSpawned {
        id: String,
        name: String,
    },
    AgentCompleted {
        id: String,
        name: String,
    },
    AgentFailed {
        id: String,
        name: String,
        error: String,
    },
    MessageBroadcast {
        message: String,
    },
}

/// Coordinates multi-agent workflows over a shared [`SubagentManager`].
#[derive(Debug)]
pub struct MultiAgentCoordinator {
    manager: SubagentManager,
    profiles: HashMap<String, AgentProfile>,
    spawned: Vec<(String, SubagentHandle, Instant)>,
    #[cfg(feature = "ipc")]
    event_tx: Option<tokio::sync::broadcast::Sender<CoordinatorEvent>>,
}

impl Default for MultiAgentCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiAgentCoordinator {
    /// Create a new coordinator with no registered profiles.
    pub fn new() -> Self {
        Self {
            manager: SubagentManager::new(),
            profiles: HashMap::new(),
            spawned: Vec::new(),
            #[cfg(feature = "ipc")]
            event_tx: None,
        }
    }

    /// Create a coordinator with an event bus of the given capacity.
    #[cfg(feature = "ipc")]
    pub fn with_event_bus(capacity: usize) -> Self {
        let (tx, _rx) = tokio::sync::broadcast::channel(capacity);
        Self {
            manager: SubagentManager::new(),
            profiles: HashMap::new(),
            spawned: Vec::new(),
            event_tx: Some(tx),
        }
    }

    /// Subscribe to the event bus, if one is configured.
    #[cfg(feature = "ipc")]
    pub fn subscribe(&self) -> Option<tokio::sync::broadcast::Receiver<CoordinatorEvent>> {
        self.event_tx.as_ref().map(|tx| tx.subscribe())
    }

    /// Register a reusable agent profile.
    pub fn register_profile(&mut self, profile: AgentProfile) {
        self.profiles.insert(profile.name.clone(), profile);
    }

    /// Look up a registered profile by name.
    pub fn profile(&self, name: &str) -> Option<&AgentProfile> {
        self.profiles.get(name)
    }

    /// Spawn an agent using a registered profile.
    pub fn spawn_named(
        &mut self,
        profile_name: &str,
        prompt: &str,
        workspace: &Path,
    ) -> Result<SubagentHandle, MultiAgentError> {
        let config = {
            let profile = self
                .profiles
                .get(profile_name)
                .ok_or_else(|| MultiAgentError::ProfileNotFound(profile_name.to_string()))?;
            profile.config.clone()
        };
        let start = Instant::now();
        let handle = self
            .manager
            .spawn(config, prompt, workspace)
            .map_err(|e| MultiAgentError::SpawnFailed(e.to_string()))?;
        #[cfg(feature = "ipc")]
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(CoordinatorEvent::AgentSpawned {
                id: handle.id().to_string(),
                name: handle.name().to_string(),
            });
        }
        self.spawned
            .push((handle.name().to_string(), handle.clone(), start));
        Ok(handle)
    }

    /// Spawn a team of agents in parallel. Each task is spawned immediately;
    /// because the underlying manager mocks synchronous execution, all handles
    /// are already completed by the time this returns.
    pub fn spawn_team(
        &mut self,
        tasks: Vec<TeamTask>,
        workspace: &Path,
    ) -> Result<Vec<SubagentHandle>, MultiAgentError> {
        let mut handles = Vec::with_capacity(tasks.len());
        for task in tasks {
            let handle = if let Some(profile_name) = &task.profile_name {
                self.spawn_named(profile_name, &task.prompt, workspace)?
            } else {
                let config = AgentRole::Worker.default_config(&task.name);
                let start = Instant::now();
                let h = self
                    .manager
                    .spawn(config, &task.prompt, workspace)
                    .map_err(|e| MultiAgentError::SpawnFailed(e.to_string()))?;
                #[cfg(feature = "ipc")]
                if let Some(tx) = &self.event_tx {
                    let _ = tx.send(CoordinatorEvent::AgentSpawned {
                        id: h.id().to_string(),
                        name: h.name().to_string(),
                    });
                }
                self.spawned.push((h.name().to_string(), h.clone(), start));
                h
            };
            handles.push(handle);
        }
        Ok(handles)
    }

    /// Broadcast a message to all running agents. In the mock execution model
    /// this is a no-op aside from emitting an event; a real implementation
    /// would push the message into each agent's inbox.
    pub fn broadcast(&self, _message: &str) {
        #[cfg(feature = "ipc")]
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(CoordinatorEvent::MessageBroadcast {
                message: _message.to_string(),
            });
        }
    }

    /// Gather results from all spawned agents, recording wall-clock duration.
    pub fn collect_results(&self) -> Vec<TeamResult> {
        self.spawned
            .iter()
            .map(|(name, handle, start)| {
                let result = handle.wait_sync();
                let duration = start.elapsed();
                TeamResult {
                    name: name.clone(),
                    result,
                    duration,
                }
            })
            .collect()
    }

    /// Access the underlying subagent manager.
    pub fn manager(&self) -> &SubagentManager {
        &self.manager
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::PermissionMode;

    #[test]
    fn agent_role_default_tools_differ() {
        assert_ne!(
            AgentRole::Worker.default_tools(),
            AgentRole::Reviewer.default_tools()
        );
        assert!(AgentRole::Researcher
            .default_tools()
            .contains(&"search".to_string()));
    }

    #[test]
    fn agent_role_default_permissions() {
        assert_eq!(
            AgentRole::Worker.default_permission_mode(),
            PermissionMode::WorkspaceWrite
        );
        assert_eq!(
            AgentRole::Reviewer.default_permission_mode(),
            PermissionMode::ReadOnly
        );
    }

    #[test]
    fn profile_for_role_derives_config() {
        let profile = AgentProfile::for_role("builder", AgentRole::Worker);
        assert_eq!(profile.role, AgentRole::Worker);
        assert_eq!(profile.config.name, "builder");
        assert!(profile.config.allowed_tools.is_some());
    }

    #[test]
    fn coordinator_register_and_lookup_profile() {
        let mut coord = MultiAgentCoordinator::new();
        let profile = AgentProfile::for_role("builder", AgentRole::Worker);
        coord.register_profile(profile);
        assert!(coord.profile("builder").is_some());
        assert!(coord.profile("missing").is_none());
    }

    #[test]
    fn spawn_named_missing_profile_errors() {
        let mut coord = MultiAgentCoordinator::new();
        let err = coord.spawn_named("nope", "p", Path::new(".")).unwrap_err();
        assert!(matches!(err, MultiAgentError::ProfileNotFound(_)));
    }

    #[test]
    fn spawn_named_succeeds_with_registered_profile() {
        let mut coord = MultiAgentCoordinator::new();
        coord.register_profile(AgentProfile::for_role("worker", AgentRole::Worker));
        let handle = coord
            .spawn_named("worker", "build it", Path::new("."))
            .expect("spawn");
        assert_eq!(handle.name(), "worker");
        assert!(handle.result().is_some());
    }

    #[test]
    fn spawn_team_uses_default_worker_for_unnamed_tasks() {
        let mut coord = MultiAgentCoordinator::new();
        let tasks = vec![
            TeamTask {
                name: "t1".to_string(),
                prompt: "do one".to_string(),
                profile_name: None,
            },
            TeamTask {
                name: "t2".to_string(),
                prompt: "do two".to_string(),
                profile_name: None,
            },
        ];
        let handles = coord.spawn_team(tasks, Path::new(".")).expect("team");
        assert_eq!(handles.len(), 2);
        assert!(handles.iter().all(|h| h.result().is_some()));
    }

    #[test]
    fn spawn_team_uses_registered_profiles() {
        let mut coord = MultiAgentCoordinator::new();
        coord.register_profile(AgentProfile::for_role("rev", AgentRole::Reviewer));
        let tasks = vec![TeamTask {
            name: "rev1".to_string(),
            prompt: "review it".to_string(),
            profile_name: Some("rev".to_string()),
        }];
        let handles = coord.spawn_team(tasks, Path::new(".")).expect("team");
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].name(), "rev");
    }

    #[test]
    fn collect_results_returns_one_per_spawned() {
        let mut coord = MultiAgentCoordinator::new();
        coord.register_profile(AgentProfile::for_role("worker", AgentRole::Worker));
        coord
            .spawn_named("worker", "a", Path::new("."))
            .expect("spawn");
        coord
            .spawn_named("worker", "b", Path::new("."))
            .expect("spawn");
        let results = coord.collect_results();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| !r.result.output.is_empty()));
    }

    #[test]
    fn broadcast_is_safe_without_event_bus() {
        let coord = MultiAgentCoordinator::new();
        coord.broadcast("hello");
    }

    #[cfg(feature = "ipc")]
    #[tokio::test]
    async fn event_bus_emits_spawn_and_broadcast() {
        let mut coord = MultiAgentCoordinator::with_event_bus(16);
        let mut rx = coord.subscribe().expect("bus");
        coord.register_profile(AgentProfile::for_role("worker", AgentRole::Worker));
        coord
            .spawn_named("worker", "p", Path::new("."))
            .expect("spawn");
        coord.broadcast("hi");
        let mut saw_spawn = false;
        let mut saw_broadcast = false;
        for _ in 0..2 {
            if let Ok(ev) = rx.try_recv() {
                match ev {
                    CoordinatorEvent::AgentSpawned { .. } => saw_spawn = true,
                    CoordinatorEvent::MessageBroadcast { .. } => saw_broadcast = true,
                    _ => {}
                }
            }
        }
        assert!(saw_spawn);
        assert!(saw_broadcast);
    }

    #[test]
    fn multiagent_error_display() {
        assert_eq!(
            MultiAgentError::ProfileNotFound("x".to_string()).to_string(),
            "agent profile not found: x"
        );
        assert_eq!(
            MultiAgentError::SpawnFailed("boom".to_string()).to_string(),
            "spawn failed: boom"
        );
        assert_eq!(
            MultiAgentError::CoordinationError("c".to_string()).to_string(),
            "coordination error: c"
        );
    }

    #[test]
    fn duration_rejects_nan_via_team_result() {
        let json = r#"{"name":"t","result":{"output":"","files_modified":[],"tool_calls":0},"duration":NaN}"#;
        let result = serde_json::from_str::<super::TeamResult>(json);
        assert!(result.is_err(), "NaN duration must be rejected");
    }

    #[test]
    fn duration_rejects_infinity_via_team_result() {
        let json = r#"{"name":"t","result":{"output":"","files_modified":[],"tool_calls":0},"duration":Infinity}"#;
        let result = serde_json::from_str::<super::TeamResult>(json);
        assert!(result.is_err(), "infinity duration must be rejected");
    }

    #[test]
    fn duration_rejects_negative_via_team_result() {
        let json = r#"{"name":"t","result":{"output":"","files_modified":[],"tool_calls":0},"duration":-1.0}"#;
        let result = serde_json::from_str::<super::TeamResult>(json);
        assert!(result.is_err(), "negative duration must be rejected");
    }

    #[test]
    fn duration_accepts_valid_via_team_result() {
        let json = r#"{"name":"t","result":{"output":"","files_modified":[],"tool_calls":0},"duration":1.5}"#;
        let result = serde_json::from_str::<super::TeamResult>(json);
        assert!(result.is_ok(), "valid duration must be accepted");
        assert_eq!(
            result.unwrap().duration,
            std::time::Duration::from_millis(1500)
        );
    }
}
