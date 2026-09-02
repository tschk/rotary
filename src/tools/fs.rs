use super::common::{parse_num_field, parse_str_field, resolve_path};
use crate::agent::{ToolContext, ToolFuture, ToolResult};
use std::sync::Arc;
use tracing::debug;

#[cfg(feature = "builtin-tools")]
use std::time::Duration;

#[cfg(feature = "builtin-tools")]
use std::process::Stdio;
#[cfg(feature = "builtin-tools")]
use tokio::io::AsyncReadExt;
#[cfg(feature = "builtin-tools")]
use tokio::process::Command;

fn hashline_display_path(ctx: &ToolContext, full: &std::path::Path, requested: &str) -> String {
    if let Ok(rel) = full.strip_prefix(&ctx.workspace_root) {
        return rel.to_string_lossy().trim_start_matches('/').to_string();
    }
    if let Ok(root) = ctx.workspace_root.canonicalize() {
        if let Ok(rel) = full.strip_prefix(root) {
            return rel.to_string_lossy().trim_start_matches('/').to_string();
        }
    }
    requested.trim_start_matches("./").to_string()
}

pub(crate) fn exec_read(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let path = match parse_str_field(&args, "path") {
            Some(p) => p,
            None => return ToolResult::err("read", "path required"),
        };
        let offset = parse_num_field(&args, "offset").unwrap_or(0) as usize;
        let limit = parse_num_field(&args, "limit").unwrap_or(2000) as usize;
        let hashline = serde_json::from_str::<serde_json::Value>(&args)
            .ok()
            .and_then(|v| v.get("hashline").and_then(|x| x.as_bool()))
            .unwrap_or(false);

        let full = match resolve_path(&ctx, &path, false) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("read", e),
        };
        match tokio::fs::read_to_string(&full).await {
            Ok(content) => {
                if let Some(versions) = &ctx.versions {
                    versions.write().observe(&full, content.as_bytes());
                }
                if hashline {
                    // Honor `offset` as a 0-based start line; `limit` still
                    // caps visible lines (head+tail never exceed max_visible).
                    let display = hashline_display_path(&ctx, &full, &path);
                    let tagged = crate::hashline::format_read(
                        &display,
                        &content,
                        crate::hashline::ReadOptions::from_limit(limit).with_offset(offset),
                    );
                    {
                        let mut sight = ctx.hashline_sight.write();
                        sight.remember(&display, tagged.tag.clone(), tagged.visible.clone());
                        sight.remember(&path, tagged.tag.clone(), tagged.visible.clone());
                    }
                    return ToolResult::ok("read", tagged.text);
                }
                let lines: Vec<&str> = content.lines().collect();
                let start = offset.min(lines.len());
                let end = (start + limit).min(lines.len());
                let mut out = String::new();
                for (i, line) in lines[start..end].iter().enumerate() {
                    out.push_str(&format!("{:>6}\t{}\n", start + i + 1, line));
                }
                if out.is_empty() {
                    out = "(empty file)".to_string();
                }
                ToolResult::ok("read", out)
            }
            Err(e) => ToolResult::err("read", format!("{e}")),
        }
    })
}

pub(crate) fn exec_write(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let path = match parse_str_field(&args, "path") {
            Some(p) => p,
            None => return ToolResult::err("write", "path required"),
        };
        let content = match parse_str_field(&args, "content") {
            Some(c) => c,
            None => return ToolResult::err("write", "content required"),
        };
        let full = match resolve_path(&ctx, &path, true) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("write", e),
        };
        if let Some(parent) = full.parent() {
            if !tokio::fs::try_exists(parent).await.unwrap_or(false) {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return ToolResult::err("write", format!("mkdir failed: {e}"));
                }
            }
        }
        if let Some(claim) = &ctx.worktree_claim {
            if !claim.allows(&full) {
                return ToolResult::err("write", "path outside claimed worktree");
            }
        }
        if let Some(versions) = &ctx.versions {
            if let Err(e) = versions.read().check(&full) {
                return ToolResult::err("write", e);
            }
        }
        if let Some(store) = &ctx.snapshots {
            store.write().snapshot_file(&full);
        }
        match tokio::fs::write(&full, &content).await {
            Ok(_) => {
                debug!("wrote {} bytes to {}", content.len(), full.display());
                ToolResult::ok(
                    "write",
                    format!("wrote {} bytes to {}", content.len(), path),
                )
            }
            Err(e) => ToolResult::err("write", format!("{e}")),
        }
    })
}

