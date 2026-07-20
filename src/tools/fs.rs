use super::common::{parse_num_field, parse_str_field, resolve_path};
use crate::agent::{ToolContext, ToolFuture, ToolResult};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::debug;

#[cfg(feature = "builtin-tools")]
use rayon::prelude::*;

pub(crate) fn exec_read(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let path = match parse_str_field(&args, "path") {
            Some(p) => p,
            None => return ToolResult::err("read", "path required"),
        };
        let offset = parse_num_field(&args, "offset").unwrap_or(0) as usize;
        let limit = parse_num_field(&args, "limit").unwrap_or(2000) as usize;

        let full = match resolve_path(&ctx, &path, false) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("read", e),
        };
        match std::fs::read_to_string(&full) {
            Ok(content) => {
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
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return ToolResult::err("write", format!("mkdir failed: {e}"));
                }
            }
        }
        match std::fs::write(&full, &content) {
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
        let content = match std::fs::read_to_string(&full) {
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
        match std::fs::write(&full, &new_content) {
            Ok(_) => ToolResult::ok("edit", format!("edited {}", path)),
            Err(e) => ToolResult::err("edit", format!("write failed: {e}")),
        }
    })
}

pub(crate) fn exec_bash(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let command = match parse_str_field(&args, "command") {
            Some(c) => c,
            None => return ToolResult::err("bash", "command required"),
        };
        let cwd = parse_str_field(&args, "cwd");
        let timeout_secs = parse_num_field(&args, "timeout").unwrap_or(120);

        if let Some(sb) = ctx.sandbox.as_ref() {
            if let Err(e) = sb.validate_command(&command) {
                return ToolResult::err("bash", e.to_string());
            }
        }

        let working_dir = if let Some(cwd) = cwd {
            match resolve_path(&ctx, &cwd, false) {
                Ok(p) => p,
                Err(e) => return ToolResult::err("bash", e),
            }
        } else if let Some(sb) = ctx.sandbox.as_ref() {
            if let Err(e) = sb.validate_path(&ctx.workspace_root, false) {
                return ToolResult::err("bash", e.to_string());
            }
            ctx.workspace_root.clone()
        } else {
            ctx.workspace_root.clone()
        };

        let mut cmd = if let Some(os) = ctx.os_sandbox.as_ref() {
            // Wrap bash -c under seatbelt/bwrap; convert std Command → tokio.
            match os.command("bash", &["-c", &command]) {
                Ok(mut c) => {
                    c.current_dir(&working_dir);
                    c.stdout(Stdio::piped()).stderr(Stdio::piped());
                    let mut tc = Command::from(c);
                    tc.kill_on_drop(true);
                    tc
                }
                Err(e) => return ToolResult::err("bash", e.to_string()),
            }
        } else if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&command);
            c.current_dir(&working_dir);
            c.stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            c
        } else {
            let mut c = Command::new("bash");
            c.arg("-c").arg(&command);
            c.current_dir(&working_dir);
            c.stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            c
        };

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolResult::err("bash", format!("failed to execute: {e}")),
        };

        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        let timeout = Duration::from_secs(timeout_secs);

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
                    #[cfg(feature = "ipc")]
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
            let (stdout_buf, stderr_buf, wait_res) =
                tokio::join!(stdout_task, stderr_task, wait_task);
            let exit_code = wait_res?;
            // If process still has leftover status after pipes closed:
            let exit_code = if exit_code == -1 {
                child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1)
            } else {
                exit_code
            };
            Ok::<_, String>((stdout_buf, stderr_buf, exit_code))
        };

        let (stdout_buf, stderr_buf, exit_code) = match tokio::time::timeout(timeout, drain).await {
            Ok(Ok(v)) => v,
            Ok(Err(msg)) => return ToolResult::err("bash", msg),
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return ToolResult::err("bash", format!("command timed out after {timeout_secs}s"));
            }
        };
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
        ToolResult::ok("bash", result)
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

        #[cfg(feature = "builtin-tools")]
        {
            let regex = match regex::Regex::new(&pattern) {
                Ok(r) => r,
                Err(e) => return ToolResult::err("grep", format!("invalid regex: {e}")),
            };

            let files: Vec<std::path::PathBuf> = if full.is_file() {
                vec![full.clone()]
            } else {
                collect_files(&full)
            };

            let results: Vec<String> = files
                .par_iter()
                .filter_map(|file| {
                    let content = std::fs::read_to_string(file).ok()?;
                    let lines: Vec<&str> = content.lines().collect();
                    let mut matches = Vec::new();
                    for (i, line) in lines.iter().enumerate() {
                        if regex.is_match(line) {
                            let start = i.saturating_sub(context);
                            let end = (i + context + 1).min(lines.len());
                            for (j, ctx_line) in lines[start..end].iter().enumerate() {
                                let line_num = start + j + 1;
                                let marker = if start + j == i { ">" } else { " " };
                                matches.push(format!(
                                    "{marker} {:>6}\t{}:{line_num}\t{ctx_line}",
                                    "",
                                    file.file_name()?.to_string_lossy(),
                                    line_num = line_num
                                ));
                            }
                            if context > 0 && end < lines.len() {
                                matches.push("  ---".to_string());
                            }
                        }
                    }
                    if matches.is_empty() {
                        None
                    } else {
                        Some(matches.join("\n"))
                    }
                })
                .collect();

            if results.is_empty() {
                ToolResult::ok("grep", "(no matches)")
            } else {
                ToolResult::ok("grep", results.join("\n"))
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

        #[cfg(feature = "builtin-tools")]
        {
            let files = collect_files(&full);
            let glob_pattern = match glob::Pattern::new(&pattern) {
                Ok(p) => p,
                Err(e) => return ToolResult::err("find", format!("invalid glob: {e}")),
            };

            let matched: Vec<String> = files
                .par_iter()
                .filter_map(|f| {
                    let name = f.file_name()?.to_string_lossy().to_string();
                    let path_str = f.to_string_lossy().to_string();
                    if glob_pattern.matches(&name) || glob_pattern.matches(&path_str) {
                        Some(path_str)
                    } else {
                        None
                    }
                })
                .collect();

            if matched.is_empty() {
                ToolResult::ok("find", "(no files found)")
            } else {
                ToolResult::ok("find", matched.join("\n"))
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
        match std::fs::read_dir(&full) {
            Ok(entries) => {
                let mut items: Vec<(String, bool)> = entries
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        let is_dir = e.file_type().ok()?.is_dir();
                        Some((name, is_dir))
                    })
                    .collect();
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

#[cfg(feature = "builtin-tools")]
pub(crate) fn collect_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    use rayon::prelude::*;
    let mut files = Vec::new();

    if root.is_file() {
        files.push(root.to_path_buf());
        return files;
    }

    #[cfg(feature = "builtin-tools")]
    {
        let walker = ignore::WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .build();

        let entries: Vec<_> = walker.filter_map(|e| e.ok()).collect();
        files = entries
            .par_iter()
            .filter_map(|e| {
                if e.file_type()?.is_file() {
                    Some(e.path().to_path_buf())
                } else {
                    None
                }
            })
            .collect();
    }

    files
}
