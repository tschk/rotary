use super::common::{parse_num_field, parse_str_field};
use crate::agent::{ToolContext, ToolFuture, ToolResult};
use crate::mode::Scope;
use crate::subagent::SubagentManager;
use dashmap::DashMap;
use parking_lot::Mutex;
#[cfg(any(test, feature = "providers"))]
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};

/// Parsed `web_fetch` target (scheme already checked).
#[cfg(any(test, feature = "providers"))]
struct FetchTarget {
    host: String,
    port: u16,
}

/// Validate a URL for web_fetch: only http(s), reject loopback / private /
/// link-local / metadata hosts. Does not perform DNS; see
/// [`validate_fetch_destination`].
#[cfg(any(test, feature = "providers"))]
fn validate_fetch_url(url: &str) -> Result<(), String> {
    let target = parse_fetch_target(url)?;
    host_is_blocked(&target.host)
}

#[cfg(any(test, feature = "providers"))]
fn parse_fetch_target(url: &str) -> Result<FetchTarget, String> {
    let url = url.trim();
    let Some(scheme_end) = url.find("://") else {
        return Err("URL must include a scheme".into());
    };
    let scheme = &url[..scheme_end];
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(format!("scheme not allowed: {scheme}"));
    }
    let rest = &url[scheme_end + 3..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return Err("URL has no host".into());
    }
    let hostport = authority
        .rsplit_once('@')
        .map(|(_, hostport)| hostport)
        .unwrap_or(authority);
    let (host, port_override) = if let Some(end) = hostport.strip_prefix('[') {
        let Some(close) = end.find(']') else {
            return Err("invalid IPv6 URL".into());
        };
        let host = &end[..close];
        let after = &end[close + 1..];
        let port = if let Some(p) = after.strip_prefix(':') {
            Some(parse_port(p)?)
        } else if after.is_empty() {
            None
        } else {
            return Err("invalid IPv6 URL".into());
        };
        (host, port)
    } else if let Some((host, port)) = hostport.rsplit_once(':') {
        if host.is_empty() {
            return Err("URL has no host".into());
        }
        (host, Some(parse_port(port)?))
    } else {
        (hostport, None)
    };
    if host.is_empty() {
        return Err("URL has no host".into());
    }
    let port = port_override.unwrap_or(if scheme.eq_ignore_ascii_case("https") {
        443
    } else {
        80
    });
    Ok(FetchTarget {
        host: host.to_string(),
        port,
    })
}

#[cfg(any(test, feature = "providers"))]
fn parse_port(raw: &str) -> Result<u16, String> {
    raw.parse::<u16>()
        .map_err(|_| format!("invalid port: {raw}"))
}

#[cfg(any(test, feature = "providers"))]
fn host_is_blocked(host: &str) -> Result<(), String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err("URL has no host".into());
    }
    if host == "localhost"
        || host == "metadata.google.internal"
        || host == "metadata.goog"
        || host.ends_with(".localhost")
        || host.ends_with(".internal")
    {
        return Err(format!("blocked host: {host}"));
    }
    if let Some(ip) = parse_literal_ip(&host) {
        if ip_is_blocked(ip) {
            return Err(format!("blocked address: {host}"));
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "providers"))]
fn parse_literal_ip(host: &str) -> Option<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(ip);
    }
    parse_abbreviated_ipv4(host).map(IpAddr::V4)
}

/// inet_aton-style IPv4: `2130706433`, `127.1`, `127.0.1`.
#[cfg(any(test, feature = "providers"))]
fn parse_abbreviated_ipv4(host: &str) -> Option<Ipv4Addr> {
    if host.bytes().all(|b| b.is_ascii_digit()) {
        return host.parse::<u32>().ok().map(Ipv4Addr::from);
    }
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() < 2 || parts.len() > 4 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let nums: Option<Vec<u32>> = parts.iter().map(|p| p.parse().ok()).collect();
    let nums = nums?;
    let addr = match nums.as_slice() {
        [a, b] if *a <= 0xff && *b <= 0xff_ffff => (*a << 24) | *b,
        [a, b, c] if *a <= 0xff && *b <= 0xff && *c <= 0xffff => (*a << 24) | (*b << 16) | *c,
        [a, b, c, d] if *a <= 0xff && *b <= 0xff && *c <= 0xff && *d <= 0xff => {
            (a << 24) | (b << 16) | (c << 8) | d
        }
        _ => return None,
    };
    Some(Ipv4Addr::from(addr))
}

