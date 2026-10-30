#![cfg(all(feature = "script", unix))]
use rx4::agent::{ToolContext, ToolRegistry};
use std::{path::Path, sync::Arc, time::Duration};

fn context(root: &Path) -> ToolContext {
    let mut ctx = ToolContext::new(root);
    ctx.script_worker = Some(env!("CARGO_BIN_EXE_rx4-script-worker").into());
    ctx
}
async fn execute(ctx: ToolContext, code: &str) -> rx4::agent::ToolResult {
    let registry = ToolRegistry::new();
    rx4::tools::register_builtin_tools(&registry);
    registry
        .execute(
            "script",
            &Arc::new(ctx),
            &serde_json::json!({"code":code,"timeout_ms":1000}).to_string(),
        )
        .await
        .unwrap()
}
fn wrapper(root: &Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("helper");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn tracked_context(root: &Path) -> ToolContext {
    let mut ctx = context(root);
    ctx.script_worker = Some(wrapper(
        root,
        &format!(
            "echo $$ > '{}'\nexec '{}'",
            root.join("pid").display(),
            env!("CARGO_BIN_EXE_rx4-script-worker")
        ),
    ));
    ctx
}
async fn pid(root: &Path) -> String {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(root.join("pid")) {
                if !pid.trim().is_empty() {
                    return pid.trim().into();
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
fn alive(pid: &str) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}
#[tokio::test]
async fn scalar_tables_protected_calls_and_limits() {
    let root = tempfile::tempdir().unwrap();
    for (code,expected) in [
        ("return 42","42"), ("return json.encode({})","{}"),
        ("return json.encode({1,2})","[1,2]"), ("print({a='b'})","{\"a\":\"b\"}"),
        ("local e={} local ok,v=pcall(error,e); return not ok and v==e","true"),
        ("local ok,v=xpcall(function() error('x') end,function(e) return 'handled' end); return v","handled"),
        ("return os==nil and io==nil and ffi==nil and require==nil","true"),
    ] {
        let result=execute(context(root.path()),code).await;
        assert!(!result.is_error && result.content.contains(expected),"{}",result.content);
    }
    for code in [
        "local t={} t[1]=t; return t",
        "local t={} local p=t for i=1,100 do p[1]={};p=p[1] end return t",
        "local t={} for i=1,5000 do t[i]=i end return t",
    ] {
        assert!(execute(context(root.path()), code).await.is_error);
    }
}
#[tokio::test]
async fn cpu_native_and_protected_timeouts_are_reaped() {
    for code in [
        "return string.find(string.rep('a',20000), 'a*a*a*a*b')",
        "while true do pcall(function() while true do end end) end",
        "while true do xpcall(function() while true do end end,function(e) return e end) end",
    ] {
        let root = tempfile::tempdir().unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            execute(tracked_context(root.path()), code),
        )
        .await
        .unwrap();
        assert!(
            result.is_error && result.content.contains("timed out"),
            "{}",
            result.content
        );
        assert!(!alive(&pid(root.path()).await));
    }
}
#[tokio::test(flavor = "current_thread")]
async fn cancelled_native_matcher_does_not_block_heartbeat_and_is_reaped() {
    let root = tempfile::tempdir().unwrap();
    let ctx = tracked_context(root.path());
    let task = tokio::spawn(execute(
        ctx,
        "return string.find(string.rep('a',20000), 'a*a*a*a*b')",
    ));
    let process = pid(root.path()).await;
    task.abort();
    tokio::time::timeout(
        Duration::from_millis(300),
        tokio::time::sleep(Duration::from_millis(100)),
    )
    .await
    .unwrap();
    let _ = task.await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while alive(&process) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn parent_bridges_preserve_workspace_and_sandbox_authority() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "TODO marker").unwrap();
    std::fs::write(outside.path().join("secret.txt"), "OUTSIDE").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
    let result = execute(
        context(root.path()),
        "return read('a.txt') .. ':' .. #grep('TODO','.')",
    )
    .await;
    assert!(
        !result.is_error && result.content.contains("TODO marker:1"),
        "{}",
        result.content
    );
    for code in ["return read('escape/secret.txt')", "return list('escape')"] {
        assert!(execute(context(root.path()), code).await.is_error);
    }
    let result = execute(context(root.path()), "return #grep('OUTSIDE','.')").await;
    assert!(
        !result.is_error && result.content.contains("0"),
        "{}",
        result.content
    );
    let mut ctx = context(root.path());
    let mut config = rx4::sandbox::SandboxConfig::new(
        rx4::sandbox::SandboxProfile::Workspace,
        root.path().into(),
    );
    config
        .deny_paths
        .push(root.path().join("a.txt").canonicalize().unwrap());
    ctx.sandbox = Some(Arc::new(rx4::sandbox::SandboxManager::from_config(config)));
    assert!(execute(ctx, "return read('a.txt')").await.is_error);
    std::fs::write(root.path().join("big"), vec![b'a'; 1048577]).unwrap();
    assert!(
        execute(context(root.path()), "return #read('big')")
            .await
            .is_error
    );
}
#[tokio::test]
async fn missing_version_oversize_and_environment_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let mut ctx = context(root.path());
    ctx.script_worker = Some(root.path().join("absent"));
    assert!(execute(ctx, "return 1").await.is_error);
    let mut ctx = context(root.path());
    ctx.script_worker = Some(wrapper(root.path(), "printf '\\000\\000\\000\\002{}'"));
    assert!(execute(ctx, "return 1").await.content.contains("version"));
    let mut ctx = context(root.path());
    ctx.script_worker = Some(wrapper(root.path(), "printf '\\177\\000\\000\\000'"));
    assert!(execute(ctx, "return 1").await.content.contains("frame"));
    let mut ctx = context(root.path());
    ctx.script_worker = Some(wrapper(
        root.path(),
        &format!(
            "/usr/bin/env > '{}'\nexec '{}'",
            root.path().join("env").display(),
            env!("CARGO_BIN_EXE_rx4-script-worker")
        ),
    ));
    assert!(!execute(ctx, "return 1").await.is_error);
    let env = std::fs::read_to_string(root.path().join("env")).unwrap();
    assert!(
        !env.contains("HOME=") && !env.contains("TOKEN=") && !env.contains("PATH="),
        "{env}"
    );
}

