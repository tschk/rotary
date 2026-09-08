use crate::agent::{ToolCall, ToolResult};
use crate::provider::{Message, Provider, ProviderError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CassetteTurn {
    pub messages: Vec<Message>,
    pub response: String,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Divergence {
    pub index: usize,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Default)]
pub struct ReplayProvider {
    pub id: String,
    pub turns: Vec<CassetteTurn>,
    cursor: std::sync::atomic::AtomicUsize,
}

impl ReplayProvider {
    pub fn new(turns: Vec<CassetteTurn>) -> Self {
        Self {
            id: "replay".into(),
            turns,
            cursor: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn next_turn(&self, messages: &[Message]) -> Result<&CassetteTurn, ProviderError> {
        let idx = self
            .cursor
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let turn = self
            .turns
            .get(idx)
            .ok_or_else(|| ProviderError::Api("cassette exhausted".into()))?;
        if let Some(div) = detect_divergence(&turn.messages, messages) {
            return Err(ProviderError::Api(format!(
                "cassette divergence at {}: expected {}, actual {}",
                div.index, div.expected, div.actual
            )));
        }
        Ok(turn)
    }
}

#[async_trait]
impl Provider for ReplayProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "ReplayProvider"
    }

    #[cfg(feature = "providers")]
    async fn stream(
        &self,
        messages: &[Message],
        _system: &Option<String>,
        _model: &str,
        _tools: &[serde_json::Value],
        _reasoning_effort: Option<&str>,
    ) -> Result<crate::provider::StreamResult, ProviderError> {
        let turn = self.next_turn(messages)?;
        let mut events = Vec::new();
        if !turn.response.is_empty() {
            events.push(Ok(crate::provider::StreamEvent::Delta(
                turn.response.clone(),
            )));
        }
        for call in &turn.tool_calls {
            events.push(Ok(crate::provider::StreamEvent::ToolCall(call.clone())));
        }
        events.push(Ok(crate::provider::StreamEvent::Done));
        Ok(Box::new(futures::stream::iter(events)))
    }

    async fn generate(
        &self,
        messages: &[Message],
        _system: &Option<String>,
        _model: &str,
        _tools: &[serde_json::Value],
    ) -> Result<String, ProviderError> {
        Ok(self.next_turn(messages)?.response.clone())
    }
}

pub fn detect_divergence(expected: &[Message], actual: &[Message]) -> Option<Divergence> {
    let n = expected.len().max(actual.len());
    for i in 0..n {
        match (expected.get(i), actual.get(i)) {
            (Some(e), Some(a)) if e != a => {
                return Some(Divergence {
                    index: i,
                    expected: format!("{}:{}", e.role, e.content),
                    actual: format!("{}:{}", a.role, a.content),
                });
            }
            (Some(e), None) => {
                return Some(Divergence {
                    index: i,
                    expected: format!("{}:{}", e.role, e.content),
                    actual: String::new(),
                });
            }
            (None, Some(a)) => {
                return Some(Divergence {
                    index: i,
                    expected: String::new(),
                    actual: format!("{}:{}", a.role, a.content),
                });
            }
            _ => {}
        }
    }
    None
}

pub fn detect_tool_divergence(expected: &[ToolCall], actual: &[ToolCall]) -> Option<Divergence> {
    let n = expected.len().max(actual.len());
    for i in 0..n {
        match (expected.get(i), actual.get(i)) {
            (Some(e), Some(a)) if e != a => {
                return Some(Divergence {
                    index: i,
                    expected: format!("{}:{}:{}", e.id, e.name, e.arguments),
                    actual: format!("{}:{}:{}", a.id, a.name, a.arguments),
                });
            }
            (Some(e), None) => {
                return Some(Divergence {
                    index: i,
                    expected: format!("{}:{}:{}", e.id, e.name, e.arguments),
                    actual: String::new(),
                });
            }
            (None, Some(a)) => {
                return Some(Divergence {
                    index: i,
                    expected: String::new(),
                    actual: format!("{}:{}:{}", a.id, a.name, a.arguments),
                });
            }
            _ => {}
        }
    }
    None
}

pub fn simulate_tool(call: &ToolCall) -> ToolResult {
    ToolResult::ok(&call.id, format!("[cassette] {}", call.name))
}

pub fn replay_without_tools(turns: &[CassetteTurn]) -> bool {
    turns.iter().all(|t| t.tool_calls.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divergence_helper_reports_index() {
        let expected = vec![Message::user("a")];
        let actual = vec![Message::user("b")];
        let div = detect_divergence(&expected, &actual).unwrap();
        assert_eq!(div.index, 0);
        assert!(div.expected.contains('a'));
        assert!(div.actual.contains('b'));
        assert!(detect_divergence(&expected, &expected).is_none());
    }

    #[test]
    fn tool_divergence_helper_reports_index() {
        let expected = vec![ToolCall {
            id: "1".into(),
            name: "read".into(),
            arguments: "{}".into(),
        }];
        let actual = vec![ToolCall {
            id: "1".into(),
            name: "write".into(),
            arguments: "{}".into(),
        }];
        let div = detect_tool_divergence(&expected, &actual).unwrap();
        assert_eq!(div.index, 0);
        assert!(div.expected.contains("read"));
        assert!(div.actual.contains("write"));
        assert!(detect_tool_divergence(&expected, &expected).is_none());
    }

    #[tokio::test]
    async fn replay_provider_replays_and_rejects_divergence() {
        let cassette = vec![CassetteTurn {
            messages: vec![Message::user("hello")],
            response: "world".into(),
            tool_calls: vec![],
        }];
        assert!(replay_without_tools(&cassette));
        let provider = ReplayProvider::new(cassette);
        let out = provider
            .generate(&[Message::user("hello")], &None, "replay", &[])
            .await
            .unwrap();
        assert_eq!(out, "world");
        let provider = ReplayProvider::new(vec![CassetteTurn {
            messages: vec![Message::user("hello")],
            response: "world".into(),
            tool_calls: vec![],
        }]);
        let err = provider
            .generate(&[Message::user("nope")], &None, "replay", &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("divergence"));
    }

    #[cfg(feature = "providers")]
    #[tokio::test]
    async fn replay_stream_emits_recorded_tool_calls() {
        use futures::StreamExt;
        let call = ToolCall {
            id: "c1".into(),
            name: "boom".into(),
            arguments: "{}".into(),
        };
        let provider = ReplayProvider::new(vec![CassetteTurn {
            messages: vec![Message::user("hello")],
            response: "calling".into(),
            tool_calls: vec![call.clone()],
        }]);
        let mut stream = provider
            .stream(&[Message::user("hello")], &None, "replay", &[], None)
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }
        assert!(matches!(
            events.first(),
            Some(crate::provider::StreamEvent::Delta(text)) if text == "calling"
        ));
        assert!(matches!(
            events.get(1),
            Some(crate::provider::StreamEvent::ToolCall(tc)) if tc == &call
        ));
        assert!(matches!(
            events.last(),
            Some(crate::provider::StreamEvent::Done)
        ));
    }
}