pub(crate) fn exec_edit(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let path = match parse_str_field(&args, "path") {
            Some(p) => p,
            None => return ToolResult::err("edit", "path required"),
        };
        let old_string = match parse_str_field(&args, "old_string") {
            Some(s) => s,
            None => return ToolResult::err("edit", "old_string required"),
        };
        let new_string = match parse_str_field(&args, "new_string") {
            Some(s) => s,
            None => return ToolResult::err("edit", "new_string required"),
        };
        let full = match resolve_path(&ctx, &path, true) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("edit", e),
        };
        if let Some(claim) = &ctx.worktree_claim {
            if !claim.allows(&full) {
                return ToolResult::err("edit", "path outside claimed worktree");
            }
        }
        if let Some(versions) = &ctx.versions {
            if let Err(e) = versions.read().check(&full) {
                return ToolResult::err("edit", e);
            }
        }
        if let Some(store) = &ctx.snapshots {
            store.write().snapshot_file(&full);
        }
        let content = match tokio::fs::read_to_string(&full).await {
            Ok(c) => c,
            Err(e) => return ToolResult::err("edit", format!("read failed: {e}")),
        };
        let occurrences = content.matches(&old_string).count();
        if occurrences == 0 {
            return ToolResult::err("edit", "old_string not found in file");
        }
        if occurrences > 1 {
            return ToolResult::err(
                "edit",
                format!("old_string found {occurrences} times — must be unique"),
            );
        }
        let new_content = content.replacen(&old_string, &new_string, 1);
        match tokio::fs::write(&full, &new_content).await {
            Ok(_) => ToolResult::ok("edit", format!("edited {}", path)),
            Err(e) => ToolResult::err("edit", format!("write failed: {e}")),
        }
    })
}

pub(crate) fn exec_hashline_edit(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let path = match parse_str_field(&args, "path") {
            Some(p) => p,
            None => return ToolResult::err("hashline_edit", "path required"),
        };
        let tag = match parse_str_field(&args, "tag") {
            Some(t) => t,
            None => return ToolResult::err("hashline_edit", "tag required"),
        };
        let script = match parse_str_field(&args, "script") {
            Some(s) => s,
            None => return ToolResult::err("hashline_edit", "script required"),
        };
        let family = match parse_str_field(&args, "family").as_deref() {
            Some("sloppy") => crate::hashline::ModelFamily::Sloppy,
            _ => crate::hashline::ModelFamily::Strict,
        };
        let full = match resolve_path(&ctx, &path, true) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("hashline_edit", e),
        };
        if let Some(claim) = &ctx.worktree_claim {
            if !claim.allows(&full) {
                return ToolResult::err("hashline_edit", "path outside claimed worktree");
            }
        }
        if let Some(versions) = &ctx.versions {
            if let Err(e) = versions.read().check(&full) {
                return ToolResult::err("hashline_edit", e);
            }
        }
        if let Some(store) = &ctx.snapshots {
            store.write().snapshot_file(&full);
        }
        let content = match tokio::fs::read_to_string(&full).await {
            Ok(c) => c,
            Err(e) => return ToolResult::err("hashline_edit", format!("read failed: {e}")),
        };
        let display = hashline_display_path(&ctx, &full, &path);
        let visible = ctx
            .hashline_sight
            .read()
            .visible_for_any([&path, &display], &tag);
        match crate::hashline::apply(&content, &tag, &script, &visible, family) {
            Ok(next) => {
                if let Some(log) = &ctx.hunk_log {
                    log.write().record(crate::hashline::HunkCheckpoint {
                        path: path.clone(),
                        before: content.clone(),
                        after: next.clone(),
                        tag: tag.clone(),
                    });
                }
                match tokio::fs::write(&full, &next).await {
                    Ok(_) => {
                        ctx.hashline_sight.write().forget(&path);
                        let display = full
                            .strip_prefix(&ctx.workspace_root)
                            .unwrap_or(full.as_path());
                        let display = display.to_string_lossy();
                        ctx.hashline_sight
                            .write()
                            .forget(display.trim_start_matches('/'));
                        ToolResult::ok("hashline_edit", format!("edited {}", path))
                    }
                    Err(e) => ToolResult::err("hashline_edit", format!("write failed: {e}")),
                }
            }
            Err(e) => ToolResult::err("hashline_edit", e.to_string()),
        }
    })
}

