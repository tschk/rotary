//! HTTP client with TLS caching and provider-aware timeouts (pi_agent_rust pattern).
//!
//! Wraps [`reqwest::Client`] with a process-global cached client and per-provider
//! timeout configuration. Cloud providers receive short request timeouts; local
//! providers (Ollama, LM Studio, localhost) receive long timeouts to accommodate
//! slow local generation. A global override is honored via the
//! `PI_HTTP_REQUEST_TIMEOUT_SECS` (or `RX4_HTTP_TIMEOUT_SECS`) environment
//! variable.
//!
//! Clones of [`reqwest::Client`] are cheap — the handle is internally `Arc`'d —
//! so callers may freely share the inner client across tasks without rebuilding
//! the connection pool or TLS state.

use std::sync::OnceLock;
use std::time::Duration;

use thiserror::Error;

/// Maximum total size of accumulated response headers before a request is
/// rejected (64 KiB).
pub const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Chunk size used when reading streaming response bodies (16 KiB).
pub const READ_CHUNK_BYTES: usize = 16 * 1024;

/// Default request timeout for cloud providers, in seconds.
const CLOUD_TIMEOUT_SECS: u64 = 60;

/// Default request timeout for local providers, in seconds.
const LOCAL_TIMEOUT_SECS: u64 = 600;

/// Default connect timeout, in seconds.
const CONNECT_TIMEOUT_SECS: u64 = 10;

/// Default idle connection pool timeout, in seconds.
const POOL_IDLE_TIMEOUT_SECS: u64 = 90;

/// Default TCP keepalive interval, in seconds.
const TCP_KEEPALIVE_SECS: u64 = 60;

/// Cloud providers that receive the short default request timeout.
const CLOUD_PROVIDERS: &[&str] = &["openai", "anthropic", "google", "xai"];

/// Local providers that receive the long default request timeout.
const LOCAL_PROVIDERS: &[&str] = &["ollama", "lmstudio", "localhost"];

/// Errors produced by the HTTP client.
#[derive(Debug, Error)]
pub enum HttpError {
    /// The request to `provider` exceeded its time budget.
    #[error("http request to {provider} timed out after {timeout_secs}s")]
    Timeout {
        /// Provider name the request was addressed to.
        provider: String,
        /// Configured request timeout, in seconds.
        timeout_secs: u64,
    },
    /// The TCP/TLS connection to `provider` could not be established.
    #[error("http connection to {provider} failed: {detail}")]
    Connect {
        /// Provider name the request was addressed to.
        provider: String,
        /// Underlying connection failure detail.
        detail: String,
    },
    /// The request to `provider` failed after connecting.
    #[error("http request to {provider} failed: {detail}")]
    Request {
        /// Provider name the request was addressed to.
        provider: String,
        /// Underlying request failure detail.
        detail: String,
    },
    /// The `reqwest::Client` could not be constructed.
    #[error("failed to build http client: {0}")]
    Builder(String),
}

/// Per-client timeout configuration (codex-rs `TimeoutConfig` pattern).
#[derive(Debug, Clone, Copy)]
pub struct TimeoutConfig {
    /// Total time a single request may take before being aborted.
    pub request_timeout: Duration,
    /// Time allowed to establish the TCP connection.
    pub connect_timeout: Duration,
    /// How long idle connections are retained in the pool before closing.
    pub pool_idle_timeout: Duration,
    /// TCP keepalive interval for pooled connections.
    pub tcp_keepalive: Duration,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(CLOUD_TIMEOUT_SECS),
            connect_timeout: Duration::from_secs(CONNECT_TIMEOUT_SECS),
            pool_idle_timeout: Duration::from_secs(POOL_IDLE_TIMEOUT_SECS),
            tcp_keepalive: Duration::from_secs(TCP_KEEPALIVE_SECS),
        }
    }
}

