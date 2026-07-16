//! Vector embeddings for semantic skill matching.
//!
//! Inspired by Unthinkclaw's embeddings module. Supports Gemini
//! `text-embedding-004` (free tier) and local Ollama `nomic-embed-text`.
//! The HTTP client is gated behind the `providers` feature; struct
//! definitions and `cosine_similarity` are always available so the module
//! compiles without any optional dependencies.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::skill_engine::Skill;

/// Embedding provider selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmbeddingProvider {
    /// Google Gemini `text-embedding-004` (free tier).
    Gemini,
    /// Local Ollama server (`nomic-embed-text` by default).
    Ollama,
}

impl EmbeddingProvider {
    /// Default model name for the provider.
    pub fn default_model(self) -> &'static str {
        match self {
            EmbeddingProvider::Gemini => "text-embedding-004",
            EmbeddingProvider::Ollama => "nomic-embed-text",
        }
    }

    /// Default embedding dimension for the provider.
    pub fn default_dimension(self) -> usize {
        match self {
            EmbeddingProvider::Gemini => 768,
            EmbeddingProvider::Ollama => 768,
        }
    }
}

/// Configuration for an [`EmbeddingClient`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Which embedding backend to use.
    pub provider: EmbeddingProvider,
    /// API key for hosted providers (Gemini). `None` for local Ollama.
    pub api_key: Option<String>,
    /// Model identifier passed to the provider.
    pub model: String,
    /// Expected vector dimensionality.
    pub dimension: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self::gemini()
    }
}

impl EmbeddingConfig {
    /// Gemini defaults: `text-embedding-004`, 768 dims, no key configured.
    pub fn gemini() -> Self {
        Self {
            provider: EmbeddingProvider::Gemini,
            api_key: None,
            model: EmbeddingProvider::Gemini.default_model().to_string(),
            dimension: EmbeddingProvider::Gemini.default_dimension(),
        }
    }

    /// Ollama defaults: `nomic-embed-text`, 768 dims, no key required.
    pub fn ollama() -> Self {
        Self {
            provider: EmbeddingProvider::Ollama,
            api_key: None,
            model: EmbeddingProvider::Ollama.default_model().to_string(),
            dimension: EmbeddingProvider::Ollama.default_dimension(),
        }
    }

    /// Attach an API key (used by hosted providers like Gemini).
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }
}

/// Errors produced by the embeddings module.
#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("io error: {0}")]
    Io(String),
    #[error("api error: {0}")]
    Api(String),
    #[error("parse error: {0}")]
    Parse(String),
}

#[cfg(feature = "providers")]
impl From<reqwest::Error> for EmbedError {
    fn from(err: reqwest::Error) -> Self {
        EmbedError::Api(err.to_string())
    }
}

/// Client for generating text embeddings via a remote or local provider.
///
/// The inner HTTP client only exists when the `providers` feature is
/// enabled. Without it, [`EmbeddingClient::embed`] returns a descriptive
/// error but the type itself remains constructible.
#[derive(Debug, Clone)]
pub struct EmbeddingClient {
    /// Provider + model configuration.
    pub config: EmbeddingConfig,
    #[cfg(feature = "providers")]
    client: reqwest::Client,
}

impl EmbeddingClient {
    /// Create a new client from a config.
    pub fn new(config: EmbeddingConfig) -> Self {
        Self {
            #[cfg(feature = "providers")]
            client: reqwest::Client::new(),
            config,
        }
    }

    /// Convenience constructor for a Gemini client with an API key.
    pub fn gemini(api_key: impl Into<String>) -> Self {
        Self::new(EmbeddingConfig::gemini().with_api_key(api_key))
    }

    /// Convenience constructor for a local Ollama client.
    pub fn ollama() -> Self {
        Self::new(EmbeddingConfig::ollama())
    }

    /// Embed a single text, returning its vector.
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        #[cfg(feature = "providers")]
        {
            match self.config.provider {
                EmbeddingProvider::Gemini => self.embed_gemini(text).await,
                EmbeddingProvider::Ollama => self.embed_ollama(text).await,
            }
        }
        #[cfg(not(feature = "providers"))]
        {
            let _ = text;
            Err(EmbedError::Api("providers feature not enabled".into()))
        }
    }

    /// Embed multiple texts, returning one vector per input.
    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    #[cfg(feature = "providers")]
    async fn embed_gemini(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let api_key = self
            .config
            .api_key
            .as_deref()
            .ok_or_else(|| EmbedError::Api("gemini provider requires an api key".into()))?;

        let url = "https://generativelanguage.googleapis.com/v1beta/models/text-embedding-004:embedContent";

        let request = serde_json::json!({
            "model": format!("models/{}", self.config.model),
            "content": {
                "parts": [{ "text": text }]
            }
        });

        let resp = self
            .client
            .post(url)
            .header("x-goog-api-key", api_key)
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let result: GeminiResponse = resp.json().await?;
        Ok(result.embedding.values)
    }

    #[cfg(feature = "providers")]
    async fn embed_ollama(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let url = "http://localhost:11434/api/embeddings";

        let request = serde_json::json!({
            "model": self.config.model,
            "prompt": text,
        });

        let resp = self
            .client
            .post(url)
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let result: OllamaResponse = resp.json().await?;
        Ok(result.embedding)
    }
}