#[cfg(feature = "builtin-tools")]
fn resolve_working_dir(
    ctx: &Arc<ToolContext>,
    cwd: Option<String>,
) -> Result<std::path::PathBuf, String> {
    if let Some(cwd) = cwd {
        resolve_path(ctx, &cwd, false)
    } else if let Some(sb) = ctx.sandbox.as_ref() {
        if let Err(e) = sb.validate_path(&ctx.workspace_root, false) {
            return Err(e.to_string());
        }
        Ok(ctx.workspace_root.clone())
    } else {
        Ok(ctx.workspace_root.clone())
    }
}

#[cfg(feature = "builtin-tools")]
fn build_command(
    ctx: &Arc<ToolContext>,
    command: &str,
    working_dir: &std::path::Path,
) -> Result<Command, String> {
    if let Some(os) = ctx.os_sandbox.as_ref() {
        // Wrap bash -c under seatbelt/bwrap; convert std Command → tokio.
        match os.command("bash", &["-c", command]) {
            Ok(mut c) => {
                c.current_dir(working_dir);
                c.stdout(Stdio::piped()).stderr(Stdio::piped());
                let mut tc = Command::from(c);
                tc.kill_on_drop(true);
                Ok(tc)
            }
            Err(e) => Err(e.to_string()),
        }
    } else if cfg!(target_os = "windows") {
        // SECURITY: The `bash` tool is explicitly designed to execute arbitrary shell commands
        // from the LLM. Command injection via operators is an intended feature.
        // The LLM is instructed in the tool definition to not pass unsanitized external input.
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c.current_dir(working_dir);
        c.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Ok(c)
    } else {
        // SECURITY: The `bash` tool is explicitly designed to execute arbitrary shell commands
        // from the LLM. Command injection via operators (&, |, ;) is an intended feature.
        // The LLM is instructed in the tool definition to not pass unsanitized external input.
        let mut c = Command::new("bash");
        c.arg("-c").arg(command);
        c.current_dir(working_dir);
        c.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Ok(c)
    }
}

#[cfg(feature = "builtin-tools")]
async fn wait_and_drain(
    ctx: Arc<ToolContext>,
    mut child: tokio::process::Child,
    timeout: Duration,
) -> Result<(Vec<u8>, Vec<u8>, i32), String> {
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    let drain = async {
        let stdout_task = async {
            let mut buf = Vec::new();
            if let Some(mut out) = stdout_pipe.take() {
                let _ = out.read_to_end(&mut buf).await;
            }
            buf
        };
        let stderr_task = async {
            let mut buf = Vec::new();
            if let Some(mut err) = stderr_pipe.take() {
                let _ = err.read_to_end(&mut buf).await;
            }
            buf
        };
        let wait_task = async {
            loop {
                if ctx.cancellation.is_canceled() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Err("command cancelled".to_string());
                }
                match child.try_wait() {
                    Ok(Some(status)) => return Ok(status.code().unwrap_or(-1)),
                    Ok(None) => tokio::time::sleep(Duration::from_millis(10)).await,
                    Err(e) => return Err(format!("wait failed: {e}")),
                }
            }
        };
        // Drain pipes concurrent with wait — avoid pipe-buffer deadlock.
        let (stdout_buf, stderr_buf, wait_res) = tokio::join!(stdout_task, stderr_task, wait_task);
        let exit_code = wait_res?;
        // If process still has leftover status after pipes closed:
        let exit_code = if exit_code == -1 {
            child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1)
        } else {
            exit_code
        };
        Ok::<_, String>((stdout_buf, stderr_buf, exit_code))
    };

    match tokio::time::timeout(timeout, drain).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(msg)) => Err(msg),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(format!("command timed out after {}s", timeout.as_secs()))
        }
    }
}

#[cfg(feature = "builtin-tools")]
fn format_output(stdout_buf: Vec<u8>, stderr_buf: Vec<u8>, exit_code: i32) -> String {
    let stdout = String::from_utf8_lossy(&stdout_buf).to_string();
    let stderr = String::from_utf8_lossy(&stderr_buf).to_string();

    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push_str("\n--- stderr ---\n");
        }
        result.push_str(&stderr);
    }
    if exit_code != 0 {
        result.push_str(&format!("\n(exit code: {exit_code})"));
    }
    if result.is_empty() {
        result = "(no output)".to_string();
    }
    result
}

#[cfg(feature = "builtin-tools")]
pub(crate) fn is_sandbox_signal_flake(stdout: &[u8], stderr: &[u8], exit_code: i32) -> bool {
    // Rust maps signaled children to None → we store -1. sandbox-exec
    // SIGABRT/SIGKILL also show up as 134/137 when the wrapper exits.
    stdout.is_empty() && stderr.is_empty() && matches!(exit_code, -1 | 134 | 137)
}

