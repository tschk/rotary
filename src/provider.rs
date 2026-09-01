//! Provider abstraction with async streaming (pi_agent_rust pattern).
//! Real SSE streaming via reqwest + eventsource-stream.

use crate::agent::ToolCall;
use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
#[cfg(feature = "providers")]
use std::collections::BTreeMap;
use std::sync::Arc;
#[cfg(feature = "providers")]
use tracing::{debug, error};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::System => write!(f, "system"),
            Self::User => write!(f, "user"),
            Self::Assistant => write!(f, "assistant"),
            Self::Tool => write!(f, "tool"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }
    pub fn tool(tool_call_id: &str, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_calls: Vec::new(),
        }
    }

    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls,
        }
    }
}

/// Streaming events from a provider (pi_agent_rust StreamEvent pattern).
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Delta(String),
    ToolCall(ToolCall),
    /// Provider-reported token usage for the completed request.
    Usage(crate::cost::TokenUsage),
    Done,
}

#[cfg(feature = "providers")]
pub type StreamResult =
    Box<dyn futures::Stream<Item = Result<StreamEvent, ProviderError>> + Send + Unpin>;

/// The provider trait — implementations stream completions from LLM backends.
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;

    #[cfg(feature = "providers")]
    async fn stream(
        &self,
        messages: &[Message],
        system: &Option<String>,
        model: &str,
        tools: &[serde_json::Value],
        reasoning_effort: Option<&str>,
    ) -> Result<StreamResult, ProviderError>;

    /// Non-streaming fallback (used when providers feature is off).
    async fn generate(
        &self,
        messages: &[Message],
        system: &Option<String>,
        model: &str,
        tools: &[serde_json::Value],
    ) -> Result<String, ProviderError> {
        #[cfg(feature = "providers")]
        {
            let mut content = String::new();
            let mut stream = self.stream(messages, system, model, tools, None).await?;
            use futures::StreamExt;
            while let Some(event) = stream.next().await {
                if let Ok(StreamEvent::Delta(delta)) = event {
                    content.push_str(&delta);
                }
            }
            return Ok(content);
        }
        #[cfg(not(feature = "providers"))]
        {
            let _ = (messages, system, model, tools);
            return Ok("[providers feature not enabled]".to_string());
        }
    }
}

/// Provider registry (dashmap, grok pattern).
pub struct ProviderRegistry {
    providers: DashMap<String, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: DashMap::new(),
        }
    }

    pub fn register(&self, id: impl Into<String>, provider: Arc<dyn Provider>) {
        self.providers.insert(id.into(), provider);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(id).map(|p| p.clone())
    }

    pub fn count(&self) -> usize {
        self.providers.len()
    }

    pub fn ids(&self) -> Vec<String> {
        self.providers.iter().map(|p| p.key().clone()).collect()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// OpenAI-compatible provider with real SSE streaming.
#[cfg(feature = "providers")]
pub struct OpenAIProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    provider_id: String,
    provider_name: String,
    prompt_cache: crate::prompt_cache::PromptCacheConfig,
}

