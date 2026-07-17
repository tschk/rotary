//! Provider abstraction with async streaming (pi_agent_rust pattern).
//! Real SSE streaming via reqwest + eventsource-stream.

use crate::agent::ToolCall;
use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_call_id: None,
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
        }
    }
}

/// Streaming events from a provider (pi_agent_rust StreamEvent pattern).
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Delta(String),
    ToolCall(ToolCall),
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
            let mut stream = self.stream(messages, system, model, tools).await?;
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
            provider_id: self.provider_id.clone(),
            connection_reused: false,
        }
    }
}

/// Session-scoped client state — preserves connection and routing info
/// across retries within a single turn (codex-rs ModelClientSession pattern).
#[cfg(feature = "providers")]
pub struct ModelClientSession {
    #[allow(dead_code)]
    provider_id: String,
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
    ) -> Result<StreamResult, ProviderError> {
        let mut body = serde_json::json!({
            "model": model,
            "stream": true,
            "messages": [],
        });

        let msgs = body["messages"].as_array_mut().unwrap();
        if let Some(sys) = system {
            msgs.push(serde_json::json!({"role": "system", "content": sys}));
        }
        for m in messages {
            let mut entry = serde_json::json!({"role": m.role, "content": m.content});
            if let Some(tid) = &m.tool_call_id {
                entry["tool_call_id"] = serde_json::json!(tid);
            }
            msgs.push(entry);
        }

        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools);
        }

        // Apply Anthropic cache_control markers when configured.
        if let Some(arr) = body["messages"].as_array_mut() {
            crate::prompt_cache::apply_cache_control(arr, &self.prompt_cache);
        }

        let mut req = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
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
        let mapped = sse_stream.filter_map(move |event_result| {
            let pid = provider_id.clone();
            async move {
                match event_result {
                    Ok(event) => {
                        if event.data == "[DONE]" {
                            return Some(Ok(StreamEvent::Done));
                        }
                        match serde_json::from_str::<serde_json::Value>(&event.data) {
                            Ok(json) => parse_sse_event(&json, &pid),
                            Err(e) => {
                                debug!(
                                    "sse parse error: {e} (data: {})",
                                    &event.data[..event.data.len().min(200)]
                                );
                                None
                            }
                        }
                    }
                    Err(e) => Some(Err(ProviderError::Stream(e.to_string()))),
                }
            }
        });

        Ok(Box::new(Box::pin(mapped)))
    }
}

#[cfg(feature = "providers")]
fn parse_sse_event(
    json: &serde_json::Value,
    provider_id: &str,
) -> Option<Result<StreamEvent, ProviderError>> {
    let delta = &json["choices"][0]["delta"];

    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
        if !content.is_empty() {
            return Some(Ok(StreamEvent::Delta(content.to_string())));
        }
    }

    if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
        if let Some(first) = tool_calls.first() {
            let id = first
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("unknown")
                .to_string();
            let function = first.get("function").unwrap_or(&serde_json::Value::Null);
            let name = function
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = function
                .get("arguments")
                .and_then(|a| a.as_str())
                .unwrap_or("{}")
                .to_string();
            if !name.is_empty() {
                return Some(Ok(StreamEvent::ToolCall(ToolCall {
                    id,
                    name,
                    arguments,
                })));
            }
        }
    }

    let finish = json["choices"][0]
        .get("finish_reason")
        .and_then(|f| f.as_str());
    if let Some("stop") = finish {
        return Some(Ok(StreamEvent::Done));
    }

    let _ = provider_id;
    None
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

    #[test]
    fn transient_errors_are_retryable() {
        assert!(ProviderError::Http("reset".into()).is_transient());
        assert!(ProviderError::Api("429 busy".into()).is_transient());
        assert!(ProviderError::Api("503 unavailable".into()).is_transient());
        assert!(!ProviderError::Api("401 unauthorized".into()).is_transient());
    }
}
