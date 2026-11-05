use crate::agent::{ToolContext, ToolFuture, ToolResult};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Default wait cap so `exec wait` cannot pin the agent loop for hours.
/// Matches bash's default tool timeout. Override per-call via `timeout`.
pub const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 120;
pub const MAX_WAIT_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecProcess {
    pub process_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecOutput {
    pub process_id: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct ProcessLifecycle {
    pub start: bool,
    pub process_id: String,
    pub program: Option<String>,
    pub exit_code: Option<i32>,
}

pub struct ExecSession {
    pub process_id: String,
    child: Child,
    group: OwnedProcessGroup,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    protocol_done: Option<Arc<std::sync::atomic::AtomicBool>>,
}

#[derive(Default)]
pub struct ExecRegistry {
    sessions: Mutex<HashMap<String, ExecSession>>,
    settlement: Arc<ProcessSettlementControl>,
    closed: std::sync::atomic::AtomicBool,
    cleanup_operation: Mutex<()>,
    #[cfg(feature = "script")]
    scripts: Mutex<Vec<Arc<ScriptLease>>>,
}

#[cfg(feature = "script")]
#[derive(Default)]
pub(crate) struct ScriptLease {
    owner: Option<String>,
    pub started: std::sync::atomic::AtomicBool,
    pub cancel: std::sync::atomic::AtomicBool,
    pub done: std::sync::atomic::AtomicBool,
    pub changed: tokio::sync::Notify,
}
#[cfg(feature = "script")]
impl ScriptLease {
    pub fn cancel(&self) {
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        self.changed.notify_waiters();
    }
    pub fn finish(&self) {
        self.done.store(true, std::sync::atomic::Ordering::SeqCst);
        self.changed.notify_waiters();
    }
    pub async fn cancelled(&self) {
        loop {
            let notice = self.changed.notified();
            if self.cancel.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            notice.await;
        }
    }
}
#[cfg(feature = "script")]
pub(crate) struct ScriptDispatchGuard(pub Arc<ScriptLease>);
#[cfg(feature = "script")]
impl Drop for ScriptDispatchGuard {
    fn drop(&mut self) {
        if !self.0.started.load(std::sync::atomic::Ordering::SeqCst) {
            self.0.finish();
        }
    }
}
#[cfg(feature = "script")]
pub(crate) struct ProtocolProcess {
    pub id: String,
    pub stdin: std::process::ChildStdin,
    pub stdout: std::process::ChildStdout,
    pub done: Arc<std::sync::atomic::AtomicBool>,
}

impl ExecRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    #[cfg(feature = "script")]
    pub(crate) fn with_settlement_control(settlement: Arc<ProcessSettlementControl>) -> Self {
        Self {
            settlement,
            ..Self::default()
        }
    }

    pub fn spawn(self: &Arc<Self>, program: &str, args: &[String]) -> Result<ExecProcess, String> {
        let mut sessions = self.sessions.lock();
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("process registry is shutting down".into());
        }
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn {program}: {e}"))?;
        let stdout_buf = Arc::new(Mutex::new(Vec::new()));
        let stderr_buf = Arc::new(Mutex::new(Vec::new()));
        let stdout_thread = child
            .stdout
            .take()
            .map(|pipe| drain_pipe(pipe, Arc::clone(&stdout_buf)));
        let stderr_thread = child
            .stderr
            .take()
            .map(|pipe| drain_pipe(pipe, Arc::clone(&stderr_buf)));
        let process_id = Uuid::new_v4().to_string();
        sessions.insert(
            process_id.clone(),
            ExecSession {
                process_id: process_id.clone(),
                group: OwnedProcessGroup::new(child.id()),
                child,
                stdout: stdout_buf,
                stderr: stderr_buf,
                stdout_thread,
                stderr_thread,
                protocol_done: None,
            },
        );
        Ok(ExecProcess { process_id })
    }

    #[cfg(all(test, feature = "script"))]
    pub(crate) fn reserve_script(&self) -> Arc<ScriptLease> {
        self.reserve_script_for(None)
    }

    #[cfg(feature = "script")]
    pub(crate) fn reserve_script_for(&self, owner: Option<&str>) -> Arc<ScriptLease> {
        let lease = Arc::new(ScriptLease {
            owner: owner.map(str::to_owned),
            ..ScriptLease::default()
        });
        self.scripts.lock().push(lease.clone());
        lease
    }

    #[cfg(all(test, feature = "script"))]
    pub(crate) async fn wait_scripts(&self) {
        self.wait_scripts_for(None).await;
    }

    #[cfg(feature = "script")]
    pub(crate) async fn wait_scripts_for(&self, owner: Option<&str>) {
        let leases = self
            .scripts
            .lock()
            .iter()
            .filter(|lease| owner.is_none_or(|owner| lease.owner.as_deref() == Some(owner)))
            .cloned()
            .collect::<Vec<_>>();
        for lease in leases {
            loop {
                let notice = lease.changed.notified();
                if lease.done.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                notice.await;
            }
        }
        self.scripts
            .lock()
            .retain(|lease| !lease.done.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[cfg(feature = "script")]
    pub(crate) fn spawn_protocol(&self, mut cmd: Command) -> Result<ProtocolProcess, String> {
        #[cfg(not(unix))]
        return Err("isolated scripts require supported Unix process settlement".into());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let mut sessions = self.sessions.lock();
            if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("process registry is shutting down".into());
            }
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0);
            let mut child = cmd
                .spawn()
                .map_err(|e| format!("script helper spawn failed: {e}"))?;
            let stdin = child.stdin.take().expect("configured stdin");
            let stdout = child.stdout.take().expect("configured stdout");
            let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let id = Uuid::new_v4().to_string();
            sessions.insert(
                id.clone(),
                ExecSession {
                    process_id: id.clone(),
                    group: OwnedProcessGroup::new(child.id()),
                    child,
                    stdout: Arc::new(Mutex::new(Vec::new())),
                    stderr: Arc::new(Mutex::new(Vec::new())),
                    stdout_thread: None,
                    stderr_thread: None,
                    protocol_done: Some(done.clone()),
                },
            );
            Ok(ProtocolProcess {
                id,
                stdin,
                stdout,
                done,
            })
        }
    }

    #[cfg(feature = "script")]
    pub(crate) fn terminate_protocol(&self, id: &str) -> Result<(), String> {
        let _operation = self.cleanup_operation.lock();
        let mut sessions = self.sessions.lock();
        // Another registered cleanup owner may already have proved/reaped it.
        let Some(session) = sessions.get_mut(id) else {
            return Ok(());
        };
        session
            .group
            .try_settle(&self.settlement)
            .inspect_err(|error| self.settlement.record_unknown(error.clone()))
    }

    #[cfg(feature = "script")]
    pub(crate) fn finish_protocol(&self, id: &str) -> Result<(), String> {
        self.kill_if_present(id)?;
        if self.sessions.lock().is_empty() {
            self.settlement.clear();
        }
        Ok(())
    }

    pub fn settlement_control(&self) -> Arc<ProcessSettlementControl> {
        Arc::clone(&self.settlement)
    }

    /// One bounded cleanup attempt. Unknown sessions stay owned for explicit retry.
    pub fn shutdown(&self) -> Result<(), String> {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        let _operation = self.cleanup_operation.lock();
        let sessions = std::mem::take(&mut *self.sessions.lock());
        let mut pending = HashMap::new();
        let mut error = None;
        for (id, mut session) in sessions {
            drop(session.child.stdin.take());
            let result = session.group.try_settle(&self.settlement).and_then(|()| {
                #[cfg(not(unix))]
                let _ = session.child.kill();
                finish_session(&mut session).map(|_| ())
            });
            if let Err(e) = result {
                pending.insert(id, session);
                error = Some(e);
            }
        }
        self.sessions.lock().extend(pending);
        if let Some(error) = error {
            self.settlement.record_unknown(error.clone());
            Err(error)
        } else {
            self.settlement.clear();
            Ok(())
        }
    }

    pub fn write_stdin(&self, process_id: &str, data: &[u8]) -> Result<usize, String> {
        let mut sessions = self.sessions.lock();
        let session = sessions
            .get_mut(process_id)
            .ok_or_else(|| format!("unknown process_id {process_id}"))?;
        let stdin = session
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| "process stdin closed".to_string())?;
        stdin
            .write_all(data)
            .map_err(|e| format!("write_stdin failed: {e}"))?;
        stdin
            .flush()
            .map_err(|e| format!("flush stdin failed: {e}"))?;
        Ok(data.len())
    }

    pub fn wait(&self, process_id: &str) -> Result<ExecOutput, String> {
        self.wait_timeout(process_id, Duration::from_secs(DEFAULT_WAIT_TIMEOUT_SECS))
    }

    pub fn wait_timeout(&self, process_id: &str, timeout: Duration) -> Result<ExecOutput, String> {
        let _operation = self.cleanup_operation.lock();
        let mut session = self
            .sessions
            .lock()
            .remove(process_id)
            .ok_or_else(|| format!("unknown process_id {process_id}"))?;
        // Close stdin so readers like `cat`/`python` don't wait forever for EOF.
        drop(session.child.stdin.take());
        let result = finish_session_timeout(&mut session, timeout, &self.settlement);
        if let Err(error) = &result {
            self.settlement.record_unknown(error.clone());
            self.sessions.lock().insert(process_id.into(), session);
        }
        result
    }

    pub fn kill(&self, process_id: &str) -> Result<ExecOutput, String> {
        self.kill_if_present(process_id)?
            .ok_or_else(|| format!("unknown process_id {process_id}"))
    }

    fn kill_if_present(&self, process_id: &str) -> Result<Option<ExecOutput>, String> {
        let _operation = self.cleanup_operation.lock();
        let Some(mut session) = self.sessions.lock().remove(process_id) else {
            return Ok(None);
        };
        drop(session.child.stdin.take());
        let result = session.group.try_settle(&self.settlement).and_then(|()| {
            #[cfg(not(unix))]
            let _ = session.child.kill();
            finish_session(&mut session)
        });
        if let Err(error) = &result {
            self.settlement.record_unknown(error.clone());
            self.sessions.lock().insert(process_id.into(), session);
        }
        result.map(Some)
    }
}