#[cfg(not(feature = "builtin-tools"))]
pub(crate) fn exec_bash(_ctx: Arc<ToolContext>, _args: String) -> ToolFuture {
    Box::pin(async move { ToolResult::err("bash", "builtin-tools feature not enabled") })
}

#[cfg(feature = "builtin-tools")]
pub(crate) fn exec_bash(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let command = match parse_str_field(&args, "command") {
            Some(c) => c,
            None => return ToolResult::err("bash", "command required"),
        };
        let cwd = parse_str_field(&args, "cwd");
        let timeout_secs = parse_num_field(&args, "timeout").unwrap_or(120);

        // Fail closed: policy requires OS sandbox but runner unavailable.
        if ctx.os_sandbox_required && ctx.os_sandbox.is_none() {
            return ToolResult::err(
                "bash",
                "OS sandbox required but unavailable — shell execution blocked",
            );
        }

        if let Some(sb) = ctx.sandbox.as_ref() {
            if let Err(e) = sb.validate_command(&command) {
                return ToolResult::err("bash", e.to_string());
            }
        }

        let working_dir = match resolve_working_dir(&ctx, cwd) {
            Ok(dir) => dir,
            Err(e) => return ToolResult::err("bash", e),
        };

        let timeout = Duration::from_secs(timeout_secs);
        // Seatbelt flake: empty + signal death (-1 / 134 ABRT / 137 KILL).
        // Retry once; still dead → fail-closed (do not return ok + exit -1).
        let mut last_flake = None;
        for attempt in 0..2 {
            let mut cmd = match build_command(&ctx, &command, &working_dir) {
                Ok(c) => c,
                Err(e) => return ToolResult::err("bash", e),
            };
            let child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => return ToolResult::err("bash", format!("failed to execute: {e}")),
            };
            let (stdout_buf, stderr_buf, exit_code) =
                match wait_and_drain(ctx.clone(), child, timeout).await {
                    Ok(res) => res,
                    Err(msg) => return ToolResult::err("bash", msg),
                };
            if ctx.os_sandbox.is_some()
                && is_sandbox_signal_flake(&stdout_buf, &stderr_buf, exit_code)
            {
                last_flake = Some(exit_code);
                if attempt == 0 {
                    continue;
                }
                return ToolResult::err(
                    "bash",
                    format!(
                        "OS sandbox aborted the process (exit {exit_code}, no output); refuse fail-open"
                    ),
                );
            }
            let result = format_output(stdout_buf, stderr_buf, exit_code);
            return ToolResult::ok("bash", result);
        }
        ToolResult::err(
            "bash",
            format!(
                "OS sandbox aborted the process (exit {}, no output); refuse fail-open",
                last_flake.unwrap_or(-1)
            ),
        )
    })
}