#[cfg(feature = "providers")]
impl OpenAIProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url("https://api.openai.com/v1", api_key, "openai", "OpenAI")
    }

    pub fn anthropic(api_key: impl Into<String>) -> Self {
        Self::with_base_url(
            "https://api.anthropic.com/v1",
            api_key,
            "anthropic",
            "Anthropic",
        )
    }

    pub fn ollama() -> Self {
        Self::with_base_url("http://localhost:11434/v1", "", "local", "Ollama")
    }

    pub fn with_base_url(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        provider_id: impl Into<String>,
        provider_name: impl Into<String>,
    ) -> Self {
        let provider_id_str = provider_id.into();
        let prompt_cache = if provider_id_str == "anthropic" {
            crate::prompt_cache::PromptCacheConfig::anthropic()
        } else if provider_id_str == "openai" {
            crate::prompt_cache::PromptCacheConfig::openai()
        } else {
            crate::prompt_cache::PromptCacheConfig::disabled()
        };
        Self {
            client: reqwest::Client::builder()
                .pool_idle_timeout(std::time::Duration::from_secs(90))
                .tcp_keepalive(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            base_url: base_url.into(),
            api_key: api_key.into(),
            provider_id: provider_id_str,
            provider_name: provider_name.into(),
            prompt_cache,
        }
    }

    /// Override prompt-cache configuration (Anthropic cache_control markers).
    pub fn with_prompt_cache(mut self, config: crate::prompt_cache::PromptCacheConfig) -> Self {
        self.prompt_cache = config;
        self
    }

    /// Prewarm the connection pool by sending a lightweight HEAD request.
    /// This establishes TCP/TLS connections before the first real request,
    /// reducing first-request latency (codex-rs preconnect pattern).
    pub async fn prewarm(&self) -> Result<(), ProviderError> {
        let url = format!("{}/models", self.base_url);
        let mut req = self.client.head(&url);
        if !self.api_key.is_empty() {
            if self.provider_id == "anthropic" {
                req = req
                    .header("x-api-key", &self.api_key)
                    .header("anthropic-version", "2023-06-01");
            } else {
                req = req.bearer_auth(&self.api_key);
            }
        }
        let _ = req.send().await;
        Ok(())
    }

    /// Create a session-scoped client that preserves connection state across retries.
    pub fn new_session(&self) -> ModelClientSession {
        ModelClientSession {
            connection_reused: false,
        }
    }
}

/// Session-scoped client state — preserves connection and routing info
/// across retries within a single turn (codex-rs ModelClientSession pattern).
#[cfg(feature = "providers")]
pub struct ModelClientSession {
    connection_reused: bool,
}

#[cfg(feature = "providers")]
impl ModelClientSession {
    pub fn was_connection_reused(&self) -> bool {
        self.connection_reused
    }

    pub fn set_connection_reused(&mut self, reused: bool) {
        self.connection_reused = reused;
    }
}

#[cfg(feature = "providers")]
#[async_trait]
impl Provider for OpenAIProvider {
    fn id(&self) -> &str {
        &self.provider_id
    }
    fn name(&self) -> &str {
        &self.provider_name
    }

    async fn stream(
        &self,
        messages: &[Message],
        system: &Option<String>,
        model: &str,
        tools: &[serde_json::Value],
        reasoning_effort: Option<&str>,
    ) -> Result<StreamResult, ProviderError> {
        let body = if self.provider_id == "anthropic" {
            anthropic_request(
                messages,
                system,
                model,
                tools,
                reasoning_effort,
                &self.prompt_cache,
            )
        } else {
            openai_request(
                messages,
                system,
                model,
                tools,
                reasoning_effort,
                self.provider_id == "openai",
            )
        };

        let endpoint = if self.provider_id == "anthropic" {
            "messages"
        } else {
            "chat/completions"
        };
        let mut req = self
            .client
            .post(format!("{}/{}", self.base_url, endpoint))
            .json(&body);

        if !self.api_key.is_empty() {
            if self.provider_id == "anthropic" {
                req = req
                    .header("x-api-key", &self.api_key)
                    .header("anthropic-version", "2023-06-01");
            } else {
                req = req.bearer_auth(&self.api_key);
            }
        }

        let response = req
            .send()
            .await
            .map_err(|e| ProviderError::Http(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            error!("provider error {status}: {text}");
            return Err(ProviderError::Api(format!("{status}: {text}")));
        }

        let byte_stream = response.bytes_stream();
        let sse_stream = eventsource_stream::Eventsource::eventsource(byte_stream);
        let provider_id = self.provider_id.clone();

        use futures::StreamExt;
        let mapped = sse_stream
            .scan(StreamState::default(), move |state, event_result| {
                let result = match event_result {
                    Ok(event) if event.data == "[DONE]" => vec![Ok(StreamEvent::Done)],
                    Ok(event) => match serde_json::from_str::<serde_json::Value>(&event.data) {
                        Ok(json) => parse_sse_events(&json, &provider_id, state),
                        Err(e) => {
                            debug!(
                                "sse parse error: {e} (data: {})",
                                &event.data[..event.data.len().min(200)]
                            );
                            Vec::new()
                        }
                    },
                    Err(e) => vec![Err(ProviderError::Stream(e.to_string()))],
                };
                std::future::ready(Some(result))
            })
            .flat_map(futures::stream::iter);

        Ok(Box::new(Box::pin(mapped)))
    }
}

#[cfg(feature = "providers")]
fn openai_request(
    messages: &[Message],
    system: &Option<String>,
    model: &str,
    tools: &[serde_json::Value],
    reasoning_effort: Option<&str>,
    include_usage: bool,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "stream": true,
        "messages": [],
    });
    if include_usage {
        body["stream_options"] = serde_json::json!({"include_usage": true});
    }

    let msgs = body["messages"]
        .as_array_mut()
        .expect("messages is initialized as an array");
    if let Some(sys) = system {
        msgs.push(serde_json::json!({"role": "system", "content": sys}));
    }
    for m in messages {
        let mut entry = serde_json::json!({"role": m.role, "content": m.content});
        if let Some(tid) = &m.tool_call_id {
            entry["tool_call_id"] = serde_json::json!(tid);
        }
        if !m.tool_calls.is_empty() {
            entry["tool_calls"] = serde_json::Value::Array(
                m.tool_calls
                    .iter()
                    .map(|call| {
                        serde_json::json!({
                            "id": call.id,
                            "type": "function",
                            "function": {"name": call.name, "arguments": call.arguments}
                        })
                    })
                    .collect(),
            );
        }
        msgs.push(entry);
    }

    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(
            tools
                .iter()
                .map(|tool| {
                    if tool.get("type").is_some() {
                        tool.clone()
                    } else {
                        serde_json::json!({"type": "function", "function": tool})
                    }
                })
                .collect(),
        );
    }
    if let Some(effort) = reasoning_effort {
        body["reasoning_effort"] = serde_json::json!(effort);
    }
    body
}