type SettlementUnknownCallback = Arc<dyn Fn() + Send + Sync>;
#[cfg(test)]
type SettlementInspector = Arc<dyn Fn(u32) -> Result<bool, String> + Send + Sync>;

/// Process cleanup observation shared by an Agent and its worker handle.
/// An error means settlement is unknown. Retry is explicit and never re-signals a group.
#[derive(Default)]
pub struct ProcessSettlementControl {
    error: Mutex<Option<String>>,
    retry: tokio::sync::Notify,
    on_unknown: Mutex<Option<SettlementUnknownCallback>>,
    #[cfg(test)]
    inspector: Mutex<Option<SettlementInspector>>,
    #[cfg(test)]
    signals: std::sync::atomic::AtomicUsize,
}

impl ProcessSettlementControl {
    pub fn error(&self) -> Option<String> {
        self.error.lock().clone()
    }
    pub fn retry(&self) {
        if self.error.lock().is_some() {
            self.retry.notify_one();
        }
    }
    pub(crate) fn record_unknown(&self, error: String) {
        let callback = self.on_unknown.lock().clone();
        if let Some(callback) = callback {
            callback();
        }
        // Publish uncertainty only after the worker is Stopping. A concurrent
        // snapshot may see Stopping without details, never Running with unknown cleanup.
        *self.error.lock() = Some(crate::secrets::Redactor::new().redact(&error));
    }
    pub(crate) fn clear(&self) {
        *self.error.lock() = None;
    }
    pub(crate) fn on_unknown(&self, callback: impl Fn() + Send + Sync + 'static) {
        *self.on_unknown.lock() = Some(Arc::new(callback));
    }
    pub(crate) fn retry_notice(&self) -> tokio::sync::futures::Notified<'_> {
        self.retry.notified()
    }
    #[cfg(test)]
    pub(crate) fn set_inspector(
        &self,
        inspector: impl Fn(u32) -> Result<bool, String> + Send + Sync + 'static,
    ) {
        *self.inspector.lock() = Some(Arc::new(inspector));
    }
    #[cfg(test)]
    pub(crate) fn signal_count(&self) -> usize {
        self.signals.load(std::sync::atomic::Ordering::SeqCst)
    }
}