/// Returns `true` when `base_url` points at a local LLM backend.
///
/// Detects `localhost`, `127.0.0.1`, `0.0.0.0`, `::1` (bracketed or bare), and
/// the `ollama` / `lmstudio` host names commonly used by local model servers.
pub fn is_local_provider(base_url: &str) -> bool {
    let lower = base_url.to_ascii_lowercase();
    let authority = lower.split("://").nth(1).unwrap_or(&lower);
    let host = authority.split('/').next().unwrap_or(authority);
    host.starts_with("localhost")
        || host.starts_with("127.0.0.1")
        || host.starts_with("0.0.0.0")
        || host.starts_with("[::1]")
        || host.starts_with("::1")
        || host.starts_with("ollama")
        || host.starts_with("lmstudio")
}

/// Returns the request timeout override from the environment, if set.
///
/// Honors `PI_HTTP_REQUEST_TIMEOUT_SECS` (pi_agent_rust) first, then the
/// `RX4_HTTP_TIMEOUT_SECS` alias.
fn env_timeout_override() -> Option<Duration> {
    std::env::var("PI_HTTP_REQUEST_TIMEOUT_SECS")
        .or_else(|_| std::env::var("RX4_HTTP_TIMEOUT_SECS"))
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Returns the default request timeout for `provider`.
///
/// Cloud providers ([`CLOUD_PROVIDERS`]) use [`CLOUD_TIMEOUT_SECS`]; local
/// providers ([`LOCAL_PROVIDERS`]) and any URL that [`is_local_provider`]
/// recognizes use [`LOCAL_TIMEOUT_SECS`]. The
/// `PI_HTTP_REQUEST_TIMEOUT_SECS` / `RX4_HTTP_TIMEOUT_SECS` environment
/// variable overrides both.
pub fn timeout_for_provider(provider: &str) -> Duration {
    if let Some(override_) = env_timeout_override() {
        return override_;
    }
    let lower = provider.to_ascii_lowercase();
    if CLOUD_PROVIDERS.contains(&lower.as_str()) {
        Duration::from_secs(CLOUD_TIMEOUT_SECS)
    } else if LOCAL_PROVIDERS.contains(&lower.as_str()) || is_local_provider(provider) {
        Duration::from_secs(LOCAL_TIMEOUT_SECS)
    } else {
        Duration::from_secs(CLOUD_TIMEOUT_SECS)
    }
}

/// Builds a [`reqwest::Client`] from `config`, preferring HTTP/1.1 to avoid
/// HTTP/2 framing complexity for SSE streams.
fn build_client(config: &TimeoutConfig) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(config.request_timeout)
        .connect_timeout(config.connect_timeout)
        .pool_idle_timeout(config.pool_idle_timeout)
        .tcp_keepalive(config.tcp_keepalive)
        .http1_only()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// HTTP client wrapping [`reqwest::Client`] with provider-aware timeouts.
pub struct HttpClient {
    client: reqwest::Client,
    timeout_config: TimeoutConfig,
    provider: String,
}

impl HttpClient {
    /// Creates a new client with default (cloud) settings.
    pub fn new() -> Self {
        Self::with_provider("openai")
    }

    /// Creates a new client configured for `provider`.
    ///
    /// Cloud providers (`openai`, `anthropic`, `google`, `xai`) receive a 60s
    /// request timeout; local providers (`ollama`, `lmstudio`, `localhost`)
    /// receive a 600s timeout to accommodate slow local generation.
    pub fn with_provider(provider: &str) -> Self {
        let request_timeout = timeout_for_provider(provider);
        let timeout_config = TimeoutConfig {
            request_timeout,
            ..TimeoutConfig::default()
        };
        let client = build_client(&timeout_config);
        Self {
            client,
            timeout_config,
            provider: provider.to_string(),
        }
    }

    /// Creates a new client from an explicit timeout configuration.
    pub fn with_timeout_config(provider: &str, timeout_config: TimeoutConfig) -> Self {
        let client = build_client(&timeout_config);
        Self {
            client,
            timeout_config,
            provider: provider.to_string(),
        }
    }

    /// Returns the underlying [`reqwest::Client`].
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Returns the timeout configuration in use.
    pub fn timeout_config(&self) -> TimeoutConfig {
        self.timeout_config
    }

    /// Returns the provider name this client was configured for.
    pub fn provider(&self) -> &str {
        &self.provider
    }
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Returns a process-global cached [`reqwest::Client`].
///
/// The client is built once with default (cloud) timeouts and reused for the
/// lifetime of the process. Clones are cheap because [`reqwest::Client`] is
/// internally `Arc`'d, so the same handle may be shared across tasks without
/// rebuilding the connection pool or TLS session cache.
pub fn global_client() -> &'static reqwest::Client {
    GLOBAL_CLIENT.get_or_init(|| build_client(&TimeoutConfig::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes tests that mutate process-wide environment variables.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn detects_local_providers() {
        assert!(is_local_provider("http://localhost:11434/v1"));
        assert!(is_local_provider("http://127.0.0.1:11434/v1"));
        assert!(is_local_provider("http://0.0.0.0:8080"));
        assert!(is_local_provider("http://[::1]:11434/v1"));
        assert!(is_local_provider("http://ollama:11434/v1"));
        assert!(is_local_provider("http://lmstudio:1234/v1"));
        assert!(is_local_provider("localhost:11434"));
        assert!(is_local_provider("OLLAMA"));
    }

    #[test]
    fn detects_remote_providers() {
        assert!(!is_local_provider("https://api.openai.com/v1"));
        assert!(!is_local_provider("https://api.anthropic.com/v1"));
        assert!(!is_local_provider(
            "https://generativelanguage.googleapis.com"
        ));
        assert!(!is_local_provider("https://api.x.ai/v1"));
    }

    #[test]
    fn timeout_config_defaults() {
        let config = TimeoutConfig::default();
        assert_eq!(
            config.request_timeout,
            Duration::from_secs(CLOUD_TIMEOUT_SECS)
        );
        assert_eq!(
            config.connect_timeout,
            Duration::from_secs(CONNECT_TIMEOUT_SECS)
        );
        assert_eq!(
            config.pool_idle_timeout,
            Duration::from_secs(POOL_IDLE_TIMEOUT_SECS)
        );
        assert_eq!(
            config.tcp_keepalive,
            Duration::from_secs(TCP_KEEPALIVE_SECS)
        );
    }

    #[test]
    fn cloud_provider_timeout() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved_pi = std::env::var("PI_HTTP_REQUEST_TIMEOUT_SECS").ok();
        let saved_rx4 = std::env::var("RX4_HTTP_TIMEOUT_SECS").ok();
        std::env::remove_var("PI_HTTP_REQUEST_TIMEOUT_SECS");
        std::env::remove_var("RX4_HTTP_TIMEOUT_SECS");

        assert_eq!(
            timeout_for_provider("openai"),
            Duration::from_secs(CLOUD_TIMEOUT_SECS)
        );
        assert_eq!(
            timeout_for_provider("anthropic"),
            Duration::from_secs(CLOUD_TIMEOUT_SECS)
        );
        assert_eq!(
            timeout_for_provider("google"),
            Duration::from_secs(CLOUD_TIMEOUT_SECS)
        );
        assert_eq!(
            timeout_for_provider("xai"),
            Duration::from_secs(CLOUD_TIMEOUT_SECS)
        );

        restore_env("PI_HTTP_REQUEST_TIMEOUT_SECS", saved_pi);
        restore_env("RX4_HTTP_TIMEOUT_SECS", saved_rx4);
    }

    #[test]
    fn local_provider_timeout() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved_pi = std::env::var("PI_HTTP_REQUEST_TIMEOUT_SECS").ok();
        let saved_rx4 = std::env::var("RX4_HTTP_TIMEOUT_SECS").ok();
        std::env::remove_var("PI_HTTP_REQUEST_TIMEOUT_SECS");
        std::env::remove_var("RX4_HTTP_TIMEOUT_SECS");

        assert_eq!(
            timeout_for_provider("ollama"),
            Duration::from_secs(LOCAL_TIMEOUT_SECS)
        );
        assert_eq!(
            timeout_for_provider("lmstudio"),
            Duration::from_secs(LOCAL_TIMEOUT_SECS)
        );
        assert_eq!(
            timeout_for_provider("localhost"),
            Duration::from_secs(LOCAL_TIMEOUT_SECS)
        );
        assert_eq!(
            timeout_for_provider("http://127.0.0.1:11434"),
            Duration::from_secs(LOCAL_TIMEOUT_SECS)
        );

        restore_env("PI_HTTP_REQUEST_TIMEOUT_SECS", saved_pi);
        restore_env("RX4_HTTP_TIMEOUT_SECS", saved_rx4);
    }

    #[test]
    fn env_override_timeout() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved_pi = std::env::var("PI_HTTP_REQUEST_TIMEOUT_SECS").ok();
        let saved_rx4 = std::env::var("RX4_HTTP_TIMEOUT_SECS").ok();
        std::env::remove_var("RX4_HTTP_TIMEOUT_SECS");

        std::env::set_var("PI_HTTP_REQUEST_TIMEOUT_SECS", "42");
        assert_eq!(timeout_for_provider("openai"), Duration::from_secs(42));
        assert_eq!(timeout_for_provider("ollama"), Duration::from_secs(42));
        std::env::remove_var("PI_HTTP_REQUEST_TIMEOUT_SECS");

        std::env::set_var("RX4_HTTP_TIMEOUT_SECS", "7");
        assert_eq!(timeout_for_provider("openai"), Duration::from_secs(7));
        assert_eq!(timeout_for_provider("ollama"), Duration::from_secs(7));
        std::env::remove_var("RX4_HTTP_TIMEOUT_SECS");

        restore_env("PI_HTTP_REQUEST_TIMEOUT_SECS", saved_pi);
        restore_env("RX4_HTTP_TIMEOUT_SECS", saved_rx4);
    }

    #[test]
    fn global_client_is_cached() {
        let a = global_client();
        let b = global_client();
        assert!(
            std::ptr::eq(a, b),
            "global_client should return the same pointer"
        );
    }

    #[test]
    fn http_client_creation() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved_pi = std::env::var("PI_HTTP_REQUEST_TIMEOUT_SECS").ok();
        let saved_rx4 = std::env::var("RX4_HTTP_TIMEOUT_SECS").ok();
        std::env::remove_var("PI_HTTP_REQUEST_TIMEOUT_SECS");
        std::env::remove_var("RX4_HTTP_TIMEOUT_SECS");

        let client = HttpClient::new();
        assert_eq!(client.provider(), "openai");
        assert_eq!(
            client.timeout_config().request_timeout,
            Duration::from_secs(CLOUD_TIMEOUT_SECS)
        );
        let _ = client.client();

        restore_env("PI_HTTP_REQUEST_TIMEOUT_SECS", saved_pi);
        restore_env("RX4_HTTP_TIMEOUT_SECS", saved_rx4);
    }

    #[test]
    fn http_client_with_local_provider() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved_pi = std::env::var("PI_HTTP_REQUEST_TIMEOUT_SECS").ok();
        let saved_rx4 = std::env::var("RX4_HTTP_TIMEOUT_SECS").ok();
        std::env::remove_var("PI_HTTP_REQUEST_TIMEOUT_SECS");
        std::env::remove_var("RX4_HTTP_TIMEOUT_SECS");

        let client = HttpClient::with_provider("ollama");
        assert_eq!(client.provider(), "ollama");
        assert_eq!(
            client.timeout_config().request_timeout,
            Duration::from_secs(LOCAL_TIMEOUT_SECS)
        );

        restore_env("PI_HTTP_REQUEST_TIMEOUT_SECS", saved_pi);
        restore_env("RX4_HTTP_TIMEOUT_SECS", saved_rx4);
    }

    #[test]
    fn http_client_with_explicit_timeout_config() {
        let config = TimeoutConfig {
            request_timeout: Duration::from_secs(120),
            connect_timeout: Duration::from_secs(5),
            pool_idle_timeout: Duration::from_secs(30),
            tcp_keepalive: Duration::from_secs(15),
        };
        let client = HttpClient::with_timeout_config("custom", config);
        assert_eq!(client.provider(), "custom");
        assert_eq!(
            client.timeout_config().request_timeout,
            Duration::from_secs(120)
        );
        assert_eq!(
            client.timeout_config().connect_timeout,
            Duration::from_secs(5)
        );
    }

    fn restore_env(key: &str, value: Option<String>) {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