#[tokio::test]
async fn explicit_limits_and_required_sandbox_fail_without_truncation() {
    let root = tempfile::tempdir().unwrap();
    assert!(execute(context(root.path()), &"x".repeat(65537))
        .await
        .content
        .contains("64 KiB"));
    assert!(
        execute(context(root.path()), "return string.rep('x',32769)")
            .await
            .is_error
    );
    let mut ctx = tracked_context(root.path());
    ctx.os_sandbox_required = true;
    assert!(execute(ctx, "return 1")
        .await
        .content
        .contains("OS sandbox required"));
    assert!(!root.path().join("pid").exists());
}

#[tokio::test]
async fn stalled_and_exited_helpers_are_owned_and_reaped() {
    for command in ["exec /bin/sleep 30", "exit 7"] {
        let root = tempfile::tempdir().unwrap();
        let mut ctx = context(root.path());
        ctx.script_worker = Some(wrapper(
            root.path(),
            &format!(
                "echo $$ > '{}'\n{command}",
                root.path().join("pid").display()
            ),
        ));
        let result = tokio::time::timeout(Duration::from_secs(3), execute(ctx, "return 1"))
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(!alive(&pid(root.path()).await));
    }
}
#[tokio::test]
async fn bridge_redaction_happens_before_untrusted_code_can_transform_secrets() {
    let root = tempfile::tempdir().unwrap();
    let token = format!("sk-{}", "a".repeat(36));
    std::fs::write(root.path().join("key.txt"), &token).unwrap();
    let result=execute(context(root.path()),"local s=read('key.txt'); local out={} for i=1,#s do out[i]=tostring(string.byte(s,i)) end return table.concat(out,',')").await;
    assert!(!result.is_error, "{}", result.content);
    let encoded = token
        .bytes()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join(",");
    assert!(!result.content.contains(&encoded));
}

#[tokio::test]
async fn helper_native_deadline_stops_even_without_parent_supervision() {
    use std::io::{Read, Write};
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_rx4-script-worker"))
        .env_clear()
        .current_dir("/")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut header = [0; 4];
    stdout.read_exact(&mut header).unwrap();
    let mut hello = vec![0; u32::from_be_bytes(header) as usize];
    stdout.read_exact(&mut hello).unwrap();
    let request=serde_json::to_vec(&serde_json::json!({"version":1,"kind":"start","timeout_ms":1000,"code":"return string.find(string.rep('a',20000),'a*a*a*a*b')"})).unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&(request.len() as u32).to_be_bytes())
        .unwrap();
    stdin.write_all(&request).unwrap();
    stdin.flush().unwrap();
    drop(stdin);
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(124));
            break;
        }
        if started.elapsed() > Duration::from_secs(3) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("native helper exceeded its independent deadline");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[cfg(feature = "providers")]