pub(crate) struct OwnedProcessGroup {
    pid: u32,
    signalled: bool,
    stopped: bool,
}
impl OwnedProcessGroup {
    pub(crate) fn new(pid: u32) -> Self {
        Self {
            pid,
            signalled: false,
            stopped: false,
        }
    }
    /// Observe without reaping or signalling. Retain the stopped observation
    /// with the unreaped Child so later pipe retries never signal a recycled ID.
    pub(crate) fn observe(
        &mut self,
        control: &ProcessSettlementControl,
        timeout: Duration,
    ) -> Result<bool, String> {
        if self.stopped {
            return Ok(true);
        }
        #[cfg(unix)]
        {
            #[cfg(test)]
            let injected = control.inspector.lock().clone();
            #[cfg(test)]
            let stopped = match injected {
                Some(inspector) => inspector(self.pid),
                None => inspect_process_group(self.pid, timeout),
            }?;
            #[cfg(not(test))]
            let stopped = {
                let _ = control;
                inspect_process_group(self.pid, timeout)
            }?;
            self.stopped = stopped;
            Ok(stopped)
        }
        #[cfg(not(unix))]
        {
            let _ = (control, timeout);
            Err("process-group observation is unavailable on this platform".into())
        }
    }

    /// One bounded attempt. Unknown/live states return without releasing ownership.
    pub(crate) fn try_settle(&mut self, control: &ProcessSettlementControl) -> Result<(), String> {
        if self.stopped {
            return Ok(());
        }
        #[cfg(unix)]
        {
            let deadline = Instant::now() + Duration::from_secs(1);
            if !self.signalled {
                // Reserve before attempting the signal: even an uncertain signal
                // result must never cause a recycled PGID to be signalled later.
                self.signalled = true;
                #[cfg(test)]
                control
                    .signals
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = bounded_control_command(
                    "/bin/kill",
                    &["-KILL", "--", &format!("-{}", self.pid)],
                    Duration::from_millis(500),
                );
            }
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(
                        "process group still live or exiting; inspection deadline reached".into(),
                    );
                }
                if self.observe(control, remaining.min(Duration::from_millis(500)))? {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(5).min(remaining));
            }
        }
        #[cfg(not(unix))]
        {
            let _ = control;
            Err("process-group settlement is unavailable on this platform".into())
        }
    }
}

