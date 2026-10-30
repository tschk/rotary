mod protocol;
pub mod worker;
use crate::agent::{ToolContext, ToolFuture, ToolResult};
use crate::tools::exec::{ExecRegistry, ScriptLease};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const DEFAULT_TIMEOUT_MS: u64 = 10000;
const MAX_TIMEOUT_MS: u64 = 30000;
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

struct CancelScript(Arc<ScriptLease>);
impl Drop for CancelScript {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn script_future(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let (code, timeout) = match script_args(&args) {
            Ok(value) => value,
            Err(error) => return ToolResult::err("script", error),
        };
        if code.len() > 65536 {
            return ToolResult::err("script", "code exceeds 64 KiB");
        }
        if ctx.cancellation.is_canceled() {
            return ToolResult::err("script", "script cancelled");
        }
        let registry = ctx.exec.clone().unwrap_or_else(|| {
            Arc::new(ExecRegistry::with_settlement_control(
                ctx.process_settlement.clone(),
            ))
        });
        let lease = ctx
            .script_lease
            .clone()
            .unwrap_or_else(|| registry.reserve_script_for(ctx.script_run_id.as_deref()));
        lease
            .started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let _cancel = CancelScript(lease.clone());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let thread_lease = lease.clone();
        let spawned = std::thread::Builder::new()
            .name("rx4-script-supervisor".into())
            .spawn(move || {
                let result = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(supervise(
                        ctx,
                        code,
                        timeout,
                        registry,
                        thread_lease.clone(),
                    )),
                    Err(error) => Err(error.to_string()),
                };
                thread_lease.finish();
                let _ = tx.send(result);
            });
        if let Err(error) = spawned {
            lease.finish();
            return ToolResult::err("script", error.to_string());
        }
        match rx.await {
            Ok(Ok(output)) => {
                ToolResult::ok("script", crate::secrets::Redactor::new().redact(&output))
            }
            Ok(Err(error)) => {
                ToolResult::err("script", crate::secrets::Redactor::new().redact(&error))
            }
            Err(error) => ToolResult::err("script", error.to_string()),
        }
    })
}

fn helper(ctx: &ToolContext) -> Result<PathBuf, String> {
    let path = match &ctx.script_worker {
        Some(path) => path.clone(),
        None => std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("executable directory missing")?
            .join("rx4-script-worker"),
    };
    if !path.is_absolute() || !path.is_file() {
        return Err(
            "trusted rx4-script-worker helper is missing; build/package the script helper".into(),
        );
    }
    Ok(path)
}

async fn supervise(
    ctx: Arc<ToolContext>,
    code: String,
    timeout: u64,
    registry: Arc<ExecRegistry>,
    lease: Arc<ScriptLease>,
) -> Result<String, String> {
    if ctx.cancellation.is_canceled() || lease.cancel.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("script cancelled".into());
    }
    let path = helper(&ctx)?;
    if ctx.os_sandbox_required {
        return Err("OS sandbox required for script helper".into());
    }
    let mut command = if let Some(sandbox) = &ctx.os_sandbox {
        sandbox
            .command(path.to_str().ok_or("invalid helper path")?, &[])
            .map_err(|e| e.to_string())?
    } else {
        std::process::Command::new(path)
    };
    command.env_clear().current_dir("/");
    if ctx.cancellation.is_canceled() || lease.cancel.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("script cancelled".into());
    }
    let process = registry.spawn_protocol(command)?;
    let id = process.id.clone();
    let done = process.done.clone();
    let deadline = Instant::now() + Duration::from_millis(timeout);
    let mut jobs = Vec::new();
    let cancellation = ctx.cancellation.clone();
    let result = tokio::select! {
        value=cancellation.run(exchange(ctx,code,timeout,process,&mut jobs,deadline,lease.clone()))=>value.unwrap_or_else(|_| Err("script cancelled".into())),
        _=lease.cancelled()=>Err("script cancelled".into()),
        _=tokio::time::sleep(Duration::from_millis(timeout))=>Err("script timed out (wall-clock cap)".into()),
    };
    let result = if Instant::now() >= deadline {
        Err("script timed out (wall-clock cap)".into())
    } else {
        result
    };
    // Pipes are closed when exchange drops. Keep every bridge job owned until it stops.
    loop {
        let retry = registry.settlement_control();
        let notice = retry.retry_notice();
        match registry.terminate_protocol(&id) {
            Ok(()) => break,
            Err(_) => notice.await,
        }
    }
    for job in jobs {
        let _ = job.await;
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    loop {
        let retry = registry.settlement_control();
        let notice = retry.retry_notice();
        match registry.finish_protocol(&id) {
            Ok(()) => break,
            Err(_) => notice.await,
        }
    }
    result
}