#[cfg(any(test, feature = "providers"))]
fn ip_is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_is_blocked(v4),
        IpAddr::V6(v6) => ipv6_is_blocked(v6),
    }
}

#[cfg(any(test, feature = "providers"))]
fn ipv4_is_blocked(ip: Ipv4Addr) -> bool {
    if ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
    {
        return true;
    }
    let o = ip.octets();
    // 0.0.0.0/8, CGNAT 100.64.0.0/10, IETF protocol 192.0.0.0/24, reserved 240.0.0.0/4
    o[0] == 0
        || (o[0] == 100 && (64..128).contains(&o[1]))
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || o[0] >= 240
}

#[cfg(any(test, feature = "providers"))]
fn ipv6_is_blocked(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return ipv4_is_blocked(v4);
    }
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unicast_link_local()
        || ip.is_unique_local()
}

#[cfg(any(test, feature = "providers"))]
async fn validate_fetch_destination(url: &str) -> Result<(), String> {
    let target = parse_fetch_target(url)?;
    host_is_blocked(&target.host)?;
    if parse_literal_ip(&target.host).is_some() {
        return Ok(());
    }
    let host = target.host.clone();
    let port = target.port;
    let addrs = tokio::task::spawn_blocking(move || (host.as_str(), port).to_socket_addrs())
        .await
        .map_err(|e| format!("dns lookup failed: {e}"))?
        .map_err(|e| format!("dns lookup failed: {e}"))?;
    let mut any = false;
    for addr in addrs {
        any = true;
        if ip_is_blocked(addr.ip()) {
            return Err(format!("blocked resolved address: {}", addr.ip()));
        }
    }
    if !any {
        return Err("dns lookup returned no addresses".into());
    }
    Ok(())
}

#[cfg(feature = "providers")]
fn web_fetch_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if validate_fetch_url(attempt.url().as_str()).is_err() {
                    attempt.error(std::io::Error::other("redirect to blocked URL"))
                } else if attempt.previous().len() >= 5 {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .http1_only()
            .build()
            .expect("web_fetch client")
    })
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct TodoItem {
    id: String,
    content: String,
    status: String,
}

fn todo_store() -> &'static DashMap<String, Vec<TodoItem>> {
    static STORE: OnceLock<DashMap<String, Vec<TodoItem>>> = OnceLock::new();
    STORE.get_or_init(DashMap::new)
}

fn workspace_key(ctx: &ToolContext) -> String {
    ctx.workspace_root.to_string_lossy().into_owned()
}