#[cfg(unix)]
fn inspect_process_group(pid: u32, timeout: Duration) -> Result<bool, String> {
    let bytes = bounded_control_command("/bin/ps", &["-e", "-o", "pgid=", "-o", "stat="], timeout)?;
    let table = String::from_utf8(bytes)
        .map_err(|_| "process inspection returned invalid text".to_string())?;
    process_group_stopped(pid, &table)
}

#[cfg(unix)]
fn process_group_stopped(pid: u32, table: &str) -> Result<bool, String> {
    for line in table.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let group = fields
            .next()
            .and_then(|group| group.parse::<u32>().ok())
            .ok_or("invalid process group observation")?;
        let state = fields.next().ok_or("missing process state observation")?;
        if group == pid && !state.starts_with('Z') {
            return Ok(false);
        }
    }
    // Zombies cannot execute or hold pipe descriptors. The direct child stays
    // owned and is reaped by wait() after this observation succeeds.
    Ok(true)
}

#[cfg(unix)]
fn bounded_control_command(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("process inspection command unavailable: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or("process inspection output unavailable")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("process inspection command timed out".to_string());
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("process inspection wait failed: {e}"));
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "process inspection reader failed")?
        .map_err(|e| format!("process inspection read failed: {e}"))?;
    if !status?.success() {
        return Err("process inspection command failed".into());
    }
    if bytes.len() > 1024 * 1024 {
        return Err("process inspection output exceeded limit".into());
    }
    Ok(bytes)
}

fn drain_pipe<R: Read + Send + 'static>(mut pipe: R, buf: Arc<Mutex<Vec<u8>>>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut tmp = [0u8; 8192];
        loop {
            match pipe.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.lock().extend_from_slice(&tmp[..n]),
            }
        }
    })
}

