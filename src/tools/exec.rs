use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecProcess {
    pub process_id: String,
}

pub struct ExecSession {
    pub process_id: String,
    child: Child,
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
        let child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn {program}: {e}"))?;
        let process_id = Uuid::new_v4().to_string();
        self.sessions.lock().insert(
            process_id.clone(),
            ExecSession {
                process_id: process_id.clone(),
                child,
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

    pub fn kill(&self, process_id: &str) -> Result<(), String> {
        let mut sessions = self.sessions.lock();
        let mut session = sessions
            .remove(process_id)
            .ok_or_else(|| format!("unknown process_id {process_id}"))?;
        session
            .child
            .kill()
            .map_err(|e| format!("kill failed: {e}"))?;
        let _ = session.child.wait();
        Ok(())
    }
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
}
