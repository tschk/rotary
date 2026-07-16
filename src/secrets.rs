//! Secrets: detection and redaction of common credential patterns in text
//! and environment variables.
//!
//! Uses plain string scanning — no regex dependency. Patterns cover API keys,
//! GitHub tokens, JWTs, PEM private key blocks, connection-string passwords,
//! bearer tokens, and sensitive environment variable names.

/// Common secret patterns detected by the [`Redactor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretPattern {
    /// `sk-`, `sk_ant-`, `xai-` prefixes followed by 20+ alphanumeric chars.
    ApiKey,
    /// `ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_` prefixes followed by 36 chars.
    GithubToken,
    /// Three base64url segments separated by dots (`eyJ...`).
    JwtToken,
    /// `-----BEGIN ... PRIVATE KEY-----` blocks.
    PrivateKey,
    /// `password=`, `passwd=`, `pwd=` in connection strings.
    ConnectionString,
    /// `Bearer ` followed by base64/hex chars.
    BearerToken,
    /// Environment variable names containing SECRET/KEY/TOKEN/PASSWORD/CREDENTIAL.
    EnvSecret,
}

impl SecretPattern {
    /// The label used in `[REDACTED:label]` replacement text.
    pub fn as_label(self) -> &'static str {
        match self {
            SecretPattern::ApiKey => "api-key",
            SecretPattern::GithubToken => "github-token",
            SecretPattern::JwtToken => "jwt",
            SecretPattern::PrivateKey => "private-key",
            SecretPattern::ConnectionString => "connection-string",
            SecretPattern::BearerToken => "bearer-token",
            SecretPattern::EnvSecret => "env-secret",
        }
    }

    fn redacted(self) -> String {
        format!("[REDACTED:{}]", self.as_label())
    }
}

/// A single detected secret span within input text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMatch {
    /// Which pattern matched.
    pub pattern: SecretPattern,
    /// Byte offset where the secret starts.
    pub start: usize,
    /// Byte offset where the secret ends (exclusive).
    pub end: usize,
    /// The replacement text that would be substituted for this span.
    pub redacted: String,
}

/// A custom pattern added to a [`RedactionConfig`]. Matched by literal
/// substring (case-sensitive) on the trigger, redacting the whole span from
/// the trigger start to the next whitespace or end-of-string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexPattern {
    trigger: String,
    label: String,
}

impl RegexPattern {
    /// Create a custom pattern. `trigger` is matched literally; the redacted
    /// span extends from the trigger start to the next whitespace boundary.
    pub fn new(trigger: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            trigger: trigger.into(),
            label: label.into(),
        }
    }
}

/// Configuration controlling redaction behavior.
#[derive(Debug, Clone)]
pub struct RedactionConfig {
    /// Whether redaction is active. When `false`, [`Redactor::redact`]
    /// returns input unchanged.
    pub enabled: bool,
    /// User-supplied custom patterns.
    pub custom_patterns: Vec<RegexPattern>,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            custom_patterns: Vec::new(),
        }
    }
}