fn readers_finished(session: &ExecSession) -> bool {
    session
        .protocol_done
        .as_ref()
        .is_none_or(|done| done.load(std::sync::atomic::Ordering::SeqCst))
        && session
            .stdout_thread
            .as_ref()
            .is_none_or(JoinHandle::is_finished)
        && session
            .stderr_thread
            .as_ref()
            .is_none_or(JoinHandle::is_finished)
}

/// Group termination is already proved by the caller. Keep the direct Child
/// unreaped until readers finish, so uncertainty retains its PID/PGID ownership.
fn finish_session(session: &mut ExecSession) -> Result<ExecOutput, String> {
    let deadline = Instant::now() + Duration::from_secs(1);
    while !readers_finished(session) {
        if Instant::now() >= deadline {
            return Err("process pipe readers have not settled before cleanup deadline".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let status = session
        .child
        .wait()
        .map_err(|e| format!("direct child reaping unconfirmed: {e}"))?;
    collect_output(session, status.code())
}

fn finish_session_timeout(
    session: &mut ExecSession,
    timeout: Duration,
    control: &ProcessSettlementControl,
) -> Result<ExecOutput, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            session.group.try_settle(control)?;
            let mut output = finish_session(session)?;
            output.exit_code = None;
            if !output.stderr.is_empty() {
                output.stderr.push('\n');
            }
            output.stderr.push_str(&format!(
                "exec wait timed out after {}s; process group stopped",
                timeout.as_secs()
            ));
            return Ok(output);
        }
        // try_wait() would reap the shell before live descendants were known
        // stopped. Observe the whole owned group first, including normal exit.
        if session.group.observe(control, Duration::from_millis(500))? && readers_finished(session)
        {
            return finish_session(session);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn collect_output(session: &mut ExecSession, exit_code: Option<i32>) -> Result<ExecOutput, String> {
    if !readers_finished(session) {
        return Err("process pipe readers are still active".into());
    }
    if let Some(handle) = session.stdout_thread.take() {
        handle.join().map_err(|_| "process stdout reader failed")?;
    }
    if let Some(handle) = session.stderr_thread.take() {
        handle.join().map_err(|_| "process stderr reader failed")?;
    }
    Ok(ExecOutput {
        process_id: session.process_id.clone(),
        stdout: String::from_utf8_lossy(&session.stdout.lock()).into_owned(),
        stderr: String::from_utf8_lossy(&session.stderr.lock()).into_owned(),
        exit_code,
    })
}

fn push_lifecycle(ctx: &ToolContext, notice: ProcessLifecycle) {
    if let Some(queue) = &ctx.process_lifecycle {
        queue.lock().push(notice);
    }
}

pub(crate) fn exec_tool(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || execute_exec(ctx, args))
            .await
            .unwrap_or_else(|e| ToolResult::err("exec", format!("exec task failed: {e}")))
    })
}