#[cfg(feature = "providers")]
fn anthropic_request(
    messages: &[Message],
    system: &Option<String>,
    model: &str,
    tools: &[serde_json::Value],
    reasoning_effort: Option<&str>,
    prompt_cache: &crate::prompt_cache::PromptCacheConfig,
) -> serde_json::Value {
    let mut converted = Vec::with_capacity(messages.len());
    for message in messages {
        match message.role {
            Role::System => {}
            Role::Tool => converted.push(serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id,
                    "content": message.content
                }]
            })),
            Role::Assistant => {
                let mut content = Vec::new();
                if !message.content.is_empty() {
                    content.push(serde_json::json!({"type": "text", "text": message.content}));
                }
                content.extend(message.tool_calls.iter().map(|call| {
                    let input = serde_json::from_str(&call.arguments)
                        .unwrap_or_else(|_| serde_json::json!({"raw": call.arguments}));
                    serde_json::json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": input
                    })
                }));
                converted.push(serde_json::json!({"role": "assistant", "content": content}));
            }
            Role::User => {
                converted.push(serde_json::json!({"role": "user", "content": message.content}))
            }
        }
    }

    // Apply Anthropic cache_control markers when configured.
    crate::prompt_cache::apply_cache_control(&mut converted, prompt_cache);

    let mut body = serde_json::json!({
        "model": model,
        "stream": true,
        "max_tokens": 8192,
        "messages": converted
    });
    if let Some(system) = system {
        body["system"] = serde_json::json!(system);
    }
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(
            tools
                .iter()
                .map(|tool| {
                    let function = tool.get("function").unwrap_or(tool);
                    serde_json::json!({
                        "name": function["name"],
                        "description": function["description"],
                        "input_schema": function["parameters"]
                    })
                })
                .collect(),
        );
    }
    if let Some(effort) = reasoning_effort {
        let budget = match effort {
            "low" => 1_024,
            "medium" => 4_096,
            "high" => 8_192,
            "xhigh" => 16_384,
            _ => 1_024,
        };
        body["thinking"] = serde_json::json!({"type": "enabled", "budget_tokens": budget});
        body["max_tokens"] = serde_json::json!(budget + 8_192);
    }
    body
}

#[cfg(feature = "providers")]
#[derive(Default)]
struct StreamState {
    tool_calls: BTreeMap<usize, ToolCall>,
    usage: crate::cost::TokenUsage,
}