/// Detects and redacts secrets in text.
#[derive(Debug, Clone)]
pub struct Redactor {
    config: RedactionConfig,
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor {
    /// Create a redactor with all default patterns enabled.
    pub fn new() -> Self {
        Self {
            config: RedactionConfig::default(),
        }
    }

    /// Create a redactor from explicit config.
    pub fn with_config(config: RedactionConfig) -> Self {
        Self { config }
    }

    /// Returns a reference to the active config.
    pub fn config(&self) -> &RedactionConfig {
        &self.config
    }

    /// Add a custom pattern.
    pub fn add_custom_pattern(&mut self, pattern: RegexPattern) {
        self.config.custom_patterns.push(pattern);
    }

    /// Replace every detected secret with `[REDACTED:type]`.
    pub fn redact(&self, text: &str) -> String {
        if !self.config.enabled {
            return text.to_string();
        }
        let matches = self.find_secrets(text);
        if matches.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0usize;
        for m in matches {
            out.push_str(&text[cursor..m.start]);
            out.push_str(&m.redacted);
            cursor = m.end;
        }
        out.push_str(&text[cursor..]);
        out
    }

    /// Redact in place, reusing the input buffer when possible.
    pub fn redact_in_place(&self, text: &mut String) {
        let replacement = self.redact(text);
        if &replacement != text {
            *text = replacement;
        }
    }

    /// Locate every secret span without modifying the input.
    pub fn find_secrets(&self, text: &str) -> Vec<SecretMatch> {
        let mut matches = Vec::new();
        if !self.config.enabled {
            return matches;
        }
        find_api_keys(text, &mut matches);
        find_github_tokens(text, &mut matches);
        find_jwts(text, &mut matches);
        find_private_keys(text, &mut matches);
        find_connection_strings(text, &mut matches);
        find_bearer_tokens(text, &mut matches);
        find_env_secrets(text, &mut matches);
        for custom in &self.config.custom_patterns {
            find_custom(text, custom, &mut matches);
        }
        // Sort by start offset; drop overlaps keeping earlier/longer spans.
        matches.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
        let mut filtered: Vec<SecretMatch> = Vec::new();
        for m in matches {
            if filtered.last().is_some_and(|prev| m.start < prev.end) {
                continue;
            }
            filtered.push(m);
        }
        filtered
    }
}

fn is_alphanumeric(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

fn is_base64url(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn is_hex_or_base64(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '+' || c == '/' || c == '='
}

fn run_len_at(text: &str, byte_idx: usize, pred: impl Fn(char) -> bool) -> usize {
    let mut len = 0usize;
    let mut idx = byte_idx;
    let bytes = text.as_bytes();
    while idx < bytes.len() {
        let rest = &text[idx..];
        let mut chars = rest.chars();
        let Some(c) = chars.next() else { break };
        if !pred(c) {
            break;
        }
        len += c.len_utf8();
        idx += c.len_utf8();
    }
    len
}

fn find_prefix_runs(
    text: &str,
    prefixes: &[&str],
    min_run: usize,
    pred: impl Fn(char) -> bool + Copy,
    pattern: SecretPattern,
    out: &mut Vec<SecretMatch>,
) {
    let mut search_from = 0usize;
    while search_from <= text.len() {
        let mut found_any = false;
        for &prefix in prefixes {
            let Some(start) = text[search_from..].find(prefix) else {
                continue;
            };
            let abs = search_from + start;
            let body_start = abs + prefix.len();
            let run = run_len_at(text, body_start, pred);
            if run >= min_run {
                let end = body_start + run;
                out.push(SecretMatch {
                    pattern,
                    start: abs,
                    end,
                    redacted: pattern.redacted(),
                });
                search_from = end;
                found_any = true;
                break;
            }
        }
        if !found_any {
            search_from += 1;
        }
    }
}

fn find_api_keys(text: &str, out: &mut Vec<SecretMatch>) {
    find_prefix_runs(
        text,
        &["sk-", "sk_ant-", "xai-"],
        20,
        is_alphanumeric,
        SecretPattern::ApiKey,
        out,
    );
}

fn find_github_tokens(text: &str, out: &mut Vec<SecretMatch>) {
    find_prefix_runs(
        text,
        &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
        36,
        is_alphanumeric,
        SecretPattern::GithubToken,
        out,
    );
}

fn find_jwts(text: &str, out: &mut Vec<SecretMatch>) {
    let mut search_from = 0usize;
    while search_from < text.len() {
        let Some(rel) = text[search_from..].find("eyJ") else {
            break;
        };
        let start = search_from + rel;
        let seg1 = run_len_at(text, start, is_base64url);
        if seg1 < 8 {
            search_from = start + 1;
            continue;
        }
        let mut idx = start + seg1;
        if !text[idx..].starts_with('.') {
            search_from = start + 1;
            continue;
        }
        idx += 1;
        let seg2 = run_len_at(text, idx, is_base64url);
        if seg2 < 8 {
            search_from = start + 1;
            continue;
        }
        idx += seg2;
        if !text[idx..].starts_with('.') {
            search_from = start + 1;
            continue;
        }
        idx += 1;
        let seg3 = run_len_at(text, idx, is_base64url);
        if seg3 < 8 {
            search_from = start + 1;
            continue;
        }
        let end = idx + seg3;
        out.push(SecretMatch {
            pattern: SecretPattern::JwtToken,
            start,
            end,
            redacted: SecretPattern::JwtToken.redacted(),
        });
        search_from = end;
    }
}

fn find_private_keys(text: &str, out: &mut Vec<SecretMatch>) {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    let mut search_from = 0usize;
    while search_from < text.len() {
        let Some(rel) = text[search_from..].find(BEGIN) else {
            break;
        };
        let start = search_from + rel;
        let Some(header_end_rel) = text[start..].find("PRIVATE KEY-----") else {
            search_from = start + BEGIN.len();
            continue;
        };
        let header_end = start + header_end_rel + "PRIVATE KEY-----".len();
        let Some(end_rel) = text[header_end..].find(END) else {
            search_from = header_end;
            continue;
        };
        let end_marker = header_end + end_rel;
        let after_end_prefix = end_marker + END.len();
        let Some(close_rel) = text[after_end_prefix..].find("-----") else {
            search_from = end_marker + END.len();
            continue;
        };
        let end = after_end_prefix + close_rel + "-----".len();
        out.push(SecretMatch {
            pattern: SecretPattern::PrivateKey,
            start,
            end,
            redacted: SecretPattern::PrivateKey.redacted(),
        });
        search_from = end;
    }
}

fn find_connection_strings(text: &str, out: &mut Vec<SecretMatch>) {
    let lower = text.to_ascii_lowercase();
    for keyword in &["password=", "passwd=", "pwd="] {
        let mut search_from = 0usize;
        while search_from < text.len() {
            let Some(rel_lower) = lower[search_from..].find(keyword) else {
                break;
            };
            let start = search_from + rel_lower;
            let value_start = start + keyword.len();
            let mut end = value_start;
            let bytes = text.as_bytes();
            while end < bytes.len() {
                let c = text[end..].chars().next().unwrap();
                if c == ' ' || c == ';' || c == '\n' || c == '\r' || c == '\t' {
                    break;
                }
                end += c.len_utf8();
            }
            if end > value_start {
                out.push(SecretMatch {
                    pattern: SecretPattern::ConnectionString,
                    start,
                    end,
                    redacted: SecretPattern::ConnectionString.redacted(),
                });
            }
            search_from = end.max(value_start + 1);
        }
    }
}

fn find_bearer_tokens(text: &str, out: &mut Vec<SecretMatch>) {
    let mut search_from = 0usize;
    while search_from < text.len() {
        let Some(rel) = text[search_from..].find("Bearer ") else {
            break;
        };
        let start = search_from + rel;
        let value_start = start + "Bearer ".len();
        let run = run_len_at(text, value_start, is_hex_or_base64);
        if run >= 8 {
            let end = value_start + run;
            out.push(SecretMatch {
                pattern: SecretPattern::BearerToken,
                start,
                end,
                redacted: SecretPattern::BearerToken.redacted(),
            });
            search_from = end;
        } else {
            search_from = value_start;
        }
    }
}

fn find_env_secrets(text: &str, out: &mut Vec<SecretMatch>) {
    let mut search_from = 0usize;
    while search_from < text.len() {
        let Some(rel) = text[search_from..].find(|c: char| c.is_ascii_alphabetic() || c == '_')
        else {
            break;
        };
        let start = search_from + rel;
        let run = run_len_at(text, start, |c| c.is_ascii_alphanumeric() || c == '_');
        if run == 0 {
            search_from = start + 1;
            continue;
        }
        let name = &text[start..start + run];
        if is_sensitive_env_var(name) {
            out.push(SecretMatch {
                pattern: SecretPattern::EnvSecret,
                start,
                end: start + run,
                redacted: SecretPattern::EnvSecret.redacted(),
            });
        }
        search_from = start + run + 1;
    }
}

fn find_custom(text: &str, pattern: &RegexPattern, out: &mut Vec<SecretMatch>) {
    let mut search_from = 0usize;
    while search_from < text.len() {
        let Some(rel) = text[search_from..].find(&pattern.trigger) else {
            break;
        };
        let start = search_from + rel;
        let body_start = start + pattern.trigger.len();
        let mut end = body_start;
        let bytes = text.as_bytes();
        while end < bytes.len() {
            let c = text[end..].chars().next().unwrap();
            if c.is_whitespace() {
                break;
            }
            end += c.len_utf8();
        }
        if end == body_start {
            end = body_start;
        }
        out.push(SecretMatch {
            pattern: SecretPattern::ApiKey,
            start,
            end,
            redacted: format!("[REDACTED:{}]", pattern.label),
        });
        search_from = end.max(start + pattern.trigger.len());
    }
}

/// Returns `true` if an environment variable name looks sensitive.
pub fn is_sensitive_env_var(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.contains("SECRET")
        || upper.contains("KEY")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("CREDENTIAL")
}

/// Filter environment variables, redacting values of sensitive names.
///
/// Variables whose names match [`is_sensitive_env_var`] keep their name but
/// have their value replaced with `[REDACTED]`. All other vars pass through
/// unchanged.
pub fn filter_env_vars(vars: &[(String, String)]) -> Vec<(String, String)> {
    vars.iter()
        .map(|(k, v)| {
            if is_sensitive_env_var(k) {
                (k.clone(), "[REDACTED]".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_openai_style_api_key() {
        let r = Redactor::new();
        let body = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        let out = r.redact(body);
        assert_eq!(out, "[REDACTED:api-key]");
        let m = r.find_secrets(body);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].pattern, SecretPattern::ApiKey);
    }

    #[test]
    fn redacts_anthropic_style_api_key() {
        let r = Redactor::new();
        let body = "token=sk_ant-abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        let out = r.redact(body);
        assert!(out.contains("[REDACTED:api-key]"));
        assert!(!out.contains("sk_ant-"));
    }

    #[test]
    fn redacts_xai_style_api_key() {
        let r = Redactor::new();
        let body = "xai-abcdefghijklmnopqrstuvwxyz0123456789";
        assert_eq!(r.redact(body), "[REDACTED:api-key]");
    }

    #[test]
    fn does_not_redact_short_prefix_run() {
        let r = Redactor::new();
        let body = "sk-short";
        assert_eq!(r.redact(body), "sk-short");
    }

    #[test]
    fn redacts_github_token() {
        let r = Redactor::new();
        let body = format!("ghp_{}", "a".repeat(36));
        assert_eq!(r.redact(&body), "[REDACTED:github-token]");
        let body2 = format!("ghs_{}", "Z".repeat(36));
        assert_eq!(r.redact(&body2), "[REDACTED:github-token]");
    }

    #[test]
    fn redacts_jwt_token() {
        let r = Redactor::new();
        let body = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let out = r.redact(body);
        assert_eq!(out, "[REDACTED:jwt]");
    }

    #[test]
    fn redacts_private_key_block() {
        let r = Redactor::new();
        let body =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA...\n-----END RSA PRIVATE KEY-----";
        let out = r.redact(body);
        assert_eq!(out, "[REDACTED:private-key]");
    }

    #[test]
    fn redacts_connection_string_password() {
        let r = Redactor::new();
        let body = "postgres://user:password=hunter2@host/db";
        let out = r.redact(body);
        assert!(out.contains("[REDACTED:connection-string]"));
        assert!(!out.contains("hunter2"));
    }

    #[test]
    fn redacts_connection_string_passwd_and_pwd() {
        let r = Redactor::new();
        let body = "server=db;passwd=secret123;uid=admin";
        let out = r.redact(body);
        assert!(out.contains("[REDACTED:connection-string]"));
        assert!(!out.contains("secret123"));
        let body2 = "server=db;pwd=p4ss;uid=admin";
        let out2 = r.redact(body2);
        assert!(out2.contains("[REDACTED:connection-string]"));
    }

    #[test]
    fn redacts_bearer_token() {
        let r = Redactor::new();
        let body = "Authorization: Bearer dGhpcyBpcyBhIHRva2Vu";
        let out = r.redact(body);
        assert!(out.contains("[REDACTED:bearer-token]"));
        assert!(!out.contains("dGhpcyBpcyBhIHRva2Vu"));
    }

    #[test]
    fn env_var_filter_redacts_sensitive_values() {
        let vars = vec![
            ("API_KEY".to_string(), "sk-1234567890".to_string()),
            (
                "DATABASE_URL".to_string(),
                "postgres://localhost".to_string(),
            ),
            ("MY_TOKEN".to_string(), "abc".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ];
        let filtered = filter_env_vars(&vars);
        assert_eq!(filtered[0].1, "[REDACTED]");
        assert_eq!(filtered[1].1, "postgres://localhost");
        assert_eq!(filtered[2].1, "[REDACTED]");
        assert_eq!(filtered[3].1, "/usr/bin");
    }

    #[test]
    fn is_sensitive_env_var_detects_keywords() {
        assert!(is_sensitive_env_var("OPENAI_API_KEY"));
        assert!(is_sensitive_env_var("GH_TOKEN"));
        assert!(is_sensitive_env_var("DB_PASSWORD"));
        assert!(is_sensitive_env_var("MY_SECRET"));
        assert!(is_sensitive_env_var("AWS_CREDENTIAL"));
        assert!(!is_sensitive_env_var("PATH"));
        assert!(!is_sensitive_env_var("HOME"));
        assert!(!is_sensitive_env_var("USER"));
    }

    #[test]
    fn no_false_positives_on_normal_text() {
        let r = Redactor::new();
        let body = "The quick brown fox jumps over the lazy dog. Path is /usr/bin.";
        assert_eq!(r.redact(body), body);
        assert!(r.find_secrets(body).is_empty());
    }

    #[test]
    fn redact_in_place_modifies_buffer() {
        let r = Redactor::new();
        let mut buf = format!("key=sk-{}", "a".repeat(24));
        r.redact_in_place(&mut buf);
        assert!(buf.contains("[REDACTED:api-key]"));
        assert!(!buf.contains("sk-aaaa"));
    }

    #[test]
    fn redact_disabled_passes_through() {
        let r = Redactor::with_config(RedactionConfig {
            enabled: false,
            custom_patterns: Vec::new(),
        });
        let body = format!("sk-{}", "a".repeat(24));
        assert_eq!(r.redact(&body), body);
    }

    #[test]
    fn custom_pattern_is_applied() {
        let mut r = Redactor::new();
        r.add_custom_pattern(RegexPattern::new("custom-secret:", "custom"));
        let body = "value=custom-secret:abcdef123456";
        let out = r.redact(body);
        assert!(out.contains("[REDACTED:custom]"));
    }

    #[test]
    fn find_secrets_returns_sorted_non_overlapping() {
        let r = Redactor::new();
        let body = format!("first sk-{} then ghp_{}", "a".repeat(24), "b".repeat(36));
        let m = r.find_secrets(&body);
        assert_eq!(m.len(), 2);
        assert!(m[0].start < m[1].start);
        assert!(m[0].end <= m[1].start);
    }

    #[test]
    fn env_secret_pattern_matches_inline_name() {
        let r = Redactor::new();
        let body = "set API_KEY to value";
        let m = r.find_secrets(body);
        assert!(m.iter().any(|m| m.pattern == SecretPattern::EnvSecret));
    }
}