fn execute_exec(ctx: Arc<ToolContext>, args: String) -> ToolResult {
    let Some(registry) = ctx.exec.clone() else {
        return ToolResult::err("exec", "exec registry is not enabled");
    };
    let action = match crate::tools::common::parse_str_field(&args, "action") {
        Some(action) if !action.is_empty() => action,
        _ => return ToolResult::err("exec", "action required"),
    };
    match action.as_str() {
        "spawn" => {
            let Some(program) = crate::tools::common::parse_str_field(&args, "program") else {
                return ToolResult::err("exec", "program required");
            };
            let spawn_args = parse_args(&args);
            match registry.spawn(&program, &spawn_args) {
                Ok(proc) => {
                    push_lifecycle(
                        &ctx,
                        ProcessLifecycle {
                            start: true,
                            process_id: proc.process_id.clone(),
                            program: Some(program),
                            exit_code: None,
                        },
                    );
                    ToolResult::ok(
                        "exec",
                        serde_json::json!({"process_id": proc.process_id}).to_string(),
                    )
                }
                Err(e) => ToolResult::err("exec", e),
            }
        }
        "stdin" => {
            let Some(process_id) = crate::tools::common::parse_str_field(&args, "process_id")
            else {
                return ToolResult::err("exec", "process_id required");
            };
            let data = crate::tools::common::parse_str_field(&args, "data").unwrap_or_default();
            match registry.write_stdin(&process_id, data.as_bytes()) {
                Ok(n) => ToolResult::ok("exec", serde_json::json!({"bytes": n}).to_string()),
                Err(e) => ToolResult::err("exec", e),
            }
        }
        "wait" | "kill" => {
            let Some(process_id) = crate::tools::common::parse_str_field(&args, "process_id")
            else {
                return ToolResult::err("exec", "process_id required");
            };
            let result = if action == "kill" {
                registry.kill(&process_id)
            } else {
                let secs = crate::tools::common::parse_num_field(&args, "timeout")
                    .unwrap_or(DEFAULT_WAIT_TIMEOUT_SECS)
                    .clamp(1, MAX_WAIT_TIMEOUT_SECS);
                registry.wait_timeout(&process_id, Duration::from_secs(secs))
            };
            match result {
                Ok(output) => {
                    push_lifecycle(
                        &ctx,
                        ProcessLifecycle {
                            start: false,
                            process_id: output.process_id.clone(),
                            program: None,
                            exit_code: output.exit_code,
                        },
                    );
                    ToolResult::ok(
                        "exec",
                        serde_json::to_string(&output).unwrap_or_else(|_| "{}".into()),
                    )
                }
                Err(e) => ToolResult::err("exec", e),
            }
        }
        other => ToolResult::err("exec", format!("unknown action {other}")),
    }
}