pub(crate) fn exec_web_fetch(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let url = match parse_str_field(&args, "url") {
            Some(u) => u,
            None => return ToolResult::err("web_fetch", "url required"),
        };
        let max_bytes = parse_num_field(&args, "max_bytes").unwrap_or(100_000) as usize;
        let max_bytes = max_bytes.min(100_000);

        if let Some(sb) = ctx.sandbox.as_ref() {
            if let Err(e) = sb.validate_network() {
                return ToolResult::err("web_fetch", e.to_string());
            }
        }

        #[cfg(feature = "providers")]
        {
            // Reject destinations that should not be fetched: non-http(s),
            // loopback, private, link-local, metadata, and DNS rebinding.
            if let Err(e) = validate_fetch_destination(&url).await {
                return ToolResult::err("web_fetch", e);
            }
            let client = web_fetch_client();
            match client.get(&url).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    match resp.bytes().await {
                        Ok(bytes) => {
                            let truncated = bytes.len() > max_bytes;
                            let slice = &bytes[..bytes.len().min(max_bytes)];
                            let mut text = String::from_utf8_lossy(slice).into_owned();
                            if truncated {
                                text.push_str(&format!(
                                    "\n\n[truncated to {max_bytes} bytes; total {}]",
                                    bytes.len()
                                ));
                            }
                            if !status.is_success() {
                                text = format!("HTTP {status}\n{text}");
                            }
                            ToolResult::ok("web_fetch", text)
                        }
                        Err(e) => ToolResult::err("web_fetch", format!("body read failed: {e}")),
                    }
                }
                Err(e) => ToolResult::err("web_fetch", format!("request failed: {e}")),
            }
        }

        #[cfg(not(feature = "providers"))]
        {
            let _ = max_bytes;
            ToolResult::err(
                "web_fetch",
                format!("providers feature required to fetch {url}"),
            )
        }
    })
}

pub(crate) fn exec_todo(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let v: serde_json::Value = match serde_json::from_str(&args) {
            Ok(v) => v,
            Err(e) => return ToolResult::err("todo", format!("invalid json: {e}")),
        };
        if let (Some(state), Some(config)) = (&ctx.todo_state, &ctx.todo_config) {
            let mutation = match crate::todo::apply_request(&mut state.write(), config, &v) {
                Ok(mutation) => mutation,
                Err(error) => return ToolResult::err("todo", error),
            };
            if let Some(updates) = &ctx.todo_updates {
                updates.lock().push(mutation.state.clone());
            }
            if mutation.verification_required {
                return ToolResult::ok(
                    "todo",
                    format!(
                        "{}\n\nRE-VERIFY this item with concrete evidence — run the code/tests, don't assert. Then call todo complete again with completion_confidence.",
                        serde_json::to_string_pretty(&mutation.state).unwrap_or_else(|_| "{}".into())
                    ),
                );
            }
            return ToolResult::ok(
                "todo",
                serde_json::to_string_pretty(&mutation.state).unwrap_or_else(|_| "{}".into()),
            );
        }
        let action = match v.get("action").and_then(|a| a.as_str()) {
            Some(a) => a,
            None => return ToolResult::err("todo", "action required"),
        };
        let key = workspace_key(&ctx);
        let store = todo_store();

        match action {
            "list" => {
                let items = store.get(&key).map(|e| e.clone()).unwrap_or_default();
                ToolResult::ok(
                    "todo",
                    serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into()),
                )
            }
            "clear" => {
                store.insert(key, Vec::new());
                ToolResult::ok("todo", "[]")
            }
            "add" => {
                let mut items = store.get(&key).map(|e| e.clone()).unwrap_or_default();
                let incoming = v
                    .get("items")
                    .and_then(|i| i.as_array())
                    .cloned()
                    .unwrap_or_default();
                if incoming.is_empty() {
                    return ToolResult::err("todo", "items required for add");
                }
                for item in incoming {
                    let content = item
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    if content.is_empty() {
                        continue;
                    }
                    let id = item
                        .get("id")
                        .and_then(|i| i.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    let status = item
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("pending")
                        .to_string();
                    items.push(TodoItem {
                        id,
                        content,
                        status,
                    });
                }
                let out = serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into());
                store.insert(key, items);
                ToolResult::ok("todo", out)
            }
            "update" | "complete" => {
                let mut items = store.get(&key).map(|e| e.clone()).unwrap_or_default();
                let incoming = v
                    .get("items")
                    .and_then(|i| i.as_array())
                    .cloned()
                    .unwrap_or_default();
                if incoming.is_empty() {
                    return ToolResult::err("todo", format!("items required for {action}"));
                }
                for item in incoming {
                    let id = item.get("id").and_then(|i| i.as_str());
                    let content = item.get("content").and_then(|c| c.as_str());
                    let status = if action == "complete" {
                        Some("completed")
                    } else {
                        item.get("status").and_then(|s| s.as_str())
                    };
                    if let Some(id) = id {
                        if let Some(existing) = items.iter_mut().find(|t| t.id == id) {
                            if let Some(c) = content {
                                existing.content = c.to_string();
                            }
                            if let Some(s) = status {
                                existing.status = s.to_string();
                            }
                        }
                    } else if let Some(c) = content {
                        if let Some(existing) = items.iter_mut().find(|t| t.content == c) {
                            if let Some(s) = status {
                                existing.status = s.to_string();
                            }
                        }
                    }
                }
                let out = serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into());
                store.insert(key, items);
                ToolResult::ok("todo", out)
            }
            other => ToolResult::err("todo", format!("unknown action: {other}")),
        }
    })
}

