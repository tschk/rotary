use super::*;
use crate::agent::{ToolCall, ToolContext, ToolDefinition, ToolResult};
use crate::provider::{Message, ProviderError, StreamEvent, StreamResult};
use tokio::sync::Semaphore;

fn identity(n: &str) -> SubagentIdentity {
    SubagentIdentity {
        root_session_id: "root".into(),
        parent_session_id: "root".into(),
        session_id: format!("session-{n}"),
        run_id: format!("run-{n}"),
        request_id: format!("request-{n}"),
    }
}

struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct PendingProvider {
    entered: Arc<Semaphore>,
    active: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Provider for PendingProvider {
    fn id(&self) -> &str {
        "pending-fixture"
    }
    fn name(&self) -> &str {
        "pending-fixture"
    }
    async fn stream(
        &self,
        _: &[Message],
        _: &Option<String>,
        _: &str,
        _: &[serde_json::Value],
        _: Option<&str>,
    ) -> Result<StreamResult, ProviderError> {
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = Active(Arc::clone(&self.active));
        self.entered.add_permits(1);
        std::future::pending().await
    }
}

fn pending_manager() -> (SubagentManager, Arc<Semaphore>, Arc<AtomicUsize>) {
    let entered = Arc::new(Semaphore::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let manager = SubagentManager::new()
        .with_root_session("root")
        .unwrap()
        .with_provider(Arc::new(PendingProvider {
            entered: Arc::clone(&entered),
            active: Arc::clone(&active),
        }));
    (manager, entered, active)
}
async fn entered(semaphore: &Semaphore, n: u32) {
    tokio::time::timeout(Duration::from_secs(3), semaphore.acquire_many(n))
        .await
        .expect("fixture entered")
        .expect("semaphore")
        .forget();
}
async fn settled(handle: &SubagentHandle) -> SubagentResult {
    tokio::time::timeout(Duration::from_secs(3), handle.wait())
        .await
        .expect("worker settled")
}

#[tokio::test]
async fn provider_cancellation_drops_execution_before_one_terminal_event() {
    let (mut manager, started, active) = pending_manager();
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let active_at_finish = Arc::clone(&active);
    manager.subscribe(move |e| {
        if matches!(e, SubagentEvent::Finished { .. }) {
            assert_eq!(active_at_finish.load(Ordering::SeqCst), 0);
        }
        observed.lock().push(e.clone());
    });
    let h = manager
        .spawn_background(SubagentConfig::default(), "wait", Path::new("."))
        .unwrap();
    entered(&started, 1).await;
    manager.cancel(h.id()).unwrap();
    assert_eq!(h.status(), SubagentStatus::Stopping);
    assert!(h.result().is_none());
    assert_eq!(settled(&h).await.error.as_deref(), Some("cancelled"));
    manager.cancel(h.id()).unwrap();
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(manager.running_count(), 0);
    assert_eq!(
        events
            .lock()
            .iter()
            .filter(|e| matches!(e, SubagentEvent::Finished { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn stable_requests_deduplicate_and_restore_never_replays() {
    let (mut manager, started, active) = pending_manager();
    let h = manager
        .spawn_background_with_identity(
            SubagentConfig::default(),
            "wait",
            Path::new("."),
            identity("a"),
        )
        .unwrap();
    let retry = manager
        .clone()
        .spawn_background_with_identity(
            SubagentConfig::default(),
            "wait",
            Path::new("."),
            identity("a"),
        )
        .unwrap();
    assert_eq!(h.id(), retry.id());
    assert!(matches!(
        manager.spawn_background_with_identity(
            SubagentConfig::default(),
            "other",
            Path::new("."),
            identity("a")
        ),
        Err(SubagentError::Conflict(_))
    ));
    entered(&started, 1).await;
    let receipt = h.steer("message-a", "bounded reference 🦀").unwrap();
    assert_eq!(
        h.steer("message-a", "bounded reference 🦀").unwrap(),
        receipt
    );
    assert!(matches!(
        h.steer("message-a", "other"),
        Err(SubagentError::Conflict(_))
    ));
    let snapshot: SubagentSnapshot =
        serde_json::from_str(&serde_json::to_string(&h.snapshot()).unwrap()).unwrap();
    let mut restored = SubagentManager::new();
    let restored_h = restored.restore(snapshot).unwrap();
    assert_eq!(restored_h.status(), SubagentStatus::Interrupted);
    let retry = restored
        .spawn_background_with_identity(
            SubagentConfig::default(),
            "wait",
            Path::new("."),
            identity("a"),
        )
        .unwrap();
    assert_eq!(restored_h.id(), retry.id());
    assert_eq!(restored_h.snapshot().session.id, "session-a");
    assert_eq!(
        restored_h
            .steer("message-a", "bounded reference 🦀")
            .unwrap(),
        receipt
    );
    assert!(restored_h.steer("new", "must not resume").is_err());
    assert_eq!(active.load(Ordering::SeqCst), 1);
    manager.shutdown_and_wait(Duration::from_secs(3)).await;
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn empty_and_oversized_requests_fail_before_reservation() {
    let (mut manager, _, _) = pending_manager();
    assert!(SubagentManager::new()
        .spawn_background(SubagentConfig::default(), "task", Path::new("."))
        .is_err());
    assert!(manager
        .spawn_background(SubagentConfig::default(), " ", Path::new("."))
        .is_err());
    assert!(manager
        .spawn_background(
            SubagentConfig::default(),
            &"x".repeat(MAX_CONTEXT_BYTES + 1),
            Path::new(".")
        )
        .is_err());
    let config = SubagentConfig {
        system_prompt: Some("x".repeat(MAX_CONTEXT_BYTES + 1)),
        use_capsule: true,
        ..Default::default()
    };
    assert!(manager
        .spawn_background(config, "task", Path::new("."))
        .is_err());
    assert!(manager.list().is_empty());
    let h = manager
        .spawn_background(SubagentConfig::default(), "task", Path::new("."))
        .unwrap();
    assert!(h.steer("empty", "").is_err());
    assert!(h.steer("big", &"é".repeat(MAX_STEER_BYTES)).is_err());
    manager.shutdown_and_wait(Duration::from_secs(3)).await;
}

struct RoundProvider {
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
}
#[async_trait::async_trait]
impl Provider for RoundProvider {
    fn id(&self) -> &str {
        "round-fixture"
    }
    fn name(&self) -> &str {
        "round-fixture"
    }
    async fn stream(
        &self,
        messages: &[Message],
        _: &Option<String>,
        _: &str,
        _: &[serde_json::Value],
        _: Option<&str>,
    ) -> Result<StreamResult, ProviderError> {
        self.requests.lock().push(messages.to_vec());
        let event = if messages
            .iter()
            .any(|m| m.role == crate::provider::Role::Tool)
        {
            StreamEvent::Delta("completed".into())
        } else {
            StreamEvent::ToolCall(ToolCall {
                id: "gate-call".into(),
                name: "read".into(),
                arguments: "{}".into(),
            })
        };
        Ok(Box::new(futures::stream::iter([
            Ok(event),
            Ok(StreamEvent::Done),
        ])))
    }
}
type GateFixture = (
    SubagentManager,
    Arc<Semaphore>,
    Arc<Semaphore>,
    Arc<AtomicUsize>,
    Arc<Mutex<Vec<Vec<Message>>>>,
);

fn gated_manager() -> GateFixture {
    let started = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let tools = ToolRegistry::new();
    let (s, r, a) = (
        Arc::clone(&started),
        Arc::clone(&release),
        Arc::clone(&active),
    );
    tools.register(ToolDefinition::new_boxed(
        "read",
        "fixture gate",
        "{}",
        Box::new(move |_ctx: Arc<ToolContext>, _| {
            let (s, r, a) = (Arc::clone(&s), Arc::clone(&r), Arc::clone(&a));
            Box::pin(async move {
                a.fetch_add(1, Ordering::SeqCst);
                let _active = Active(a);
                s.add_permits(1);
                r.acquire().await.unwrap().forget();
                ToolResult::ok("gate", "settled")
            })
        }),
    ));
    let manager = SubagentManager::new()
        .with_root_session("root")
        .unwrap()
        .with_provider(Arc::new(RoundProvider {
            requests: Arc::clone(&requests),
        }))
        .with_tools(Arc::new(tools));
    (manager, started, release, active, requests)
}

#[tokio::test]
async fn two_children_receive_only_their_own_accepted_steering() {
    let (mut manager, started, release, _, requests) = gated_manager();
    let a = manager
        .spawn_background_with_identity(
            SubagentConfig::default(),
            "task-a",
            Path::new("."),
            identity("a"),
        )
        .unwrap();
    let b = manager
        .spawn_background_with_identity(
            SubagentConfig::default(),
            "task-b",
            Path::new("."),
            identity("b"),
        )
        .unwrap();
    entered(&started, 2).await;
    a.steer("same-id", "steer-a").unwrap();
    b.steer("same-id", "steer-b").unwrap();
    release.add_permits(2);
    assert!(settled(&a).await.error.is_none());
    assert!(settled(&b).await.error.is_none());
    let requests = requests.lock();
    for (task, own, other) in [
        ("task-a", "steer-a", "steer-b"),
        ("task-b", "steer-b", "steer-a"),
    ] {
        let request = requests
            .iter()
            .find(|ms| ms.iter().any(|m| m.content == task) && ms.iter().any(|m| m.content == own))
            .expect("steer reached provider");
        assert!(!request.iter().any(|m| m.content == other));
    }
    assert_eq!(a.snapshot().session.id, "session-a");
}

#[tokio::test]
async fn pending_tool_timeout_reports_stopping_until_cleanup_finishes() {
    let (mut manager, started, release, active, _) = gated_manager();
    let events = Arc::new(AtomicUsize::new(0));
    let e = Arc::clone(&events);
    manager.subscribe(move |event| {
        if matches!(event, SubagentEvent::Finished { .. }) {
            e.fetch_add(1, Ordering::SeqCst);
        }
    });
    let h = manager
        .spawn_background(SubagentConfig::default(), "task", Path::new("."))
        .unwrap();
    entered(&started, 1).await;
    let snapshot = manager
        .cancel_tree_and_wait(h.id(), Duration::from_millis(10))
        .await
        .unwrap();
    assert_eq!(snapshot[0].status, SubagentStatus::Stopping);
    assert_eq!(active.load(Ordering::SeqCst), 1);
    assert_eq!(events.load(Ordering::SeqCst), 0);
    assert!(h.result().is_none());
    assert!(matches!(
        manager.cleanup(h.id()),
        Err(SubagentError::AlreadyRunning(_))
    ));
    release.add_permits(1);
    settled(&h).await;
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(h.status(), SubagentStatus::Cancelled);
    assert_eq!(events.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn descendants_and_manager_shutdown_settle_and_seal_the_family() {
    let (mut manager, started, active) = pending_manager();
    let parent = manager
        .spawn_background(SubagentConfig::default(), "parent", Path::new("."))
        .unwrap();
    let child = manager
        .spawn_background(
            SubagentConfig {
                parent_id: Some(parent.id().into()),
                ..Default::default()
            },
            "child",
            Path::new("."),
        )
        .unwrap();
    let grandchild = manager
        .spawn_background(
            SubagentConfig {
                parent_id: Some(child.id().into()),
                ..Default::default()
            },
            "grandchild",
            Path::new("."),
        )
        .unwrap();
    entered(&started, 3).await;
    let snapshots = manager
        .cancel_tree_and_wait(parent.id(), Duration::from_secs(3))
        .await
        .unwrap();
    assert_eq!(snapshots.len(), 3);
    assert!(snapshots
        .iter()
        .all(|s| s.status == SubagentStatus::Cancelled));
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(grandchild.status(), SubagentStatus::Cancelled);
    manager.shutdown_and_wait(Duration::from_secs(3)).await;
    assert!(manager
        .spawn_background(SubagentConfig::default(), "late", Path::new("."))
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_finish_and_cancel_emit_one_settled_result() {
    for _ in 0..8 {
        let (mut manager, started, release, _, _) = gated_manager();
        let count = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&count);
        manager.subscribe(move |e| {
            if matches!(e, SubagentEvent::Finished { .. }) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
        });
        let h = manager
            .spawn_background(SubagentConfig::default(), "task", Path::new("."))
            .unwrap();
        entered(&started, 1).await;
        let finish = tokio::spawn(async move {
            release.add_permits(1);
        });
        let mut cancelling = manager.clone();
        let id = h.id().to_owned();
        let cancel = tokio::spawn(async move {
            cancelling.cancel(&id).unwrap();
        });
        finish.await.unwrap();
        cancel.await.unwrap();
        settled(&h).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(terminal(h.status()));
    }
}

struct ProcessProvider {
    arguments: String,
}
#[async_trait::async_trait]
impl Provider for ProcessProvider {
    fn id(&self) -> &str {
        "process-fixture"
    }
    fn name(&self) -> &str {
        "process-fixture"
    }
    async fn stream(
        &self,
        messages: &[Message],
        _: &Option<String>,
        _: &str,
        _: &[serde_json::Value],
        _: Option<&str>,
    ) -> Result<StreamResult, ProviderError> {
        let event = if messages
            .iter()
            .any(|m| m.role == crate::provider::Role::Tool)
        {
            StreamEvent::Delta("finished".into())
        } else {
            StreamEvent::ToolCall(ToolCall {
                id: "process-call".into(),
                name: "bash".into(),
                arguments: self.arguments.clone(),
            })
        };
        Ok(Box::new(futures::stream::iter([
            Ok(event),
            Ok(StreamEvent::Done),
        ])))
    }
}

#[cfg(all(unix, feature = "builtin-tools"))]
#[tokio::test]
async fn shell_process_is_reaped_before_worker_cancellation_settles() {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let pidfile = root.join("child.pid");
    let command = format!(
        "sleep 60 & child=$!; echo $$ $child > '{}'; exit 0",
        pidfile.display()
    );
    let tools = ToolRegistry::new();
    crate::tools::register_builtin_tools(&tools);
    let mut parent = Agent::new();
    parent.set_workspace_root(&root);
    parent.set_policy(Policy::full_access());
    parent.os_sandbox = None;
    let authority = parent.subagent_authority();
    let mut manager = SubagentManager::new()
        .with_authority(authority)
        .with_tools(Arc::new(tools))
        .with_provider(Arc::new(ProcessProvider {
            arguments: serde_json::json!({"command":command,"timeout":60}).to_string(),
        }));
    let worker = manager
        .spawn_background(
            SubagentConfig {
                permission_mode: Some(PermissionMode::FullAccess),
                ..Default::default()
            },
            "run pending process",
            &root,
        )
        .unwrap();
    let pid = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&pidfile) {
                let pids = text
                    .split_whitespace()
                    .map(str::parse::<u32>)
                    .collect::<Result<Vec<_>, _>>();
                if let Ok(pids) = pids {
                    if pids.len() == 2 {
                        break pids;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|e| panic!("subprocess start {e}: {:?}", worker.snapshot()));
    for pid in &pid {
        assert!(Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap()
            .success());
    }
    let snapshots = manager
        .cancel_tree_and_wait(worker.id(), Duration::from_secs(3))
        .await
        .unwrap();
    assert_eq!(snapshots[0].status, SubagentStatus::Cancelled);
    for process in pid {
        let state = Command::new("ps")
            .args(["-p", &process.to_string(), "-o", "stat="])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&state.stdout);
        assert!(
            state.trim().is_empty() || state.trim().starts_with('Z'),
            "worker terminal while process {process} executes: {state}"
        );
    }
}

#[cfg(feature = "builtin-tools")]
#[tokio::test]
async fn inherited_required_os_sandbox_refuses_shell_without_a_runner() {
    let workspace = tempfile::tempdir().unwrap();
    let tools = ToolRegistry::new();
    crate::tools::register_builtin_tools(&tools);
    let mut parent = Agent::new();
    parent.set_policy(Policy::full_access());
    let mut authority = parent.subagent_authority();
    authority.policy.enable_os_sandbox = true;
    authority.os_sandbox_failed = true;
    let mut manager = SubagentManager::new()
        .with_tools(Arc::new(tools))
        .with_authority(authority)
        .with_provider(Arc::new(ProcessProvider {
            arguments: r#"{"command":"touch forbidden-marker"}"#.into(),
        }));
    let h = manager
        .spawn_background(
            SubagentConfig {
                permission_mode: Some(PermissionMode::FullAccess),
                ..Default::default()
            },
            "attempt process",
            workspace.path(),
        )
        .unwrap();
    settled(&h).await;
    assert!(!workspace.path().join("forbidden-marker").exists());
    assert!(h
        .snapshot()
        .session
        .provider_messages()
        .iter()
        .any(|m| m.role == crate::provider::Role::Tool && m.content.contains("sandbox")));
}

struct FailureProvider;
#[async_trait::async_trait]
impl Provider for FailureProvider {
    fn id(&self) -> &str {
        "failure-fixture"
    }
    fn name(&self) -> &str {
        "failure-fixture"
    }
    async fn stream(
        &self,
        _: &[Message],
        _: &Option<String>,
        _: &str,
        _: &[serde_json::Value],
        _: Option<&str>,
    ) -> Result<StreamResult, ProviderError> {
        Err(ProviderError::Api("fixture failure".into()))
    }
}

#[tokio::test]
async fn provider_failure_is_a_settled_failed_run() {
    let mut manager = SubagentManager::new().with_provider(Arc::new(FailureProvider));
    let h = manager
        .spawn_background(SubagentConfig::default(), "task", Path::new("."))
        .unwrap();
    assert!(settled(&h).await.error.unwrap().contains("fixture failure"));
    assert_eq!(h.status(), SubagentStatus::Failed);
}

struct NestedProvider {
    child_started: Arc<Semaphore>,
}
#[async_trait::async_trait]
impl Provider for NestedProvider {
    fn id(&self) -> &str {
        "nested-fixture"
    }
    fn name(&self) -> &str {
        "nested-fixture"
    }
    async fn stream(
        &self,
        messages: &[Message],
        _: &Option<String>,
        _: &str,
        _: &[serde_json::Value],
        _: Option<&str>,
    ) -> Result<StreamResult, ProviderError> {
        if messages.iter().any(|m| m.content == "nested child") {
            self.child_started.add_permits(1);
            return Ok(Box::new(futures::stream::pending()));
        }
        if messages
            .iter()
            .any(|m| m.role == crate::provider::Role::Tool)
        {
            return Ok(Box::new(futures::stream::pending()));
        }
        Ok(Box::new(futures::stream::iter([
            Ok(StreamEvent::ToolCall(ToolCall {
                id: "nested-call".into(),
                name: "spawn_agent".into(),
                arguments: r#"{"prompt":"nested child","isolate":false}"#.into(),
            })),
            Ok(StreamEvent::Done),
        ])))
    }
}

#[tokio::test]
async fn model_spawned_descendant_uses_the_same_family_and_real_parent() {
    let started = Arc::new(Semaphore::new(0));
    let tools = ToolRegistry::new();
    crate::tools::register_builtin_tools(&tools);
    let mut manager = SubagentManager::new()
        .with_provider(Arc::new(NestedProvider {
            child_started: Arc::clone(&started),
        }))
        .with_tools(Arc::new(tools));
    let parent = manager
        .spawn_background(
            SubagentConfig {
                permission_mode: Some(PermissionMode::FullAccess),
                ..Default::default()
            },
            "parent",
            Path::new("."),
        )
        .unwrap();
    entered(&started, 1).await;
    let child = manager
        .list()
        .into_iter()
        .find(|h| h.id() != parent.id())
        .expect("shared child tree");
    assert_eq!(child.parent_id().as_deref(), Some(parent.id()));
    assert_eq!(
        child.snapshot().identity.parent_session_id,
        parent.snapshot().identity.session_id
    );
    manager.cancel(parent.id()).unwrap();
    assert!(manager
        .spawn_background(
            SubagentConfig {
                parent_id: Some(parent.id().into()),
                ..Default::default()
            },
            "late child",
            Path::new(".")
        )
        .is_err());
    settled(&parent).await;
    assert_eq!(child.status(), SubagentStatus::Cancelled);
}

#[tokio::test]
async fn restored_snapshot_redacts_all_text_and_rejects_secret_identity() {
    let (mut manager, started, _) = pending_manager();
    let h = manager
        .spawn_background_with_identity(
            SubagentConfig::default(),
            "task",
            Path::new("."),
            identity("redaction"),
        )
        .unwrap();
    entered(&started, 1).await;
    let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
    let mut snapshot = h.snapshot();
    snapshot.name = secret.into();
    snapshot.settlement_error = Some(secret.into());
    snapshot.session.name = secret.into();
    snapshot.session.entries[0].content = secret.into();
    snapshot.session.entries[0].tool_call_id = Some(secret.into());
    snapshot.session.entries[0].tool_calls.push(ToolCall {
        id: secret.into(),
        name: secret.into(),
        arguments: secret.into(),
    });
    snapshot.result = Some(SubagentResult {
        output: secret.into(),
        files_modified: vec![secret.into()],
        tool_calls: 0,
        error: Some(secret.into()),
        cost: 0.0,
        duration_seconds: 0,
    });
    snapshot.steering.push(SteerReceipt {
        message_id: "safe".into(),
        text: secret.into(),
        entry_id: 1,
        fingerprint: format!("{:x}", Sha256::digest(secret.as_bytes())),
    });
    let mut restored = SubagentManager::new();
    let restored_h = restored.restore(snapshot.clone()).unwrap();
    let encoded = serde_json::to_string(&restored_h.snapshot()).unwrap();
    assert!(!encoded.contains(secret));
    assert!(!restored_h.name().contains(secret));
    assert!(!restored_h.result().unwrap().output.contains(secret));
    snapshot.status = SubagentStatus::Completed;
    let contradictory = SubagentManager::new().restore(snapshot.clone()).unwrap();
    assert_eq!(contradictory.status(), SubagentStatus::Interrupted);
    snapshot.identity.run_id = secret.into();
    assert!(SubagentManager::new().restore(snapshot).is_err());
    manager.shutdown_and_wait(Duration::from_secs(3)).await;
}

struct FinishAfterSpawn {
    child_started: Arc<Semaphore>,
    child_tool: bool,
}
#[async_trait::async_trait]
impl Provider for FinishAfterSpawn {
    fn id(&self) -> &str {
        "finish-spawn-fixture"
    }
    fn name(&self) -> &str {
        "finish-spawn-fixture"
    }
    async fn stream(
        &self,
        messages: &[Message],
        _: &Option<String>,
        _: &str,
        _: &[serde_json::Value],
        _: Option<&str>,
    ) -> Result<StreamResult, ProviderError> {
        let event = if messages.iter().any(|m| m.content == "nested child") {
            self.child_started.add_permits(1);
            if !self.child_tool {
                return Ok(Box::new(futures::stream::pending()));
            }
            StreamEvent::ToolCall(ToolCall {
                id: "read-child".into(),
                name: "read".into(),
                arguments: "{}".into(),
            })
        } else if messages
            .iter()
            .any(|m| m.role == crate::provider::Role::Tool)
        {
            StreamEvent::Delta("parent finished".into())
        } else {
            StreamEvent::ToolCall(ToolCall {
                id: "spawn-child".into(),
                name: "spawn_agent".into(),
                arguments: r#"{"prompt":"nested child","isolate":false}"#.into(),
            })
        };
        Ok(Box::new(futures::stream::iter([
            Ok(event),
            Ok(StreamEvent::Done),
        ])))
    }
}

#[tokio::test]
async fn successful_root_prompt_keeps_background_children_running() {
    let started = Arc::new(Semaphore::new(0));
    let manager = Arc::new(Mutex::new(SubagentManager::new()));
    let tools = ToolRegistry::new();
    crate::tools::register_spawn_agent_tool(&tools, Arc::clone(&manager));
    let mut parent = Agent::new();
    parent.set_policy(Policy::full_access());
    parent.set_tools(tools);
    parent.set_provider(Arc::new(FinishAfterSpawn {
        child_started: Arc::clone(&started),
        child_tool: false,
    }));
    tokio::time::timeout(Duration::from_secs(3), parent.prompt("parent task"))
        .await
        .expect("root prompt remains nonblocking")
        .unwrap();
    entered(&started, 1).await;
    let child = manager.lock().list().pop().expect("background child");
    assert_eq!(child.status(), SubagentStatus::Running);
    let mut drain = manager.lock().clone();
    drain.shutdown_and_wait(Duration::from_secs(3)).await;
}

#[tokio::test]
async fn captured_cancel_handle_remains_effective_during_descendant_drain() {
    let (mut manager, started, release, _, _) = gated_manager();
    let tools = manager.tools.as_ref().unwrap();
    crate::tools::register_builtin_tools(tools);
    // Restore the deliberate noncooperative read fixture overwritten by builtins.
    let s = Arc::clone(&started);
    let r = Arc::clone(&release);
    tools.register(ToolDefinition::new_boxed(
        "read",
        "cleanup gate",
        "{}",
        Box::new(move |_, _| {
            let (s, r) = (Arc::clone(&s), Arc::clone(&r));
            Box::pin(async move {
                s.add_permits(1);
                r.acquire().await.unwrap().forget();
                ToolResult::ok("read", "cleaned")
            })
        }),
    ));
    let child_started = Arc::new(Semaphore::new(0));
    manager.set_provider(Arc::new(FinishAfterSpawn {
        child_started,
        child_tool: true,
    }));
    let parent = manager
        .spawn_background(
            SubagentConfig {
                permission_mode: Some(PermissionMode::FullAccess),
                ..Default::default()
            },
            "parent task",
            Path::new("."),
        )
        .unwrap();
    entered(&started, 1).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while parent.status() != SubagentStatus::Stopping {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let cancellation = parent
        .state
        .lock()
        .cancellation
        .clone()
        .expect("parent retains cancellation until settlement");
    cancellation.cancel();
    release.add_permits(1);
    settled(&parent).await;
    assert_eq!(parent.status(), SubagentStatus::Cancelled);
    assert!(manager.list().iter().all(|h| terminal(h.status())));
}

#[tokio::test]
async fn explicit_calling_family_is_shared_by_spawn_list_and_cancel_tools() {
    let registered = Arc::new(Mutex::new(SubagentManager::new()));
    let registry = Arc::new(ToolRegistry::new());
    crate::tools::register_spawn_agent_tool(&registry, Arc::clone(&registered));
    let (family, started, _) = pending_manager();
    let mut context = ToolContext::new(".");
    context.subagent_manager = Some(family.clone());
    context.provider = family.provider.clone();
    context.tools = Some(Arc::clone(&registry));
    let context = Arc::new(context);
    let spawned = registry
        .execute(
            "spawn_agent",
            &context,
            r#"{"prompt":"wait","isolate":false}"#,
        )
        .await
        .unwrap();
    assert!(!spawned.is_error, "{}", spawned.content);
    entered(&started, 1).await;
    assert!(registered.lock().list().is_empty());
    let h = family.list().pop().unwrap();
    let listed = registry
        .execute("list_subagents", &context, "{}")
        .await
        .unwrap();
    assert!(listed.content.contains(h.id()));
    let cancelled = registry
        .execute(
            "cancel_subagent",
            &context,
            &serde_json::json!({"id":h.id()}).to_string(),
        )
        .await
        .unwrap();
    assert!(!cancelled.is_error);
    settled(&h).await;
    assert_eq!(h.status(), SubagentStatus::Cancelled);
}

#[cfg(unix)]
async fn assert_unknown_process_settlement_requires_explicit_retry(initial: usize, use_bash: bool) {
    let mode = Arc::new(AtomicUsize::new(initial));
    let inspections = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Semaphore::new(0));
    let observed = Arc::new(Mutex::new(None));
    let tools = ToolRegistry::new();
    let (m, c, s, o) = (
        Arc::clone(&mode),
        Arc::clone(&inspections),
        Arc::clone(&started),
        Arc::clone(&observed),
    );
    tools.register(ToolDefinition::new_boxed(
        if use_bash { "bash" } else { "read" },
        "owned cleanup fixture",
        "{}",
        Box::new(move |ctx, args| {
            let (m, c, s, o) = (
                Arc::clone(&m),
                Arc::clone(&c),
                Arc::clone(&s),
                Arc::clone(&o),
            );
            Box::pin(async move {
                let control = Arc::clone(&ctx.process_settlement);
                control.set_inspector(move |_| {
                    c.fetch_add(1, Ordering::SeqCst);
                    match m.load(Ordering::SeqCst) {
                        0 => Err("fixture ps failed: unknown process state".into()),
                        1 => Ok(false),
                        _ => Ok(true),
                    }
                });
                *o.lock() = Some(Arc::clone(&control));
                if use_bash {
                    s.add_permits(1);
                    crate::tools::fs::exec_bash(ctx, args).await
                } else {
                    ctx.exec
                        .as_ref()
                        .unwrap()
                        .spawn("sleep", &["60".into()])
                        .unwrap();
                    s.add_permits(1);
                    ToolResult::ok("read", "process spawned")
                }
            })
        }),
    ));
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let mut parent = Agent::new();
    parent.set_workspace_root(&root);
    parent.set_policy(Policy::full_access());
    parent.os_sandbox = None;
    let mut manager = SubagentManager::new()
        .with_tools(Arc::new(tools))
        .with_authority(parent.subagent_authority());
    if use_bash {
        manager.set_provider(Arc::new(ProcessProvider {
            arguments: r#"{"command":"exec sleep 60"}"#.into(),
        }));
    } else {
        manager.set_provider(Arc::new(RoundProvider {
            requests: Arc::new(Mutex::new(Vec::new())),
        }));
    }
    let events = Arc::new(AtomicUsize::new(0));
    let event_count = Arc::clone(&events);
    manager.subscribe(move |event| {
        if matches!(event, SubagentEvent::Finished { .. }) {
            event_count.fetch_add(1, Ordering::SeqCst);
        }
    });
    let worker = manager
        .spawn_background(
            SubagentConfig {
                permission_mode: Some(PermissionMode::FullAccess),
                ..Default::default()
            },
            "owned task",
            &root,
        )
        .unwrap();
    entered(&started, 1).await;
    let before = Instant::now();
    let snapshots = manager
        .cancel_tree_and_wait(worker.id(), Duration::from_millis(10))
        .await
        .unwrap();
    assert!(before.elapsed() < Duration::from_secs(1));
    assert_eq!(snapshots[0].status, SubagentStatus::Stopping);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if worker.snapshot().settlement_error.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("explicit uncertainty recorded");
    assert!(worker.result().is_none());
    assert_eq!(events.load(Ordering::SeqCst), 0);
    let control = observed.lock().clone().unwrap();
    assert_eq!(control.signal_count(), 1);
    let count = inspections.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        inspections.load(Ordering::SeqCst),
        count,
        "no uncontrolled inspection loop while unknown"
    );
    mode.store(1, Ordering::SeqCst);
    worker.retry_settlement().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while inspections.load(Ordering::SeqCst) == count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(worker.status(), SubagentStatus::Stopping);
    assert_eq!(events.load(Ordering::SeqCst), 0);
    assert_eq!(control.signal_count(), 1);
    mode.store(2, Ordering::SeqCst);
    worker.retry_settlement().unwrap();
    settled(&worker).await;
    assert_eq!(worker.status(), SubagentStatus::Cancelled);
    assert_eq!(events.load(Ordering::SeqCst), 1);
    assert_eq!(control.signal_count(), 1);
    assert!(worker.snapshot().settlement_error.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn failed_bash_inspection_stays_stopping_until_explicit_retry() {
    assert_unknown_process_settlement_requires_explicit_retry(0, true).await;
}

#[cfg(unix)]
#[tokio::test]
async fn persistent_exec_live_or_exiting_observation_is_bounded_and_never_resignalled() {
    assert_unknown_process_settlement_requires_explicit_retry(1, false).await;
}

#[cfg(unix)]
fn process_executes(pid: u32) -> bool {
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&output.stdout);
    !state.trim().is_empty() && !state.trim().starts_with('Z')
}

#[cfg(unix)]
#[tokio::test]
async fn normal_bash_worker_waits_for_shell_exits_first_descendants_before_finished() {
    for redirect in ["", ">/dev/null 2>&1"] {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().canonicalize().unwrap();
        let release = root.join("release");
        let marker = root.join("pid");
        let command=format!("/bin/sh -c 'while [ ! -f \"$1\" ]; do sleep 0.02; done' sh '{}' {redirect} & child=$!; echo $child > '{}'; echo $child; exit 0",release.display(),marker.display());
        let tools = ToolRegistry::new();
        crate::tools::register_builtin_tools(&tools);
        let mut parent = Agent::new();
        parent.set_workspace_root(&root);
        parent.set_policy(Policy::full_access());
        parent.os_sandbox = None;
        let mut manager = SubagentManager::new()
            .with_authority(parent.subagent_authority())
            .with_tools(Arc::new(tools))
            .with_provider(Arc::new(ProcessProvider {
                arguments: serde_json::json!({"command":command,"timeout":5}).to_string(),
            }));
        let pid = Arc::new(Mutex::new(None));
        let finished = Arc::new(AtomicUsize::new(0));
        let live_at_finish = Arc::new(AtomicUsize::new(0));
        let (p, f, l) = (
            Arc::clone(&pid),
            Arc::clone(&finished),
            Arc::clone(&live_at_finish),
        );
        manager.subscribe(move |event| {
            if matches!(event, SubagentEvent::Finished { .. }) {
                if p.lock().is_some_and(process_executes) {
                    l.fetch_add(1, Ordering::SeqCst);
                }
                f.fetch_add(1, Ordering::SeqCst);
            }
        });
        let h = manager
            .spawn_background(SubagentConfig::default(), "normal shell exit", &root)
            .unwrap();
        let child = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&marker) {
                    if let Ok(id) = text.trim().parse::<u32>() {
                        break id;
                    }
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        *pid.lock() = Some(child);
        let live_before_release = process_executes(child);
        let early_finished = finished.load(Ordering::SeqCst);
        std::fs::write(&release, "").unwrap();
        let result = settled(&h).await;
        assert!(live_before_release);
        assert_eq!(
            early_finished, 0,
            "normal shell exit discarded a live descendant"
        );
        assert!(result.error.is_none());
        assert_eq!(h.status(), SubagentStatus::Completed);
        assert_eq!(finished.load(Ordering::SeqCst), 1);
        assert_eq!(live_at_finish.load(Ordering::SeqCst), 0);
        assert!(!process_executes(child));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn exec_wait_worker_finished_requires_inherited_and_redirected_descendants_stopped() {
    for redirect in ["", ">/dev/null 2>&1"] {
        let pid = Arc::new(Mutex::new(None));
        let p = Arc::clone(&pid);
        let command = format!("sleep 2 {redirect} & echo $!; exit 0");
        let tools = ToolRegistry::new();
        tools.register(ToolDefinition::new_boxed(
            "read",
            "owned exec wait fixture",
            "{}",
            Box::new(move |ctx, _| {
                let command = command.clone();
                let p = Arc::clone(&p);
                Box::pin(async move {
                    tokio::task::spawn_blocking(move || {
                        let registry = ctx.exec.as_ref().unwrap();
                        let process = registry.spawn("/bin/sh", &["-c".into(), command]).unwrap();
                        match registry.wait_timeout(&process.process_id, Duration::from_millis(20))
                        {
                            Ok(output) => {
                                *p.lock() = Some(output.stdout.trim().parse::<u32>().unwrap());
                                ToolResult::ok("read", output.stdout)
                            }
                            Err(error) => ToolResult::err("read", error),
                        }
                    })
                    .await
                    .unwrap()
                })
            }),
        ));
        let mut manager = SubagentManager::new()
            .with_tools(Arc::new(tools))
            .with_provider(Arc::new(RoundProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            }));
        let finished = Arc::new(AtomicUsize::new(0));
        let live_at_finish = Arc::new(AtomicUsize::new(0));
        let (p, f, l) = (
            Arc::clone(&pid),
            Arc::clone(&finished),
            Arc::clone(&live_at_finish),
        );
        manager.subscribe(move |event| {
            if matches!(event, SubagentEvent::Finished { .. }) {
                if p.lock().is_some_and(process_executes) {
                    l.fetch_add(1, Ordering::SeqCst);
                }
                f.fetch_add(1, Ordering::SeqCst);
            }
        });
        let h = manager
            .spawn_background(
                SubagentConfig::default(),
                "exec wait fixture",
                Path::new("."),
            )
            .unwrap();
        settled(&h).await;
        assert!(pid.lock().is_some());
        assert_eq!(finished.load(Ordering::SeqCst), 1);
        assert_eq!(live_at_finish.load(Ordering::SeqCst), 0);
        assert!(!process_executes(pid.lock().unwrap()));
    }
}