pub(crate) fn exec_grep(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let pattern = match parse_str_field(&args, "pattern") {
            Some(p) => p,
            None => return ToolResult::err("grep", "pattern required"),
        };
        let path = parse_str_field(&args, "path").unwrap_or_else(|| ".".to_string());
        let context = parse_num_field(&args, "context").unwrap_or(0) as usize;

        let full = match resolve_path(&ctx, &path, false) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("grep", e),
        };

        #[cfg(all(feature = "builtin-tools", feature = "fff"))]
        {
            let workspace_root = ctx.workspace_root.clone();
            let result = tokio::task::spawn_blocking(move || {
                let root = if full.is_file() {
                    full.parent().unwrap_or(&workspace_root).to_path_buf()
                } else {
                    full
                };
                let shared = crate::search::picker_for(root)?;
                let guard = shared.read().map_err(|e| e.to_string())?;
                let picker = guard.as_ref().ok_or("picker missing")?;
                let query = fff_search::parse_grep_query(&pattern);
                let options = fff_search::GrepSearchOptions {
                    before_context: context,
                    after_context: context,
                    page_limit: 100,
                    ..Default::default()
                };
                let grep_result = picker.grep(&query, &options);

                let mut out = String::new();
                for m in &grep_result.matches {
                    let file = &grep_result.files[m.file_index];
                    let path = file.absolute_path(picker, &picker.base_path);
                    let path_str = path.to_string_lossy();
                    for (i, line) in m.context_before.iter().enumerate() {
                        let num = m.line_number as usize - m.context_before.len() + i;
                        out.push_str(&format!("  {num:>6}\t{path_str}\t{line}\n"));
                    }
                    out.push_str(&format!(
                        "> {line_number:>6}\t{path_str}\t{line_content}\n",
                        line_number = m.line_number,
                        line_content = m.line_content
                    ));
                    for (i, line) in m.context_after.iter().enumerate() {
                        let num = m.line_number as usize + 1 + i;
                        out.push_str(&format!("  {num:>6}\t{path_str}\t{line}\n"));
                    }
                    if context > 0 && !m.context_after.is_empty() {
                        out.push_str("  ---\n");
                    }
                }
                Ok::<_, String>(if out.is_empty() {
                    "(no matches)".to_string()
                } else {
                    out
                })
            })
            .await
            .unwrap_or_else(|e| Err(format!("search task failed: {e}")));

            match result {
                Ok(content) => ToolResult::ok("grep", content),
                Err(e) => ToolResult::err("grep", e),
            }
        }

        #[cfg(all(feature = "builtin-tools", not(feature = "fff")))]
        {
            let workspace_root = ctx.workspace_root.clone();
            let result = tokio::task::spawn_blocking(move || {
                let root = if full.is_file() {
                    full.parent().unwrap_or(&workspace_root).to_path_buf()
                } else {
                    full
                };
                stdlib_grep(&root, &pattern, context)
            })
            .await
            .unwrap_or_else(|e| Err(format!("search task failed: {e}")));

            match result {
                Ok(content) => ToolResult::ok("grep", content),
                Err(e) => ToolResult::err("grep", e),
            }
        }

        #[cfg(not(feature = "builtin-tools"))]
        {
            let _ = (pattern, context, full);
            ToolResult::err("grep", "builtin-tools feature not enabled")
        }
    })
}

pub(crate) fn exec_find(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let pattern = match parse_str_field(&args, "pattern") {
            Some(p) => p,
            None => return ToolResult::err("find", "pattern required"),
        };
        let path = parse_str_field(&args, "path").unwrap_or_else(|| ".".to_string());
        let full = match resolve_path(&ctx, &path, false) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("find", e),
        };

        #[cfg(all(feature = "builtin-tools", feature = "fff"))]
        {
            let result = tokio::task::spawn_blocking(move || {
                let shared = crate::search::picker_for(full)?;
                let guard = shared.read().map_err(|e| e.to_string())?;
                let picker = guard.as_ref().ok_or("picker missing")?;
                let parser = fff_search::QueryParser::<fff_search::FileSearchConfig>::default();
                let query = parser.parse(&pattern);
                let options = fff_search::FuzzySearchOptions {
                    max_threads: 0,
                    pagination: fff_search::PaginationArgs {
                        offset: 0,
                        limit: 100,
                    },
                    ..Default::default()
                };
                let search_result = picker.fuzzy_search(&query, None, options);

                let mut out = Vec::new();
                for item in search_result.items {
                    let path = item.absolute_path(picker, &picker.base_path);
                    out.push(path.to_string_lossy().into_owned());
                }
                Ok::<_, String>(if out.is_empty() {
                    "(no files found)".to_string()
                } else {
                    out.join("\n")
                })
            })
            .await
            .unwrap_or_else(|e| Err(format!("search task failed: {e}")));

            match result {
                Ok(content) => ToolResult::ok("find", content),
                Err(e) => ToolResult::err("find", e),
            }
        }

        #[cfg(all(feature = "builtin-tools", not(feature = "fff")))]
        {
            let result = tokio::task::spawn_blocking(move || stdlib_find(&full, &pattern))
                .await
                .unwrap_or_else(|e| Err(format!("search task failed: {e}")));

            match result {
                Ok(content) => ToolResult::ok("find", content),
                Err(e) => ToolResult::err("find", e),
            }
        }

        #[cfg(not(feature = "builtin-tools"))]
        {
            let _ = (pattern, full);
            ToolResult::err("find", "builtin-tools feature not enabled")
        }
    })
}