mod agents {
    use super::*;
    use rx4::{
        agent::{Agent, ToolCall},
        provider::{Message, Provider, ProviderError, StreamEvent, StreamResult},
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Fixture {
        calls: AtomicUsize,
        code: String,
    }
    #[async_trait::async_trait]
    impl Provider for Fixture {
        fn id(&self) -> &str {
            "script-fixture"
        }
        fn name(&self) -> &str {
            "script-fixture"
        }
        async fn stream(
            &self,
            _: &[Message],
            _: &Option<String>,
            _: &str,
            _: &[serde_json::Value],
            _: Option<&str>,
        ) -> Result<StreamResult, ProviderError> {
            let events = if self.calls.fetch_add(1, Ordering::SeqCst).is_multiple_of(2) {
                vec![
                    Ok(StreamEvent::ToolCall(ToolCall {
                        id: "script-call".into(),
                        name: "script".into(),
                        arguments: serde_json::json!({"code":self.code,"timeout_ms":1000})
                            .to_string(),
                    })),
                    Ok(StreamEvent::Done),
                ]
            } else {
                vec![Ok(StreamEvent::Done)]
            };
            Ok(Box::new(futures::stream::iter(events)))
        }
    }
    fn agent(root: &Path, code: &str) -> Agent {
        let registry = ToolRegistry::new();
        rx4::tools::register_builtin_tools(&registry);
        let mut agent = Agent::new();
        agent.set_tools(registry);
        agent.set_workspace_root(root);
        agent.set_scope(rx4::Scope::Coding);
        agent.set_policy(rx4::Policy::full_access());
        agent.os_sandbox = None;
        agent.script_worker = tracked_context(root).script_worker;
        agent.set_provider(Arc::new(Fixture {
            calls: AtomicUsize::new(0),
            code: code.into(),
        }));
        agent
    }
    #[tokio::test]
    async fn script_and_bridge_dispatch_keep_parent_authorization() {
        for denied in ["script", "read"] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("a.txt"), "PRIVATE_MARKER").unwrap();
            let mut parent = agent(root.path(), "return read('a.txt')");
            parent.async_tools.enabled = false;
            let mut policy = rx4::Policy::full_access();
            policy.denylist.push(denied.into());
            parent.set_policy(policy);
            parent.os_sandbox = None;
            parent.prompt("inspect").await.unwrap();
            let messages = parent.messages_handle();
            let messages = messages.read();
            let result = messages
                .iter()
                .find(|message| message.role == rx4::provider::Role::Tool)
                .unwrap();
            assert!(result.content.contains("denied"), "{}", result.content);
            assert!(!result.content.contains("PRIVATE_MARKER"));
            if denied == "script" {
                assert!(!root.path().join("pid").exists());
            }
        }
    }

    struct AskRead;
    impl rx4::permissions::Authorizer for AskRead {
        fn authorize(
            &self,
            _: &rx4::Policy,
            tool: &str,
            _: &str,
            _: Option<&dyn rx4::permissions::Approver>,
            _: Option<&Path>,
        ) -> rx4::permissions::Decision {
            if tool == "read" {
                rx4::permissions::Decision::Ask
            } else {
                rx4::permissions::Decision::Allow
            }
        }
    }
    struct DenyReadApproval(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl rx4::permissions::AsyncApprover for DenyReadApproval {
        async fn approve(&self, _: &ToolCall) -> rx4::permissions::Decision {
            self.0.fetch_add(1, Ordering::SeqCst);
            rx4::permissions::Decision::Deny
        }
    }
    #[tokio::test]
    async fn bridge_approval_stays_with_parent() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "PRIVATE_MARKER").unwrap();
        let mut parent = agent(root.path(), "return read('a.txt')");
        parent.async_tools.enabled = false;
        let approvals = Arc::new(AtomicUsize::new(0));
        parent.set_authorizer(Arc::new(AskRead));
        parent.set_async_approver(Arc::new(DenyReadApproval(approvals.clone())));
        parent.prompt("inspect").await.unwrap();
        assert_eq!(approvals.load(Ordering::SeqCst), 1);
        let messages = parent.messages_handle();
        let messages = messages.read();
        let result = messages
            .iter()
            .find(|message| message.role == rx4::provider::Role::Tool)
            .unwrap();
        assert!(
            result.content.contains("approval denied")
                && !result.content.contains("PRIVATE_MARKER"),
            "{}",
            result.content
        );
    }

    struct BlockRead {
        entered: Arc<tokio::sync::Semaphore>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl rx4::permissions::Authorizer for BlockRead {
        fn authorize(
            &self,
            _: &rx4::Policy,
            tool: &str,
            _: &str,
            _: Option<&dyn rx4::permissions::Approver>,
            _: Option<&Path>,
        ) -> rx4::permissions::Decision {
            if tool == "read" {
                self.entered.add_permits(1);
                let _ = self.release.lock().unwrap().recv();
            }
            rx4::permissions::Decision::Allow
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn blocked_parent_bridge_retains_worker_until_drain_without_blocking_deadline() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "marker").unwrap();
        let mut parent = agent(root.path(), "return read('a.txt')");
        let entered = Arc::new(tokio::sync::Semaphore::new(0));
        let (release, rx) = std::sync::mpsc::channel();
        parent.set_authorizer(Arc::new(BlockRead {
            entered: entered.clone(),
            release: std::sync::Mutex::new(rx),
        }));
        let mut manager = rx4::SubagentManager::new()
            .with_provider(Arc::new(Fixture {
                calls: AtomicUsize::new(0),
                code: "return read('a.txt')".into(),
            }))
            .with_tools(parent.tools.clone())
            .with_authority(parent.subagent_authority());
        let handle = manager
            .spawn_background(rx4::SubagentConfig::default(), "inspect", root.path())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        let deadline = tokio::time::timeout(
            Duration::from_millis(200),
            manager.cancel_tree_and_wait(handle.id(), Duration::from_millis(10)),
        )
        .await;
        assert!(
            deadline.is_ok(),
            "manager deadline blocked the current-thread runtime"
        );
        assert_eq!(handle.status(), rx4::SubagentStatus::Stopping);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), handle.wait())
                .await
                .is_err()
        );
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), handle.wait())
            .await
            .unwrap();
        assert_eq!(handle.status(), rx4::SubagentStatus::Cancelled);
        assert!(!alive(&pid(root.path()).await));
    }

    #[tokio::test]
    async fn root_registry_reusable_and_worker_finished_requires_reaping() {
        let root = tempfile::tempdir().unwrap();
        let mut parent = agent(root.path(), "return 42");
        parent.async_tools.sync_window = Duration::ZERO;
        parent.prompt("first").await.unwrap();
        assert!(!alive(&pid(root.path()).await));
        parent.prompt("second").await.unwrap();
        assert!(!alive(&pid(root.path()).await));
        let process = parent
            .exec
            .spawn("/bin/sh", &["-c".into(), "exit 0".into()])
            .unwrap();
        parent
            .exec
            .wait_timeout(&process.process_id, Duration::from_secs(1))
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let parent = agent(root.path(), "return 42");
        let finished = Arc::new(tokio::sync::Semaphore::new(0));
        let notice = finished.clone();
        let file = root.path().join("pid");
        let mut manager = rx4::SubagentManager::new()
            .with_provider(Arc::new(Fixture {
                calls: AtomicUsize::new(0),
                code: "return string.find(string.rep('a',20000),'a*a*a*a*b')".into(),
            }))
            .with_tools(parent.tools.clone())
            .with_authority(parent.subagent_authority());
        manager.subscribe(move |event| {
            if matches!(event, rx4::SubagentEvent::Finished { .. }) {
                assert!(!alive(std::fs::read_to_string(&file).unwrap().trim()));
                notice.add_permits(1);
            }
        });
        let handle = manager
            .spawn_background(
                rx4::SubagentConfig::default(),
                "exercise script",
                root.path(),
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), handle.wait())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), finished.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
    }
    #[tokio::test(flavor = "current_thread")]
    async fn root_cancellation_settles_scripts_without_blocking_runtime() {
        let root = tempfile::tempdir().unwrap();
        let mut agent = agent(
            root.path(),
            "return string.find(string.rep('a',20000),'a*a*a*a*b')",
        );
        let cancel = agent.cancellation_handle();
        let run = tokio::spawn(async move {
            let _ = agent.prompt("run").await;
            agent
        });
        let process = pid(root.path()).await;
        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let agent = tokio::time::timeout(Duration::from_secs(3), run)
            .await
            .unwrap()
            .unwrap();
        assert!(!alive(&process));
        assert!(agent
            .exec
            .spawn("/bin/sh", &["-c".into(), "exit 0".into()])
            .is_ok());
        agent.exec.shutdown().unwrap();
    }
}