pub(crate) fn exec_spawn_agent(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    let mut manager = SubagentManager::new();
    if let Some(provider) = ctx.provider.clone() {
        manager = manager.with_provider(provider);
    }
    if let Some(tools) = ctx.tools.clone() {
        manager = manager.with_tools(tools);
    }
    Box::pin(super::execute_spawn_agent(
        Arc::new(Mutex::new(manager)),
        ctx,
        args,
    ))
}

pub(crate) fn exec_enter_plan_mode(ctx: Arc<ToolContext>, _args: String) -> ToolFuture {
    Box::pin(async move {
        if let Some(slot) = ctx.pending_scope.as_ref() {
            *slot.lock() = Some(Scope::Plan);
        }
        ToolResult::ok(
            "enter_plan_mode",
            "Entered plan mode. Produce a concrete multi-step plan: files, risks, verification. Do not modify the workspace. Host should set_scope(Plan).",
        )
    })
}

pub(crate) fn exec_exit_plan_mode(ctx: Arc<ToolContext>, _args: String) -> ToolFuture {
    Box::pin(async move {
        if let Some(slot) = ctx.pending_scope.as_ref() {
            *slot.lock() = Some(Scope::Coding);
        }
        ToolResult::ok(
            "exit_plan_mode",
            "Exited plan mode. Resume coding scope. Host should set_scope(Coding).",
        )
    })
}

pub(crate) fn exec_lsp_diagnostics(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        #[cfg(feature = "ipc")]
        {
            let uri = match parse_str_field(&args, "uri") {
                Some(u) => u,
                None => return ToolResult::err("lsp_diagnostics", "uri required"),
            };
            let language = match parse_str_field(&args, "language") {
                Some(l) => l,
                None => return ToolResult::err("lsp_diagnostics", "language required"),
            };
            let Some(lsp) = ctx.lsp.clone() else {
                return ToolResult::err("lsp_diagnostics", "lsp not configured");
            };
            match lsp.diagnostics(&uri, &language).await {
                Ok(diags) => ToolResult::ok(
                    "lsp_diagnostics",
                    serde_json::to_string_pretty(&diags).unwrap_or_else(|e| e.to_string()),
                ),
                Err(e) => ToolResult::err("lsp_diagnostics", e.to_string()),
            }
        }
        #[cfg(not(feature = "ipc"))]
        {
            let _ = (ctx, args);
            ToolResult::err("lsp_diagnostics", "lsp not configured")
        }
    })
}

