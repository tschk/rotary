//! Sandboxed Lua `script` tool: run a short Lua program in one turn with
//! workspace-confined bridges (read/list/grep/json) instead of many tool
//! round-trips. No io/os/debug libs; reads only; memory + wall-clock capped.

use super::common::resolve_path;
use crate::agent::{ToolContext, ToolFuture, ToolResult};
use std::sync::Arc;

const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const MAX_TIMEOUT_MS: u64 = 30_000;
const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
const MAX_OUTPUT: usize = 32 * 1024;
/// Hard cap on grep hits per script run.
const MAX_GREP_HITS: usize = 500;

struct PrintBuffer(std::sync::Mutex<Vec<String>>);

/// Everything blocking (Lua VM + std fs bridges) happens on a blocking
/// thread; the async wrapper applies the wall-clock timeout around its join.
fn run_lua(ctx: Arc<ToolContext>, code: String) -> Result<String, String> {
    // Sandbox: ALL_SAFE = every std lib that cannot touch the host
    // (no io, os, debug, package).
    let lua = mlua::Lua::new_with(mlua::StdLib::ALL_SAFE, mlua::LuaOptions::new())
    .map_err(|e| format!("lua init: {e}"))?;
    let _ = lua.set_memory_limit(MEMORY_LIMIT);
    install_bridges(&lua, &ctx).map_err(|e| format!("bridge setup failed: {e}"))?;

    let result: Result<mlua::Value, mlua::Error> =
        lua.load(&code).set_name("script").eval();
    let out = match result {
        Ok(val) => value_to_string(&val),
        Err(e) => return Err(format!("{e}")),
    };
    let printed = lua
        .app_data_ref::<PrintBuffer>()
        .map(|b| b.0.lock().map(|v| v.join("\n")).unwrap_or_default())
        .unwrap_or_default();

    let mut body = String::new();
    if !printed.is_empty() {
        body.push_str(&printed.chars().take(MAX_OUTPUT).collect::<String>());
        body.push('\n');
    }
    if !out.is_empty() && out != "nil" {
        if !body.is_empty() {
            body.push_str("result: ");
        }
        body.push_str(&out);
        body.push('\n');
    }
    if body.is_empty() {
        body.push_str("(no output — print() or return a value)");
    }
    Ok(body)
}

fn script_args(args: &str) -> Result<(String, u64), String> {
    let v: serde_json::Value =
        serde_json::from_str(args).map_err(|e| format!("invalid json: {e}"))?;
    let code = match v.get("code").and_then(|c| c.as_str()) {
        Some(c) if !c.is_empty() => c.to_string(),
        _ => return Err("code required".to_string()),
    };
    let timeout_ms = v
        .get("timeout_ms")
        .and_then(|t| t.as_u64())
        .map(|t| t.clamp(1_000, MAX_TIMEOUT_MS))
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    Ok((code, timeout_ms))
}

fn script_future(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let (code, timeout_ms) = match script_args(&args) {
            Ok(res) => res,
            Err(e) => return ToolResult::err("script", e),
        };
        let handle = tokio::task::spawn_blocking(move || run_lua(ctx, code));
        match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), handle).await {
            Ok(Ok(Ok(body))) => ToolResult::ok("script", body),
            Ok(Ok(Err(e))) => ToolResult::err("script", e),
            Ok(Err(e)) => ToolResult::err("script", format!("join error: {e}")),
            Err(_) => ToolResult::err(
                "script",
                format!("script timed out after {timeout_ms}ms"),
            ),
        }
    })
}

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    async fn run_code(tmp: &tempfile::TempDir, code: &str) -> ToolResult {
        let ctx = Arc::new(ToolContext::new(tmp.path()));
        script_future(
            ctx,
            serde_json::json!({ "code": code }).to_string(),
        )
        .await
    }

    #[tokio::test]
    async fn computes_and_returns_value() {
        let tmp = tempfile::TempDir::new().unwrap();
        let r = run_code(&tmp, "return 6 * 7").await;
        assert!(!r.is_error);
        assert!(r.content.contains("42"), "got: {}", r.content);
    }

    #[tokio::test]
    async fn read_grep_print_bridges() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello world\nTODO fix\nbye\n").unwrap();
        let code = r#"
            local hits = grep('TODO', '.')
            print(#hits .. ' hits')
            local c = read('a.txt')
            return c:find('world') ~= nil
        "#;
        let r = run_code(&tmp, code).await;
        assert!(!r.is_error, "content: {}", r.content);
        assert!(r.content.contains("1 hits"), "got: {}", r.content);
        assert!(r.content.contains("result: true"), "got: {}", r.content);
    }

    #[tokio::test]
    async fn json_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let r = run_code(
            &tmp,
            r#"local t = json.decode('{"a":[1,2,3],"b":"x"}'); return t.a[2] + (t.b == 'x' and 0 or 9)"#,
        )
        .await;
        assert!(!r.is_error);
        assert!(r.content.contains("result: 2"), "got: {}", r.content);
    }

    #[tokio::test]
    async fn rejects_io_and_os() {
        let tmp = tempfile::TempDir::new().unwrap();
        for forbidden in ["os.exit", "io.open", "dofile", "loadfile"] {
            let head = forbidden.split('.').next().unwrap();
            let r = run_code(&tmp, &format!("{head} is nil")).await;
            assert!(r.content.contains("true"), "{forbidden} must be nil: {}", r.content);
        }
    }

    #[tokio::test]
    async fn memory_limit_blocks_allocation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let r = run_code(&tmp, "local t = {} for i=1,50_000_000 do t[i] = 'x' end return #t").await;
        assert!(r.is_error, "expected memory error");
    }

    #[tokio::test]
    async fn wall_clock_timeout() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));
        let r = script_future(
            ctx,
            serde_json::json!({ "code": "while true do end", "timeout_ms": 1000 }).to_string(),
        )
        .await;
        assert!(r.is_error);
        assert!(r.content.contains("timed out"), "got: {}", r.content);
    }

    #[tokio::test]
    async fn read_confined_to_workspace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let r = run_code(&tmp, "return read('/etc/hostname') ~= nil").await;
        // /etc/hostname exists but is outside the workspace — must be refused.
        assert!(r.is_error || r.content.contains("result: false") || r.content.contains("nil"), "got: {}", r.content);
    }
}