#[cfg(feature = "providers")]
fn parse_sse_events(
    json: &serde_json::Value,
    provider_id: &str,
    state: &mut StreamState,
) -> Vec<Result<StreamEvent, ProviderError>> {
    if provider_id == "anthropic" {
        return parse_anthropic_event(json, state);
    }

    // OpenAI-compatible providers send usage in a final stream chunk when
    // `stream_options.include_usage` is enabled. That chunk commonly has no
    // choices, so inspect it before looking for a delta.
    if let Some(usage) = json.get("usage").and_then(parse_token_usage) {
        return vec![Ok(StreamEvent::Usage(usage))];
    }

    let delta = &json["choices"][0]["delta"];

    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
        if !content.is_empty() {
            return vec![Ok(StreamEvent::Delta(content.to_string()))];
        }
    }

    if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
        for fragment in tool_calls {
            let index = fragment
                .get("index")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as usize;
            let call = state.tool_calls.entry(index).or_insert_with(|| ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
            if let Some(id) = fragment.get("id").and_then(|value| value.as_str()) {
                call.id.push_str(id);
            }
            if let Some(function) = fragment.get("function") {
                if let Some(name) = function.get("name").and_then(|value| value.as_str()) {
                    call.name.push_str(name);
                }
                if let Some(arguments) = function.get("arguments").and_then(|value| value.as_str())
                {
                    call.arguments.push_str(arguments);
                }
            }
        }
    }

    let finish = json["choices"][0]
        .get("finish_reason")
        .and_then(|f| f.as_str());
    if matches!(finish, Some("stop")) {
        return vec![Ok(StreamEvent::Done)];
    }
    if matches!(finish, Some("tool_calls")) {
        return state
            .tool_calls
            .split_off(&0)
            .into_values()
            .map(|call| Ok(StreamEvent::ToolCall(call)))
            .collect();
    }

    Vec::new()
}

#[cfg(feature = "providers")]
fn parse_anthropic_event(
    json: &serde_json::Value,
    state: &mut StreamState,
) -> Vec<Result<StreamEvent, ProviderError>> {
    match json.get("type").and_then(|value| value.as_str()) {
        Some("content_block_start") if json["content_block"]["type"] == "tool_use" => {
            let index = json["index"].as_u64().unwrap_or(0) as usize;
            state.tool_calls.insert(
                index,
                ToolCall {
                    id: json["content_block"]["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    name: json["content_block"]["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    arguments: String::new(),
                },
            );
            Vec::new()
        }
        Some("content_block_delta") if json["delta"]["type"] == "text_delta" => json["delta"]
            ["text"]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(|text| vec![Ok(StreamEvent::Delta(text.to_string()))])
            .unwrap_or_default(),
        Some("content_block_delta") if json["delta"]["type"] == "input_json_delta" => {
            let index = json["index"].as_u64().unwrap_or(0) as usize;
            if let Some(call) = state.tool_calls.get_mut(&index) {
                call.arguments
                    .push_str(json["delta"]["partial_json"].as_str().unwrap_or_default());
            }
            Vec::new()
        }
        Some("content_block_stop") => {
            let index = json["index"].as_u64().unwrap_or(0) as usize;
            state
                .tool_calls
                .remove(&index)
                .map(|call| vec![Ok(StreamEvent::ToolCall(call))])
                .unwrap_or_default()
        }
        Some("message_start") => {
            if let Some(usage) = json
                .get("message")
                .and_then(|message| message.get("usage"))
                .and_then(parse_token_usage)
            {
                state.usage.input_tokens = usage.input_tokens;
                state.usage.cache_read_tokens = usage.cache_read_tokens;
                state.usage.cache_write_tokens = usage.cache_write_tokens;
            }
            Vec::new()
        }
        Some("message_delta") => {
            if let Some(usage) = json.get("usage").and_then(parse_token_usage) {
                state.usage.output_tokens = usage.output_tokens;
            }
            vec![Ok(StreamEvent::Usage(state.usage))]
        }
        Some("message_stop") => {
            let mut events = Vec::new();
            if state.usage.input_tokens > 0
                || state.usage.output_tokens > 0
                || state.usage.cache_read_tokens > 0
                || state.usage.cache_write_tokens > 0
            {
                events.push(Ok(StreamEvent::Usage(state.usage)));
            }
            events.push(Ok(StreamEvent::Done));
            events
        }
        Some("error") => vec![Err(ProviderError::Api(
            json["error"]["message"]
                .as_str()
                .unwrap_or("Anthropic stream error")
                .to_string(),
        ))],
        _ => Vec::new(),
    }
}

#[cfg(feature = "providers")]
fn parse_token_usage(value: &serde_json::Value) -> Option<crate::cost::TokenUsage> {
    let number = |key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let usage = crate::cost::TokenUsage {
        input_tokens: number("input_tokens").max(number("prompt_tokens")),
        output_tokens: number("output_tokens").max(number("completion_tokens")),
        cache_read_tokens: number("cache_read_input_tokens").max(
            value
                .get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
        ),
        cache_write_tokens: number("cache_creation_input_tokens"),
    };
    (usage.input_tokens > 0
        || usage.output_tokens > 0
        || usage.cache_read_tokens > 0
        || usage.cache_write_tokens > 0)
        .then_some(usage)
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("http error: {0}")]
    Http(String),
    #[error("api error: {0}")]
    Api(String),
    #[error("stream error: {0}")]
    Stream(String),
}

