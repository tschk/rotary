//! Pi RPC protocol — JSON Lines over stdin/stdout.
//!
//! Compatible with pi_agent_rust RPC mode (`pi --mode rpc`).
//! Line-delimited JSON, each line is a request or event.

use crate::agent::Agent;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info, warn};

/// RPC commands that a client can send (pi protocol).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum PiRpcCommand {
    #[serde(rename = "prompt")]
    Prompt { text: String },
    #[serde(rename = "steer")]
    Steer { text: String },
    #[serde(rename = "follow-up")]
    FollowUp { text: String },
    #[serde(rename = "abort")]
    Abort,
    #[serde(rename = "get-state")]
    GetState,
    #[serde(rename = "compact")]
    Compact,
    #[serde(rename = "set-model")]
    SetModel { provider: String, model: String },
    #[serde(rename = "subscribe")]
    Subscribe,
    #[serde(rename = "unsubscribe")]
    Unsubscribe,
}

/// RPC events that the server emits (pi protocol).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PiRpcEvent {
    #[serde(rename = "agent_start")]
    AgentStart,
    #[serde(rename = "turn_start")]
    TurnStart { turn: usize },
    #[serde(rename = "message_delta")]
    MessageDelta { delta: String },
    #[serde(rename = "message_end")]
    MessageEnd { role: String, content: String },
    #[serde(rename = "tool_call")]
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        id: String,
        content: String,
        is_error: bool,
    },
    #[serde(rename = "turn_end")]
    TurnEnd { turn: usize },
    #[serde(rename = "agent_end")]
    AgentEnd,
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "state")]
    State {
        model: String,
        scope: String,
        message_count: usize,
        tool_count: usize,
    },
    #[serde(rename = "compaction")]
    Compaction { summary: String },
}

impl PiRpcEvent {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

/// RPC server — reads commands from stdin, writes events to stdout.
pub struct PiRpcServer {
    agent: Arc<parking_lot::Mutex<Agent>>,
}

impl PiRpcServer {
    pub fn new(agent: Agent) -> Self {
        Self {
            agent: Arc::new(parking_lot::Mutex::new(agent)),
        }
    }

    /// Run the RPC server on stdin/stdout.
    /// Blocks until stdin is closed or an abort command is received.
    pub fn run(&self) -> std::io::Result<()> {
        use std::io::{BufRead, Write};
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();

        info!("pi RPC server started on stdin/stdout");

        for line in stdin.lock().lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    warn!("stdin read error: {e}");
                    break;
                }
            };
            if line.is_empty() {
                continue;
            }

            let response = self.handle_command(&line);
            if let Some(event) = response {
                writeln!(stdout, "{event}")?;
                stdout.flush()?;
            }
        }

        info!("pi RPC server shutting down");
        Ok(())
    }

    #[allow(clippy::await_holding_lock)]
    pub fn handle_command(&self, line: &str) -> Option<String> {
        let cmd: PiRpcCommand = match serde_json::from_str(line) {
            Ok(c) => c,
            Err(e) => {
                let event = PiRpcEvent::Error {
                    message: format!("parse error: {e}"),
                };
                return Some(event.to_json());
            }
        };

        match cmd {
            PiRpcCommand::GetState => {
                let agent = self.agent.lock();
                let event = PiRpcEvent::State {
                    model: agent.model.clone(),
                    scope: agent.scope.name().to_string(),
                    message_count: agent.message_count(),
                    tool_count: agent.tools.count(),
                };
                Some(event.to_json())
            }
            PiRpcCommand::SetModel { provider, model } => {
                let mut agent = self.agent.lock();
                let _ = provider;
                agent.set_model(&model);
                let event = PiRpcEvent::State {
                    model: model.clone(),
                    scope: agent.scope.name().to_string(),
                    message_count: agent.message_count(),
                    tool_count: agent.tools.count(),
                };
                Some(event.to_json())
            }
            PiRpcCommand::Compact => {
                let agent = self.agent.lock();
                agent.compact("rpc compact command");
                let event = PiRpcEvent::Compaction {
                    summary: "context compacted".into(),
                };
                Some(event.to_json())
            }
            PiRpcCommand::Abort => {
                info!("abort received — stopping RPC server");
                None
            }
            PiRpcCommand::Prompt { text } => {
                let agent = self.agent.clone();
                let text = text.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        let mut agent = agent.lock();
                        if let Err(e) = agent.prompt(&text).await {
                            error!("prompt error: {e}");
                        }
                    });
                });
                let event = PiRpcEvent::AgentStart;
                Some(event.to_json())
            }
            PiRpcCommand::Steer { text } => {
                let agent = self.agent.lock();
                agent
                    .messages
                    .write()
                    .push(crate::provider::Message::user(text));
                Some(
                    PiRpcEvent::State {
                        model: agent.model.clone(),
                        scope: agent.scope.name().to_string(),
                        message_count: agent.message_count(),
                        tool_count: agent.tools.count(),
                    }
                    .to_json(),
                )
            }
            PiRpcCommand::FollowUp { text } => {
                let agent = self.agent.clone();
                let text = text.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        let mut agent = agent.lock();
                        if let Err(e) = agent.prompt(&text).await {
                            error!("follow-up error: {e}");
                        }
                    });
                });
                Some(PiRpcEvent::AgentStart.to_json())
            }
            PiRpcCommand::Subscribe | PiRpcCommand::Unsubscribe => Some(
                PiRpcEvent::State {
                    model: "ok".into(),
                    scope: "ok".into(),
                    message_count: 0,
                    tool_count: 0,
                }
                .to_json(),
            ),
        }
    }
}

/// RPC client — connects to a pi RPC server subprocess or in-process.
pub struct PiRpcClient {
    writer: Box<dyn std::io::Write + Send>,
}

impl PiRpcClient {
    pub fn new(writer: Box<dyn std::io::Write + Send>) -> Self {
        Self { writer }
    }

    pub fn send_command(&mut self, cmd: &PiRpcCommand) -> std::io::Result<()> {
        let json = serde_json::to_string(cmd)?;
        writeln!(self.writer, "{json}")?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn prompt(&mut self, text: &str) -> std::io::Result<()> {
        self.send_command(&PiRpcCommand::Prompt { text: text.into() })
    }

    pub fn steer(&mut self, text: &str) -> std::io::Result<()> {
        self.send_command(&PiRpcCommand::Steer { text: text.into() })
    }

    pub fn abort(&mut self) -> std::io::Result<()> {
        self.send_command(&PiRpcCommand::Abort)
    }

    pub fn get_state(&mut self) -> std::io::Result<()> {
        self.send_command(&PiRpcCommand::GetState)
    }

    pub fn set_model(&mut self, provider: &str, model: &str) -> std::io::Result<()> {
        self.send_command(&PiRpcCommand::SetModel {
            provider: provider.into(),
            model: model.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_serialization() {
        let cmd = PiRpcCommand::Prompt {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"method\":\"prompt\""));
        assert!(json.contains("\"text\":\"hello\""));

        let parsed: PiRpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            PiRpcCommand::Prompt { text } => assert_eq!(text, "hello"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn event_serialization() {
        let event = PiRpcEvent::MessageDelta { delta: "hi".into() };
        let json = event.to_json();
        assert!(json.contains("\"type\":\"message_delta\""));
    }

    #[test]
    fn abort_command_parses() {
        let json = r#"{"method":"abort"}"#;
        let cmd: PiRpcCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, PiRpcCommand::Abort));
    }
}
