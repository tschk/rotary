use crate::agent::{ToolContext, ToolFuture, ToolResult};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;
use uuid::Uuid;

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
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub struct ExecRegistry {
    sessions: Mutex<HashMap<String, ExecSession>>,
}

impl ExecRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn(self: &Arc<Self>, program: &str, args: &[String]) -> Result<ExecProcess, String> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
        self.sessions.lock().insert(
            process_id.clone(),
            ExecSession {
                process_id: process_id.clone(),
                child,
                stdout: stdout_buf,
                stderr: stderr_buf,
                stdout_thread,
                stderr_thread,
            },
        );
        Ok(ExecProcess { process_id })
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
        let mut session = self
            .sessions
            .lock()
            .remove(process_id)
            .ok_or_else(|| format!("unknown process_id {process_id}"))?;
        finish_session(&mut session)
    }

    pub fn kill(&self, process_id: &str) -> Result<ExecOutput, String> {
        let mut session = self
            .sessions
            .lock()
            .remove(process_id)
            .ok_or_else(|| format!("unknown process_id {process_id}"))?;
        session
            .child
            .kill()
            .map_err(|e| format!("kill failed: {e}"))?;
        finish_session(&mut session)
    }
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

fn finish_session(session: &mut ExecSession) -> Result<ExecOutput, String> {
    let status = session
        .child
        .wait()
        .map_err(|e| format!("wait failed: {e}"))?;
    if let Some(handle) = session.stdout_thread.take() {
        let _ = handle.join();
    }
    if let Some(handle) = session.stderr_thread.take() {
        let _ = handle.join();
    }
    Ok(ExecOutput {
        process_id: session.process_id.clone(),
        stdout: String::from_utf8_lossy(&session.stdout.lock()).into_owned(),
        stderr: String::from_utf8_lossy(&session.stderr.lock()).into_owned(),
        exit_code: status.code(),
    })
}

fn push_lifecycle(ctx: &ToolContext, notice: ProcessLifecycle) {
    if let Some(queue) = &ctx.process_lifecycle {
        queue.lock().push(notice);
    }
}

pub(crate) fn exec_tool(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move { execute_exec(ctx, args) })
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
                registry.wait(&process_id)
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
}