impl ProviderError {
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Http(_) => true,
            Self::Api(message) => matches!(
                message.split_whitespace().next(),
                Some("408" | "409" | "429" | "500" | "502" | "503" | "504")
            ),
            Self::Stream(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderError;
    #[cfg(feature = "providers")]
    use super::*;
    use super::{Message, Role};

    #[test]
    fn transient_errors_are_retryable() {
        assert!(ProviderError::Http("reset".into()).is_transient());
        assert!(ProviderError::Api("429 busy".into()).is_transient());
        assert!(ProviderError::Api("503 unavailable".into()).is_transient());
        assert!(!ProviderError::Api("401 unauthorized".into()).is_transient());
    }

    #[test]
    fn message_factory_methods_test_edge_cases() {
        let user = Message::user("hello");
        assert_eq!(user.role, Role::User);
        assert_eq!(user.content, "hello");
        assert_eq!(user.tool_call_id, None);
        assert!(user.tool_calls.is_empty());

        let system = Message::system("sys_hello");
        assert_eq!(system.role, Role::System);
        assert_eq!(system.content, "sys_hello");
        assert_eq!(system.tool_call_id, None);
        assert!(system.tool_calls.is_empty());

        let tool = Message::tool("call_1", "result");
        assert_eq!(tool.role, Role::Tool);
        assert_eq!(tool.content, "result");
        assert_eq!(tool.tool_call_id, Some("call_1".to_string()));
        assert!(tool.tool_calls.is_empty());

        let assistant = Message::assistant_with_tools(
            "thinking",
            vec![crate::agent::ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: "{}".into(),
            }],
        );
        assert_eq!(assistant.role, Role::Assistant);
        assert_eq!(assistant.content, "thinking");
        assert_eq!(assistant.tool_call_id, None);
        assert_eq!(assistant.tool_calls.len(), 1);
    }