fn parse_args(args: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(args) else {
        return Vec::new();
    };
    value
        .get("args")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn shell_exit_wait_settles_inherited_and_redirected_descendants() {
        for redirect in ["", ">/dev/null 2>&1"] {
            let registry = Arc::new(ExecRegistry::new());
            let command = format!("sleep 2 {redirect} & echo $!; exit 0");
            let process = registry.spawn("/bin/sh", &["-c".into(), command]).unwrap();
            let start = Instant::now();
            let output = registry
                .wait_timeout(&process.process_id, Duration::from_millis(20))
                .unwrap();
            let elapsed = start.elapsed();
            let pid = output.stdout.trim().parse::<u32>().unwrap();
            registry.shutdown().unwrap();
            let observed = Command::new("/bin/ps")
                .args(["-p", &pid.to_string(), "-o", "stat="])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&observed.stdout);
            assert!(
                state.trim().is_empty() || state.trim().starts_with('Z'),
                "descendant still executes after successful wait: {state}"
            );
            assert!(
                elapsed < Duration::from_secs(1),
                "inherited pipe join exceeded wait/cleanup bounds: {elapsed:?}"
            );
            assert!(output.exit_code.is_none());
            assert!(registry.sessions.lock().is_empty());
        }
    }

    #[cfg(all(unix, feature = "script"))]
    #[tokio::test(flavor = "current_thread")]
    async fn script_supervisor_unknown_cleanup_waits_for_explicit_retry() {
        let registry = Arc::new(ExecRegistry::new());
        let control = registry.settlement_control();
        let inspections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = inspections.clone();
        *control.inspector.lock() = Some(Arc::new(move |_| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("injected helper cleanup uncertainty".into())
        }));
        let root = tempfile::tempdir().unwrap();
        let mut ctx = ToolContext::new(root.path());
        // A trusted test executable that never sends hello exercises the real watchdog.
        ctx.script_worker = Some("/bin/sh".into());
        ctx.exec = Some(registry.clone());
        ctx.process_settlement = control.clone();
        let tools = crate::agent::ToolRegistry::new();
        crate::tools::register_builtin_tools(&tools);
        let task = tokio::spawn(async move {
            tools
                .execute(
                    "script",
                    &Arc::new(ctx),
                    r#"{"code":"return 42","timeout_ms":1000}"#,
                )
                .await
                .unwrap()
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while control.error().is_none() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(!task.is_finished());
        assert_eq!(registry.sessions.lock().len(), 1);
        let count = inspections.load(std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(inspections.load(std::sync::atomic::Ordering::SeqCst), count);
        assert_eq!(control.signals.load(std::sync::atomic::Ordering::SeqCst), 1);
        *control.inspector.lock() = None;
        control.retry();
        let result = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            result.is_error && result.content.contains("timed out"),
            "{}",
            result.content
        );
        assert_eq!(control.signals.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(registry.sessions.lock().is_empty());
    }

    #[cfg(feature = "script")]
    #[tokio::test]
    async fn script_run_drain_does_not_wait_for_another_owner() {
        let registry = ExecRegistry::new();
        let first = registry.reserve_script_for(Some("first"));
        let unrelated = registry.reserve_script_for(Some("other"));
        first.finish();
        tokio::time::timeout(
            Duration::from_millis(50),
            registry.wait_scripts_for(Some("first")),
        )
        .await
        .unwrap();
        assert!(!unrelated.done.load(std::sync::atomic::Ordering::SeqCst));
        unrelated.finish();
        registry.wait_scripts().await;
    }

    #[cfg(all(unix, feature = "script"))]
    #[tokio::test]
    async fn script_protocol_uncertainty_keeps_ownership_and_registry_reusable() {
        let registry = Arc::new(ExecRegistry::new());
        let lease = registry.reserve_script();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        command.env_clear();
        let process = registry.spawn_protocol(command).unwrap();
        *registry.settlement.inspector.lock() = Some(Arc::new(|_| {
            Err("injected script inspection failure".into())
        }));
        assert!(registry.terminate_protocol(&process.id).is_err());
        assert_eq!(registry.sessions.lock().len(), 1);
        assert_eq!(
            registry
                .settlement
                .signals
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(10), registry.wait_scripts())
                .await
                .is_err()
        );
        drop(process.stdin);
        drop(process.stdout);
        process
            .done
            .store(true, std::sync::atomic::Ordering::SeqCst);
        *registry.settlement.inspector.lock() = None;
        registry.finish_protocol(&process.id).unwrap();
        assert_eq!(
            registry
                .settlement
                .signals
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        lease.finish();
        registry.wait_scripts().await;
        let ordinary = registry
            .spawn("/bin/sh", &["-c".into(), "exit 0".into()])
            .unwrap();
        registry
            .wait_timeout(&ordinary.process_id, Duration::from_secs(1))
            .unwrap();
        assert!(!registry.closed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[cfg(unix)]
    #[test]
    fn pipe_cleanup_timeout_retains_unreaped_owner_until_reader_finishes() {
        let registry = Arc::new(ExecRegistry::new());
        let process = registry
            .spawn("/bin/sh", &["-c".into(), "exit 0".into()])
            .unwrap();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        {
            let mut sessions = registry.sessions.lock();
            let session = sessions.get_mut(&process.process_id).unwrap();
            session.stdout_thread.take().unwrap().join().unwrap();
            session.stdout_thread = Some(std::thread::spawn(move || {
                let _ = blocked.recv_timeout(Duration::from_secs(3));
            }));
        }
        let start = Instant::now();
        let result = registry.wait_timeout(&process.process_id, Duration::from_millis(20));
        let elapsed = start.elapsed();
        let retained = registry.sessions.lock().contains_key(&process.process_id);
        let signal_count = registry.settlement_control().signal_count();
        let _ = release.send(());
        registry.shutdown().unwrap();
        assert!(result.is_err());
        assert!(retained);
        assert!(elapsed < Duration::from_secs(2));
        assert_eq!(
            signal_count, 0,
            "a proved-stopped group must not be signalled while retaining pipe ownership"
        );
        assert!(registry.sessions.lock().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn inspection_commands_time_out_and_unavailable_commands_are_unknown() {
        let start = Instant::now();
        assert!(bounded_control_command("/bin/sleep", &["10"], Duration::from_millis(20)).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(bounded_control_command(
            "/nonexistent-rx4-inspector",
            &[],
            Duration::from_millis(20)
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn live_and_kernel_exit_states_are_unsettled_and_zombies_do_not_execute() {
        for state in ["R", "S", "?E"] {
            assert!(!process_group_stopped(42, &format!("42 {state}\n")).unwrap());
        }
        assert!(process_group_stopped(42, "42 Z\n").unwrap());
        assert!(process_group_stopped(42, "43 R\n").unwrap());
        assert!(process_group_stopped(42, "malformed observation").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn uncertain_shutdown_retains_ownership_and_explicit_retry_never_resignals() {
        let registry = Arc::new(ExecRegistry::new());
        registry.spawn("sleep", &["60".into()]).unwrap();
        let control = registry.settlement_control();
        control.set_inspector(|_| Err("fixture ps failure".into()));
        let start = Instant::now();
        assert!(registry.shutdown().is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(control.error().is_some());
        assert_eq!(registry.sessions.lock().len(), 1);
        assert_eq!(control.signal_count(), 1);
        assert!(registry.spawn("sleep", &["60".into()]).is_err());
        assert!(registry.shutdown().is_err());
        assert_eq!(control.signal_count(), 1);
        control.set_inspector(|_| Ok(true));
        registry.shutdown().unwrap();
        assert!(registry.sessions.lock().is_empty());
        assert_eq!(control.signal_count(), 1);
        assert!(control.error().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_terminates_process_descendants_and_joins_pipe_readers() {
        let workspace = tempfile::tempdir().unwrap();
        let marker = workspace.path().join("pids");
        let command = format!(
            "sleep 60 & child=$!; echo $$ $child > '{}'; wait $child",
            marker.display()
        );
        let registry = Arc::new(ExecRegistry::new());
        registry.spawn("bash", &["-c".into(), command]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let pids = loop {
            if let Ok(text) = std::fs::read_to_string(&marker) {
                if let Ok(pids) = text
                    .split_whitespace()
                    .map(str::parse::<u32>)
                    .collect::<Result<Vec<_>, _>>()
                {
                    if pids.len() == 2 {
                        break pids;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "fixture subprocess did not start"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        registry.shutdown().unwrap();
        assert!(registry.sessions.lock().is_empty());
        for pid in pids {
            let status = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "stat="])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&status.stdout);
            assert!(
                state.trim().is_empty() || state.trim().starts_with('Z'),
                "owned process still executes: {state}"
            );
        }
    }

    #[test]
    fn process_id_and_write_stdin() {
        let registry = Arc::new(ExecRegistry::new());
        let proc = registry.spawn("cat", &[]).expect("spawn cat");
        assert!(!proc.process_id.is_empty());
        let n = registry
            .write_stdin(&proc.process_id, b"hello\n")
            .expect("write");
        assert_eq!(n, 6);
        registry.kill(&proc.process_id).ok();
    }

    #[test]
    fn unknown_process_does_not_silent_pass() {
        let registry = ExecRegistry::new();
        assert!(registry.write_stdin("missing", b"x").is_err());
    }

    #[test]
    fn large_stdout_is_drained_without_deadlock() {
        let registry = Arc::new(ExecRegistry::new());
        let proc = registry
            .spawn("seq", &["1".into(), "20000".into()])
            .expect("spawn seq");
        let output = registry.wait(&proc.process_id).expect("wait");
        assert!(
            output.stdout.lines().count() >= 20_000,
            "stdout was not fully drained: {} lines",
            output.stdout.lines().count()
        );
        assert_eq!(output.exit_code, Some(0));
    }

    #[test]
    fn wait_closes_stdin_so_cat_exits() {
        let registry = Arc::new(ExecRegistry::new());
        let proc = registry.spawn("cat", &[]).expect("spawn cat");
        registry
            .write_stdin(&proc.process_id, b"ok\n")
            .expect("write");
        let output = registry
            .wait_timeout(&proc.process_id, Duration::from_secs(2))
            .expect("wait");
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "ok\n");
    }

    #[test]
    fn wait_kills_stuck_child_instead_of_hanging() {
        let registry = Arc::new(ExecRegistry::new());
        let proc = registry
            .spawn("sleep", &["30".into()])
            .expect("spawn sleep");
        let started = Instant::now();
        let output = registry
            .wait_timeout(&proc.process_id, Duration::from_millis(200))
            .expect("wait timeout");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "wait hung for {:?}",
            started.elapsed()
        );
        assert!(output.exit_code.is_none());
        assert!(output.stderr.contains("timed out"));
    }
}