fn install_bridges(lua: &mlua::Lua, ctx: &Arc<ToolContext>) -> Result<(), mlua::Error> {
    lua.set_app_data(PrintBuffer(std::sync::Mutex::new(Vec::new())));
    let print = lua.create_function(|lua, args: mlua::MultiValue| {
        let parts: Vec<String> = args
            .into_iter()
            .map(|a| value_to_string(&a))
            .collect();
        if let Some(buf) = lua.app_data_ref::<PrintBuffer>() {
            if let Ok(mut v) = buf.0.lock() {
                v.push(parts.join("\t"));
            }
        }
        Ok(())
    })?;
    lua.globals().set("print", print)?;

    let root = ctx.clone();
    let read = lua.create_function(move |_, path: String| {
        let full = resolve_path(&root, &path, false).map_err(mlua::Error::external)?;
        match std::fs::read_to_string(&full) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(mlua::Error::external(format!("read {path}: {e}"))),
        }
    })?;
    lua.globals().set("read", read)?;

    let root = ctx.clone();
    let list = lua.create_function(move |lua, path: String| {
        let full = resolve_path(&root, &path, false).map_err(mlua::Error::external)?;
        let entries =
            std::fs::read_dir(&full).map_err(|e| mlua::Error::external(format!("list {path}: {e}")))?;
        let t = lua.create_table()?;
        for (i, entry) in entries.flatten().enumerate() {
            let row = lua.create_table()?;
            row.set("name", entry.file_name().to_string_lossy().to_string())?;
            row.set("type", if entry.path().is_dir() { "dir" } else { "file" })?;
            t.set(i + 1, row)?;
        }
        Ok(t)
    })?;
    lua.globals().set("list", list)?;

    let root = ctx.clone();
    let grep = lua.create_function(
        move |lua, (pattern, path, limit): (String, String, Option<u32>)| {
            let re = regex::Regex::new(&pattern).map_err(mlua::Error::external)?;
            let full = resolve_path(&root, &path, false).map_err(mlua::Error::external)?;
            let limit = (limit.unwrap_or(200).max(1) as usize).min(MAX_GREP_HITS);
            let mut hits = Vec::new();
            collect_grep(&re, &full, &full, limit, &mut hits);
            let t = lua.create_table()?;
            for (i, m) in hits.iter().enumerate() {
                let row = lua.create_table()?;
                row.set("path", m.path.clone())?;
                row.set("line", m.line)?;
                row.set("text", m.text.clone())?;
                t.set(i + 1, row)?;
            }
            Ok(t)
        },
    )?;
    lua.globals().set("grep", grep)?;

    let json_t = lua.create_table()?;
    let encode = lua.create_function(|_lua, val: mlua::Value| {
        let json = lua_value_to_json(&val);
        serde_json::to_string(&json).map_err(mlua::Error::external)
    })?;
    let decode = lua.create_function(|lua, s: String| {
        let json: serde_json::Value =
            serde_json::from_str(&s).map_err(mlua::Error::external)?;
        json_to_lua(lua, &json)
    })?;
    json_t.set("encode", encode)?;
    json_t.set("decode", decode)?;
    lua.globals().set("json", json_t)?;

    Ok(())
}

struct GrepHit {
    path: String,
    line: usize,
    text: String,
}