pub(crate) fn exec_ls(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let path = match parse_str_field(&args, "path") {
            Some(p) => p,
            None => return ToolResult::err("ls", "path required"),
        };
        let full = match resolve_path(&ctx, &path, false) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("ls", e),
        };
        match tokio::fs::read_dir(&full).await {
            Ok(mut entries) => {
                let mut items: Vec<(String, bool)> = Vec::new();
                loop {
                    match entries.next_entry().await {
                        Ok(Some(e)) => {
                            let name = e.file_name().to_string_lossy().to_string();
                            if let Ok(file_type) = e.file_type().await {
                                items.push((name, file_type.is_dir()));
                            }
                        }
                        Ok(None) => break,
                        Err(_) => continue,
                    }
                }
                items.sort_by(|a, b| a.0.cmp(&b.0));
                let out: Vec<String> = items
                    .iter()
                    .map(|(name, is_dir)| {
                        if *is_dir {
                            format!("{name}/")
                        } else {
                            name.clone()
                        }
                    })
                    .collect();
                if out.is_empty() {
                    ToolResult::ok("ls", "(empty directory)")
                } else {
                    ToolResult::ok("ls", out.join("\n"))
                }
            }
            Err(e) => ToolResult::err("ls", format!("{e}")),
        }
    })
}

#[cfg(all(feature = "builtin-tools", not(feature = "fff")))]
fn skip_dir_name(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | ".hg" | ".svn" | "dist" | "build" | ".venv"
    )
}

#[cfg(all(feature = "builtin-tools", not(feature = "fff")))]
fn walk_files(root: &std::path::Path, out: &mut Vec<std::path::PathBuf>, max: usize) {
    if out.len() >= max {
        return;
    }
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if out.len() >= max {
            return;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            if skip_dir_name(&name) {
                continue;
            }
            walk_files(&path, out, max);
        } else if ft.is_file() {
            out.push(path);
        }
    }
}

#[cfg(all(feature = "builtin-tools", not(feature = "fff")))]
fn stdlib_grep(root: &std::path::Path, pattern: &str, context: usize) -> Result<String, String> {
    let re = regex::Regex::new(pattern).map_err(|e| format!("invalid pattern: {e}"))?;
    let mut files = Vec::new();
    if root.is_file() {
        files.push(root.to_path_buf());
    } else {
        walk_files(root, &mut files, 2_000);
    }
    let mut out = String::new();
    let mut matches = 0usize;
    for path in files {
        if matches >= 100 {
            break;
        }
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() > 1_048_576 {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if bytes.contains(&0) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        let path_str = path.to_string_lossy();
        for (idx, line) in lines.iter().enumerate() {
            if matches >= 100 {
                break;
            }
            if !re.is_match(line) {
                continue;
            }
            let line_number = idx + 1;
            if context > 0 {
                let start = idx.saturating_sub(context);
                for (i, ctx_line) in lines[start..idx].iter().enumerate() {
                    let num = start + i + 1;
                    out.push_str(&format!("  {num:>6}\t{path_str}\t{ctx_line}\n"));
                }
            }
            out.push_str(&format!("> {line_number:>6}\t{path_str}\t{line}\n"));
            if context > 0 {
                let end = (idx + 1 + context).min(lines.len());
                for (i, ctx_line) in lines[idx + 1..end].iter().enumerate() {
                    let num = line_number + 1 + i;
                    out.push_str(&format!("  {num:>6}\t{path_str}\t{ctx_line}\n"));
                }
                if end > idx + 1 {
                    out.push_str("  ---\n");
                }
            }
            matches += 1;
        }
    }
    Ok(if out.is_empty() {
        "(no matches)".to_string()
    } else {
        out
    })
}

#[cfg(all(feature = "builtin-tools", not(feature = "fff")))]
fn glob_to_regex(pattern: &str) -> Result<regex::Regex, String> {
    let mut escaped = String::from("(?i)");
    for ch in pattern.chars() {
        match ch {
            '*' => escaped.push_str(".*"),
            '?' => escaped.push('.'),
            other => escaped.push_str(&regex::escape(&other.to_string())),
        }
    }
    regex::Regex::new(&escaped).map_err(|e| format!("invalid pattern: {e}"))
}

#[cfg(all(feature = "builtin-tools", not(feature = "fff")))]
fn stdlib_find(root: &std::path::Path, pattern: &str) -> Result<String, String> {
    let re = glob_to_regex(pattern)?;
    let mut files = Vec::new();
    if root.is_file() {
        files.push(root.to_path_buf());
    } else {
        walk_files(root, &mut files, 2_000);
    }
    let mut out = Vec::new();
    for path in files {
        let path_str = path.to_string_lossy();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if re.is_match(&path_str) || re.is_match(name) {
            out.push(path_str.into_owned());
            if out.len() >= 100 {
                break;
            }
        }
    }
    Ok(if out.is_empty() {
        "(no files found)".to_string()
    } else {
        out.join("\n")
    })
}