pub(crate) fn exec_lsp_definition(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        #[cfg(feature = "ipc")]
        {
            let uri = match parse_str_field(&args, "uri") {
                Some(u) => u,
                None => return ToolResult::err("lsp_definition", "uri required"),
            };
            let language = match parse_str_field(&args, "language") {
                Some(l) => l,
                None => return ToolResult::err("lsp_definition", "language required"),
            };
            let line = match parse_num_field(&args, "line") {
                Some(n) => n as u32,
                None => return ToolResult::err("lsp_definition", "line required"),
            };
            let character = parse_num_field(&args, "character")
                .or_else(|| parse_num_field(&args, "char"))
                .unwrap_or(0) as u32;
            let Some(lsp) = ctx.lsp.clone() else {
                return ToolResult::err("lsp_definition", "lsp not configured");
            };
            match lsp.definition(&uri, &language, line, character).await {
                Ok(locs) => ToolResult::ok(
                    "lsp_definition",
                    serde_json::to_string_pretty(&locs).unwrap_or_else(|e| e.to_string()),
                ),
                Err(e) => ToolResult::err("lsp_definition", e.to_string()),
            }
        }
        #[cfg(not(feature = "ipc"))]
        {
            let _ = (ctx, args);
            ToolResult::err("lsp_definition", "lsp not configured")
        }
    })
}

pub(crate) fn exec_lsp_references(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        #[cfg(feature = "ipc")]
        {
            let uri = match parse_str_field(&args, "uri") {
                Some(u) => u,
                None => return ToolResult::err("lsp_references", "uri required"),
            };
            let language = match parse_str_field(&args, "language") {
                Some(l) => l,
                None => return ToolResult::err("lsp_references", "language required"),
            };
            let line = match parse_num_field(&args, "line") {
                Some(n) => n as u32,
                None => return ToolResult::err("lsp_references", "line required"),
            };
            let character = parse_num_field(&args, "character")
                .or_else(|| parse_num_field(&args, "char"))
                .unwrap_or(0) as u32;
            let Some(lsp) = ctx.lsp.clone() else {
                return ToolResult::err("lsp_references", "lsp not configured");
            };
            match lsp.references(&uri, &language, line, character).await {
                Ok(locs) => ToolResult::ok(
                    "lsp_references",
                    serde_json::to_string_pretty(&locs).unwrap_or_else(|e| e.to_string()),
                ),
                Err(e) => ToolResult::err("lsp_references", e.to_string()),
            }
        }
        #[cfg(not(feature = "ipc"))]
        {
            let _ = (ctx, args);
            ToolResult::err("lsp_references", "lsp not configured")
        }
    })
}

#[cfg(test)]
mod fetch_url_tests {
    use super::*;

    #[test]
    fn rejects_non_http_schemes() {
        assert!(validate_fetch_url("file:///etc/passwd").is_err());
        assert!(validate_fetch_url("ftp://example.com/").is_err());
        assert!(validate_fetch_url("gopher://example.com/").is_err());
        assert!(validate_fetch_url("example.com/no-scheme").is_err());
    }

    #[test]
    fn rejects_loopback_and_unspecified() {
        for url in [
            "http://127.0.0.1/",
            "http://127.0.0.1:8080/secret",
            "https://localhost/meta",
            "http://[::1]/",
            "http://0.0.0.0/",
            "http://2130706433/",
            "http://[::ffff:127.0.0.1]/",
        ] {
            assert!(validate_fetch_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn rejects_private_link_local_and_metadata() {
        for url in [
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://metadata.google.internal/",
            "http://100.64.0.1/",
            "http://[fe80::1]/",
            "http://[fd00::1]/",
        ] {
            assert!(validate_fetch_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn userinfo_does_not_hide_host() {
        assert!(validate_fetch_url("http://user:pass@127.0.0.1/").is_err());
        assert!(validate_fetch_url("https://evil@169.254.169.254/").is_err());
    }

    #[test]
    fn allows_public_https_hosts() {
        assert!(validate_fetch_url("https://example.com/path").is_ok());
        assert!(validate_fetch_url("https://example.com:8443/").is_ok());
        assert!(validate_fetch_url("http://1.1.1.1/").is_ok());
        assert!(validate_fetch_url("https://[2001:4860:4860::8888]/").is_ok());
    }

    #[tokio::test]
    async fn destination_check_blocks_loopback_literals() {
        let err = validate_fetch_destination("http://127.1/")
            .await
            .unwrap_err();
        assert!(err.contains("blocked"), "{err}");
    }
}