async fn read_async(reader: &mut (impl tokio::io::AsyncRead + Unpin)) -> Result<Value, String> {
    let size = reader.read_u32().await.map_err(|e| e.to_string())? as usize;
    if size > protocol::MAX_FRAME {
        return Err("script frame exceeds limit".into());
    }
    let mut bytes = vec![0; size];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
async fn write_async(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &Value,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > protocol::MAX_FRAME {
        return Err("script frame exceeds limit".into());
    }
    writer
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|e| e.to_string())?;
    writer.write_all(&bytes).await.map_err(|e| e.to_string())?;
    writer.flush().await.map_err(|e| e.to_string())
}
async fn exchange(
    ctx: Arc<ToolContext>,
    code: String,
    timeout: u64,
    process: crate::tools::exec::ProtocolProcess,
    jobs: &mut Vec<tokio::task::JoinHandle<Result<Value, String>>>,
    deadline: Instant,
    lease: Arc<ScriptLease>,
) -> Result<String, String> {
    let mut stdin =
        tokio::process::ChildStdin::from_std(process.stdin).map_err(|e| e.to_string())?;
    let mut stdout =
        tokio::process::ChildStdout::from_std(process.stdout).map_err(|e| e.to_string())?;
    let hello = read_async(&mut stdout).await?;
    if hello["version"] != 1 || hello["kind"] != "hello" {
        return Err("script helper protocol/version mismatch".into());
    }
    write_async(
        &mut stdin,
        &json!({"version":1,"kind":"start","code":code,"timeout_ms":timeout}),
    )
    .await?;
    let mut requests = 0;
    loop {
        let frame = read_async(&mut stdout).await?;
        if frame["version"] != 1 {
            return Err("script protocol/version mismatch".into());
        }
        match frame["kind"].as_str() {
            Some("result") => {
                if let Some(error) = frame["error"].as_str() {
                    if error.len() > 32768 {
                        return Err("script error exceeds limit".into());
                    }
                    return Err(error.into());
                }
                let output = frame["output"].as_str().ok_or("script output missing")?;
                if output.len() > 32768 {
                    return Err("script output exceeds limit".into());
                }
                return Ok(output.into());
            }
            Some("bridge") => {
                requests += 1;
                if frame["id"].as_u64() != Some(requests) {
                    return Err("script bridge request identity mismatch".into());
                }
                if requests > 4096 {
                    return Err("script bridge request limit exceeded".into());
                }
                let request_ctx = ctx.clone();
                let authorization_frame = frame.clone();
                jobs.push(tokio::task::spawn_blocking(move || {
                    authorize_bridge(&request_ctx, &authorization_frame)
                }));
                let completed = jobs.last_mut().expect("owned authorization").await;
                jobs.pop();
                let authorization = completed.map_err(|e| e.to_string())?;
                let allowed = match authorization {
                    Ok(Value::Bool(true)) => Ok(()),
                    Ok(ask) => {
                        let call = crate::agent::ToolCall {
                            id: format!("script-{}-{requests}", process.id),
                            name: ask["tool"].as_str().unwrap_or("").into(),
                            arguments: ask["arguments"].as_str().unwrap_or("").into(),
                        };
                        match ctx
                            .subagent_authority
                            .as_ref()
                            .and_then(|authority| authority.async_approver.clone())
                        {
                            Some(approver)
                                if approver.approve(&call).await
                                    == crate::permissions::Decision::Allow =>
                            {
                                Ok(())
                            }
                            _ => Err("script bridge approval denied".into()),
                        }
                    }
                    Err(error) => Err(error),
                };
                let result = if let Err(error) = allowed {
                    Err(error)
                } else {
                    let request_ctx = ctx.clone();
                    let request_lease = lease.clone();
                    jobs.push(tokio::task::spawn_blocking(move || {
                        bridge(&request_ctx, &frame, deadline, &request_lease)
                    }));
                    let completed = jobs.last_mut().expect("owned bridge").await;
                    jobs.pop();
                    completed.map_err(|e| e.to_string())?
                };
                let response = match result {
                    Ok(mut value) => {
                        redact_bridge(&mut value);
                        json!({"version":1,"kind":"bridge_result","id":requests,"value":value})
                    }
                    Err(error) => {
                        json!({"version":1,"kind":"bridge_result","id":requests,"error":crate::secrets::Redactor::new().redact(&error)})
                    }
                };
                write_async(&mut stdin, &response).await?;
            }
            _ => return Err("invalid script protocol operation".into()),
        }
    }
}