/// Cosine similarity between two vectors.
///
/// Returns `0.0` for empty vectors, mismatched lengths, or zero vectors
/// (which would otherwise divide by zero).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot_product: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let magnitude_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let magnitude_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if magnitude_a == 0.0 || magnitude_b == 0.0 {
        return 0.0;
    }

    dot_product / (magnitude_a * magnitude_b)
}

/// Semantic search over skills using cached embeddings.
///
/// Holds an [`EmbeddingClient`] plus a cache keyed by a hash of the input
/// text, so repeated queries or skill descriptions are not re-embedded.
#[derive(Debug, Clone)]
pub struct SemanticSearch {
    client: EmbeddingClient,
    cache: HashMap<u64, Vec<f32>>,
}

impl SemanticSearch {
    /// Create a new semantic search index backed by `client`.
    pub fn new(client: EmbeddingClient) -> Self {
        Self {
            client,
            cache: HashMap::new(),
        }
    }

    /// Embed `text`, using the cache when available.
    pub async fn embed_cached(&mut self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let key = hash_text(text);
        if let Some(vec) = self.cache.get(&key) {
            return Ok(vec.clone());
        }
        let vec = self.client.embed(text).await?;
        self.cache.insert(key, vec.clone());
        Ok(vec)
    }

    /// Search `skills` for the ones most semantically similar to `query`.
    ///
    /// Returns up to `top_k` `(skill_id, similarity)` pairs sorted by
    /// similarity descending.
    pub async fn search_skills(
        &mut self,
        query: &str,
        skills: &[&Skill],
        top_k: usize,
    ) -> Result<Vec<(String, f32)>, EmbedError> {
        if skills.is_empty() || top_k == 0 {
            return Ok(Vec::new());
        }

        let query_vec = self.embed_cached(query).await?;

        let mut scored: Vec<(String, f32)> = Vec::with_capacity(skills.len());
        for skill in skills {
            let vec = self.embed_cached(&skill.description).await?;
            let sim = cosine_similarity(&query_vec, &vec);
            scored.push((skill.id.clone(), sim));
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        Ok(scored)
    }

    /// Number of cached embeddings.
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Clear the embedding cache.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }
}

/// Stable, allocation-free string hash for cache keys (FNV-1a 64-bit).
fn hash_text(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(feature = "providers")]
#[derive(Deserialize)]
struct GeminiResponse {
    embedding: GeminiEmbedding,
}

#[cfg(feature = "providers")]
#[derive(Deserialize)]
struct GeminiEmbedding {
    values: Vec<f32>,
}

#[cfg(feature = "providers")]
#[derive(Deserialize)]
struct OllamaResponse {
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_basic() {
        // Identical vectors -> 1.0
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);

        // Opposite vectors -> -1.0
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - (-1.0)).abs() < 1e-6);

        // 45-degree angle -> ~0.7071
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
    }

    #[test]
    fn test_cosine_similarity_edge_cases() {
        // Empty vectors
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 1e-6);

        // Mismatched lengths
        let a = vec![1.0];
        let b = vec![1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 1e-6);

        // Zero vectors
        let a = vec![0.0, 0.0];
        let b = vec![0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 1e-6);

        // One zero vector, one non-zero
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 1.0];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        // Orthogonal vectors -> 0.0
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 1e-6);

        // Orthogonal in higher dimensions
        let a = vec![1.0, 0.0, 0.0, 0.0];
        let b = vec![0.0, 0.0, 0.0, 1.0];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_config_defaults() {
        let g = EmbeddingConfig::gemini();
        assert_eq!(g.provider, EmbeddingProvider::Gemini);
        assert_eq!(g.model, "text-embedding-004");
        assert_eq!(g.dimension, 768);
        assert!(g.api_key.is_none());

        let o = EmbeddingConfig::ollama();
        assert_eq!(o.provider, EmbeddingProvider::Ollama);
        assert_eq!(o.model, "nomic-embed-text");
        assert_eq!(o.dimension, 768);

        let with_key = EmbeddingConfig::gemini().with_api_key("secret");
        assert_eq!(with_key.api_key.as_deref(), Some("secret"));
    }

    #[test]
    fn test_hash_text_stable() {
        assert_eq!(hash_text("hello"), hash_text("hello"));
        assert_ne!(hash_text("hello"), hash_text("world"));
    }

    #[cfg(not(feature = "providers"))]
    #[tokio::test]
    async fn test_embed_without_providers_errors() {
        let client = EmbeddingClient::new(EmbeddingConfig::gemini());
        let result = client.embed("test").await;
        assert!(matches!(result, Err(EmbedError::Api(_))));
    }
}