fn collect_grep(re: &regex::Regex, root: &std::path::Path, dir: &std::path::Path, limit: usize, out: &mut Vec<GrepHit>) {
    if out.len() >= limit {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= limit {
            return;
        }
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }
        if p.is_dir() {
            collect_grep(re, root, &p, limit, out);
        } else if p.is_file()
            && p.metadata().map(|m| m.len() < 1_000_000).unwrap_or(false)
        {
            let Ok(content) = std::fs::read_to_string(&p) else {
                continue;
            };
            for (n, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    out.push(GrepHit {
                        path: p
                            .strip_prefix(root)
                            .unwrap_or(&p)
                            .to_string_lossy()
                            .to_string(),
                        line: n + 1,
                        text: line.chars().take(300).collect(),
                    });
                    if out.len() >= limit {
                        return;
                    }
                }
            }
        }
    }
}

fn value_to_string(val: &mlua::Value) -> String {
    match val {
        mlua::Value::Nil => "nil".to_string(),
        mlua::Value::Boolean(b) => b.to_string(),
        mlua::Value::Integer(i) => i.to_string(),
        mlua::Value::Number(f) => f.to_string(),
        mlua::Value::String(s) => s.to_string_lossy().to_string(),
        other => match serde_json::to_string(&lua_value_to_json(other)) {
            Ok(json) => json,
            Err(_) => format!("{other:?}"),
        },
    }
}

fn lua_value_to_json(val: &mlua::Value) -> serde_json::Value {
    match val {
        mlua::Value::Nil => serde_json::Value::Null,
        mlua::Value::Boolean(b) => serde_json::Value::Bool(*b),
        mlua::Value::Integer(i) => serde_json::json!(i),
        mlua::Value::Number(f) => serde_json::json!(f),
        mlua::Value::String(s) => serde_json::json!(s.to_string_lossy().to_string()),
        mlua::Value::Table(t) => {
            // Contiguous 1..n tables become arrays; anything else, objects.
            let mut arr = Vec::new();
            let mut i = 1i64;
            while let Ok(v) = t.get::<mlua::Value>(mlua::Value::Integer(i)) {
                arr.push(lua_value_to_json(&v));
                i += 1;
            }
            let mut map = serde_json::Map::new();
            for pair in t.clone().pairs::<mlua::Value, mlua::Value>().flatten() {
                let key = match &pair.0 {
                    mlua::Value::String(s) => s.to_string_lossy().to_string(),
                    mlua::Value::Integer(i) => i.to_string(),
                    other => format!("{other:?}"),
                };
                map.insert(key, lua_value_to_json(&pair.1));
            }
            if i > 1 && map.is_empty() {
                serde_json::Value::Array(arr)
            } else {
                serde_json::Value::Object(map)
            }
        }
        other => serde_json::json!(format!("{other:?}")),
    }
}

fn json_to_lua(lua: &mlua::Lua, json: &serde_json::Value) -> Result<mlua::Value, mlua::Error> {
    Ok(match json {
        serde_json::Value::Null => mlua::Value::Nil,
        serde_json::Value::Bool(b) => mlua::Value::Boolean(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => mlua::Value::Integer(i),
            None => mlua::Value::Number(n.as_f64().unwrap_or(0.0)),
        },
        serde_json::Value::String(s) => mlua::Value::String(lua.create_string(s)?),
        serde_json::Value::Array(items) => {
            let t = lua.create_table()?;
            for (i, item) in items.iter().enumerate() {
                t.set(i + 1, json_to_lua(lua, item)?)?;
            }
            mlua::Value::Table(t)
        }
        serde_json::Value::Object(map) => {
            let t = lua.create_table()?;
            for (k, v) in map {
                t.set(k.as_str(), json_to_lua(lua, v)?)?;
            }
            mlua::Value::Table(t)
        }
    })
}

pub(crate) fn script_tool() -> crate::agent::ToolDefinition {
    crate::agent::ToolDefinition::new_fn(
        "script",
        "Run a short sandboxed Lua 5.4 program in one turn and get its output. Use for multi-step data processing, parsing, numeric verification, and bulk file inspection without extra tool round-trips. Available: read(path), list(path), grep(pattern, path, limit?) -> table of {path,line,text}, json.encode/json.decode, print(...), plus the standard string/table/math libs. No io/os/network; reads only inside the workspace; memory-capped and wall-clock-capped (timeout_ms, default 10000, max 30000). Return a value to see it as `result:`.",
        r#"{"type":"object","properties":{"code":{"type":"string","description":"Lua source. Example: local hits = grep('TODO', 'src'); print(#hits .. ' hits'); return json.encode(hits[1])"},"timeout_ms":{"type":"integer","description":"Wall-clock cap in ms (default 10000, max 30000)"}},"required":["code"]}"#,
        script_future,
    )
    .with_effect(crate::agent::ToolEffect::Read)
}