fn redact_bridge(value: &mut Value) {
    match value {
        Value::String(text) => *text = crate::secrets::Redactor::new().redact(text),
        Value::Array(values) => {
            for value in values {
                redact_bridge(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_bridge(value);
            }
        }
        _ => {}
    }
}

fn authorize_bridge(ctx: &ToolContext, frame: &Value) -> Result<Value, String> {
    let Some(authority) = &ctx.subagent_authority else {
        return Ok(Value::Bool(true));
    };
    let args = frame["args"]
        .as_array()
        .ok_or("bridge arguments required")?;
    let (name, arguments) = match frame["op"].as_str() {
        Some("read") => ("read", json!({"path":args.first().ok_or("path required")?})),
        Some("list") => ("ls", json!({"path":args.first().ok_or("path required")?})),
        Some("grep") => (
            "grep",
            json!({"pattern":args.first().ok_or("pattern required")?,"path":args.get(1).ok_or("path required")?}),
        ),
        _ => return Err("unsupported bridge operation".into()),
    };
    if authority
        .scope
        .is_some_and(|scope| !crate::mode::tool_allowed(&crate::mode::profile(scope), name))
    {
        return Err("script bridge denied by scope".into());
    }
    let arguments = arguments.to_string();
    let decision = if let Some(authorizer) = &authority.authorizer {
        authorizer.authorize(
            &authority.policy,
            name,
            &arguments,
            authority.approver.as_deref(),
            Some(&ctx.workspace_root),
        )
    } else {
        crate::permissions::authorize_with_workspace(
            &authority.policy,
            name,
            &arguments,
            authority.approver.as_deref(),
            Some(&ctx.workspace_root),
        )
    };
    match decision {
        crate::permissions::Decision::Allow => Ok(Value::Bool(true)),
        crate::permissions::Decision::Deny => Err("script bridge denied by parent policy".into()),
        crate::permissions::Decision::Ask => Ok(json!({"tool":name,"arguments":arguments})),
    }
}

fn bridge(
    ctx: &ToolContext,
    frame: &Value,
    deadline: Instant,
    lease: &ScriptLease,
) -> Result<Value, String> {
    let check = || {
        if Instant::now() >= deadline {
            Err("script timed out (wall-clock cap)".to_string())
        } else if lease.cancel.load(std::sync::atomic::Ordering::SeqCst)
            || ctx.cancellation.is_canceled()
        {
            Err("script cancelled".into())
        } else {
            Ok(())
        }
    };
    check()?;
    let args = frame["args"].as_array().ok_or("bridge arguments missing")?;
    let text = |i: usize| -> Result<&str, String> {
        let text = args
            .get(i)
            .and_then(Value::as_str)
            .ok_or("bridge string argument required")?;
        if text.len() > 4096 {
            Err("bridge argument exceeds limit".into())
        } else {
            Ok(text)
        }
    };
    let path = |value: &str| -> Result<PathBuf, String> {
        let full = super::common::resolve_path(ctx, value, false)?;
        let root = ctx
            .workspace_root
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !full.starts_with(root) {
            return Err("path outside workspace".into());
        }
        Ok(full)
    };
    match frame["op"].as_str() {
        Some("read") => {
            let file = path(text(0)?)?;
            match std::fs::metadata(&file) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Value::Null)
                }
                Err(error) => return Err(error.to_string()),
                Ok(meta) if !meta.is_file() => return Err("read requires a regular file".into()),
                Ok(_) => {}
            }
            use std::io::Read;
            let mut value = String::new();
            std::fs::File::open(file)
                .map_err(|e| e.to_string())?
                .take(1048577)
                .read_to_string(&mut value)
                .map_err(|e| e.to_string())?;
            check()?;
            if value.len() > 1048576 {
                return Err("bridge read exceeds 1 MiB".into());
            }
            Ok(json!(value))
        }
        Some("list") => {
            let mut rows = Vec::new();
            for entry in std::fs::read_dir(path(text(0)?)?).map_err(|e| e.to_string())? {
                check()?;
                let entry = entry.map_err(|e| e.to_string())?;
                if rows.len() >= 4096 {
                    return Err("list exceeds limit".into());
                }
                rows.push(json!({"name":entry.file_name().to_string_lossy(),"type":if entry.file_type().map_err(|e|e.to_string())?.is_dir(){"dir"}else{"file"}}));
            }
            Ok(json!(rows))
        }
        Some("grep") => {
            let pattern = regex::Regex::new(text(0)?).map_err(|e| e.to_string())?;
            let root = path(text(1)?)?;
            let mut stack = vec![root.clone()];
            let mut hits = Vec::new();
            let limit = args
                .get(2)
                .and_then(Value::as_u64)
                .unwrap_or(200)
                .clamp(1, 500) as usize;
            while let Some(dir) = stack.pop() {
                check()?;
                for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
                    check()?;
                    let entry = entry.map_err(|e| e.to_string())?;
                    let kind = entry.file_type().map_err(|e| e.to_string())?;
                    let name = entry.file_name().to_string_lossy().to_string();
                    if kind.is_symlink()
                        || name.starts_with('.')
                        || name == "target"
                        || name == "node_modules"
                    {
                        continue;
                    }
                    let Ok(full) = path(&entry.path().to_string_lossy()) else {
                        continue;
                    };
                    if kind.is_dir() {
                        stack.push(full);
                        continue;
                    }
                    if !kind.is_file()
                        || entry.metadata().map_err(|e| e.to_string())?.len() > 1000000
                    {
                        continue;
                    }
                    let content = std::fs::read_to_string(&full).map_err(|e| e.to_string())?;
                    for (i, line) in content.lines().enumerate() {
                        check()?;
                        if pattern.is_match(line) {
                            hits.push(json!({"path":full.strip_prefix(&root).unwrap_or(&full).to_string_lossy(),"line":i+1,"text":line.chars().take(300).collect::<String>()}));
                            if hits.len() >= limit {
                                return Ok(json!(hits));
                            }
                        }
                    }
                }
            }
            Ok(json!(hits))
        }
        _ => Err("unsupported script bridge operation".into()),
    }
}
pub(crate) fn script_tool() -> crate::agent::ToolDefinition {
    crate::agent::ToolDefinition::new_fn(
        "script",
        "Run a short sandboxed LuaJIT (Lua 5.1) program in an isolated process and get its output. Requires the trusted rx4-script-worker helper packaged/configured by the host. Use for multi-step data processing, parsing, numeric verification, and bulk file inspection without extra tool round-trips. Available: read(path), list(path), grep(pattern, path, limit?) -> table of {path,line,text}, json.encode/json.decode, print(...), plus the string/table/math/bit libs. No io/os/network/ffi and no loader (dofile/loadfile/require are removed); reads only inside the workspace; wall-clock-capped (timeout_ms, default 10000, max 30000) with no memory limit. Code is limited to 64 KiB, output to 32 KiB, and each bridge read to 1 MiB; exceeding a limit returns an error. Return a value to see it as `result:`.",
        r#"{"type":"object","properties":{"code":{"type":"string","description":"Lua source. Example: local hits = grep('TODO', 'src'); print(#hits .. ' hits'); return json.encode(hits[1])"},"timeout_ms":{"type":"integer","description":"Wall-clock cap in ms (default 10000, max 30000)"}},"required":["code"]}"#,
        script_future,
    )
    .with_effect(crate::agent::ToolEffect::Read)
}