    #[cfg(feature = "providers")]
    #[test]
    fn assembles_fragmented_openai_tool_calls() {
        let mut state = StreamState::default();
        let fragments = [
            serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_","function":{"name":"re","arguments":"{\"pa"}}]}}]}),
            serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"1","function":{"name":"ad","arguments":"th\":\"x\"}"}}]}}]}),
        ];
        for fragment in fragments {
            assert!(parse_sse_events(&fragment, "openai", &mut state).is_empty());
        }
        let events = parse_sse_events(
            &serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            "openai",
            &mut state,
        );
        let StreamEvent::ToolCall(call) =
            events.into_iter().next().expect("tool call").expect("ok")
        else {
            panic!("expected tool call");
        };
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "read");
        assert_eq!(call.arguments, "{\"path\":\"x\"}");
    }

    #[cfg(feature = "providers")]
    #[test]
    fn wraps_registry_tools_for_openai_compatible_providers() {
        let tools = vec![serde_json::json!({
            "name":"read","description":"Read","parameters":{"type":"object"}
        })];
        let body = openai_request(&[], &None, "grok-4.5", &tools, Some("high"), false);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"], tools[0]);
        assert!(body.get("stream_options").is_none());
    }

    #[cfg(feature = "providers")]
    #[test]
    fn parses_openai_usage_chunk() {
        let mut state = StreamState::default();
        let events = parse_sse_events(
            &serde_json::json!({
                "choices": [],
                "usage": {
                    "prompt_tokens": 1000,
                    "completion_tokens": 200,
                    "prompt_tokens_details": {"cached_tokens": 800}
                }
            }),
            "openai",
            &mut state,
        );
        assert!(matches!(
            events.into_iter().next().unwrap().unwrap(),
            StreamEvent::Usage(crate::cost::TokenUsage {
                input_tokens: 1000,
                output_tokens: 200,
                cache_read_tokens: 800,
                ..
            })
        ));
    }

    #[cfg(feature = "providers")]
    #[test]
    fn parses_anthropic_usage_events() {
        let mut state = StreamState::default();
        assert!(parse_sse_events(
            &serde_json::json!({
                "type": "message_start",
                "message": {"usage": {"input_tokens": 100, "cache_read_input_tokens": 60}}
            }),
            "anthropic",
            &mut state,
        )
        .is_empty());
        let events = parse_sse_events(
            &serde_json::json!({"type":"message_delta","usage":{"output_tokens":25}}),
            "anthropic",
            &mut state,
        );
        assert!(matches!(
            events.into_iter().next().unwrap().unwrap(),
            StreamEvent::Usage(crate::cost::TokenUsage {
                input_tokens: 100,
                output_tokens: 25,
                cache_read_tokens: 60,
                ..
            })
        ));
    }

    #[cfg(feature = "providers")]
    #[test]
    fn builds_native_anthropic_request_and_stream() {
        let messages = vec![
            Message::user("inspect"),
            Message::assistant_with_tools(
                "",
                vec![ToolCall {
                    id: "tool_1".into(),
                    name: "read".into(),
                    arguments: "{\"path\":\"x\"}".into(),
                }],
            ),
            Message::tool("tool_1", "contents"),
        ];
        let tools = vec![serde_json::json!({
            "name":"read","description":"Read","parameters":{"type":"object"}
        })];
        let body = anthropic_request(
            &messages,
            &Some("system".into()),
            "claude-sonnet-4",
            &tools,
            Some("high"),
            &crate::prompt_cache::PromptCacheConfig::disabled(),
        );
        assert_eq!(body["system"], "system");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(body["thinking"]["budget_tokens"], 8192);

        let mut state = StreamState::default();
        parse_sse_events(
            &serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool_1","name":"read","input":{}}}),
            "anthropic",
            &mut state,
        );
        parse_sse_events(
            &serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"x\"}"}}),
            "anthropic",
            &mut state,
        );
        let events = parse_sse_events(
            &serde_json::json!({"type":"content_block_stop","index":0}),
            "anthropic",
            &mut state,
        );
        let StreamEvent::ToolCall(call) =
            events.into_iter().next().expect("tool call").expect("ok")
        else {
            panic!("expected tool call");
        };
        assert_eq!(call.arguments, "{\"path\":\"x\"}");
    }

    #[cfg(feature = "providers")]
    #[test]
    fn propagates_openai_reasoning_effort() {
        let body = openai_request(
            &[Message::user("solve")],
            &None,
            "gpt-5.6-sol",
            &[],
            Some("xhigh"),
            true,
        );
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["reasoning_effort"], "xhigh");
    }

    #[cfg(feature = "providers")]
    #[test]
    fn omits_openai_reasoning_effort_when_host_does_not_supply_one() {
        let body = openai_request(
            &[Message::user("solve")],
            &None,
            "grok-4.20-0309-reasoning",
            &[],
            None,
            false,
        );
        assert!(body.get("reasoning_effort").is_none());
    }
}
