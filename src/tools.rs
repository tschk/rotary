//! Built-in coding tools: read, write, edit, bash, grep, find, ls.
//! Uses rayon for parallel search (grok pattern).

use crate::agent::{ToolContext, ToolDefinition, ToolEffect, ToolFuture, ToolRegistry, ToolResult};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::debug;

#[cfg(feature = "builtin-tools")]
use rayon::prelude::*;

pub fn register_builtin_tools(registry: &mut ToolRegistry) {
    registry.register(
        ToolDefinition::new_fn(
            "read",
            "Read the contents of a file at the given path. Returns content with line numbers.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"}},"required":["path"]}"#,
            exec_read,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "write",
            "Write content to a file, creating or overwriting it.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#,
            exec_write,
        )
        .with_effect(ToolEffect::Write),
    );
    registry.register(
        ToolDefinition::new_fn(
            "edit",
            "Perform a string replacement in a file. old_string must be unique.",
            r#"{"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}},"required":["path","old_string","new_string"]}"#,
            exec_edit,
        )
        .with_effect(ToolEffect::Write),
    );
    registry.register(
        ToolDefinition::new_fn(
            "bash",
            "Execute a shell command and return stdout/stderr. Optional timeout in seconds.",
            r#"{"type":"object","properties":{"command":{"type":"string"},"cwd":{"type":"string"},"timeout":{"type":"integer"}},"required":["command"]}"#,
            exec_bash,
        )
        .with_effect(ToolEffect::Process),
    );
    registry.register(
        ToolDefinition::new_fn(
            "grep",
            "Search file contents using regex. Returns matching lines with context.",
            r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"context":{"type":"integer"}},"required":["pattern"]}"#,
            exec_grep,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "find",
            "Find files by glob pattern. Uses rayon for parallel traversal.",
            r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}"#,
            exec_find,
        )
        .with_effect(ToolEffect::Read),
    );
    registry.register(
        ToolDefinition::new_fn(
            "ls",
            "List entries in a directory.",
            r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
            exec_ls,
        )
        .with_effect(ToolEffect::Read),
    );
}

fn parse_str_field(args: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get(field)?.as_str().map(|s| s.to_string())
}

fn parse_num_field(args: &str, field: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get(field)?.as_u64()
}

fn resolve_path(ctx: &ToolContext, path: &str, write: bool) -> Result<std::path::PathBuf, String> {
    let p = std::path::PathBuf::from(path);
    let full = if p.is_absolute() {
        p
    } else {
        ctx.workspace_root.join(p)
    };
    if let Some(sb) = ctx.sandbox.as_ref() {
        sb.validate_path(&full, write).map_err(|e| e.to_string())?;
    }
    Ok(full)
}

fn exec_read(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
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

fn exec_write(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
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

fn exec_edit(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
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

fn exec_bash(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
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

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&command);
            c
        } else {
            let mut c = Command::new("bash");
            c.arg("-c").arg(&command);
            c
        };

        cmd.current_dir(&working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolResult::err("bash", format!("failed to execute: {e}")),
        };

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return ToolResult::err(
                            "bash",
                            format!("command timed out after {timeout_secs}s"),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return ToolResult::err("bash", format!("wait failed: {e}")),
            }
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut out) = child.stdout.take() {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            stdout = String::from_utf8_lossy(&buf).to_string();
        }
        if let Some(mut err) = child.stderr.take() {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
            stderr = String::from_utf8_lossy(&buf).to_string();
        }
        let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);

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

fn exec_grep(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
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
            let _ = (pattern, context);
            ToolResult::err("grep", "builtin-tools feature not enabled")
        }
    })
}

fn exec_find(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
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
            let _ = pattern;
            ToolResult::err("find", "builtin-tools feature not enabled")
        }
    })
}

fn exec_ls(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
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
fn collect_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_and_read() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));

        let write_result = exec_write(
            ctx.clone(),
            r#"{"path":"test.txt","content":"hello world"}"#.to_string(),
        )
        .await;
        assert!(!write_result.is_error);

        let read_result = exec_read(ctx, r#"{"path":"test.txt"}"#.to_string()).await;
        assert!(!read_result.is_error);
        assert!(read_result.content.contains("hello world"));
    }

    #[tokio::test]
    async fn test_edit() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));

        exec_write(
            ctx.clone(),
            r#"{"path":"edit.txt","content":"foo bar baz"}"#.to_string(),
        )
        .await;
        let edit_result = exec_edit(
            ctx,
            r#"{"path":"edit.txt","old_string":"bar","new_string":"qux"}"#.to_string(),
        )
        .await;
        assert!(!edit_result.is_error);

        let content = std::fs::read_to_string(tmp.path().join("edit.txt")).unwrap();
        assert_eq!(content, "foo qux baz");
    }

    #[tokio::test]
    async fn test_bash() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));
        let result = exec_bash(ctx, r#"{"command":"echo hello"}"#.to_string()).await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_bash_timeout() {
        let tmp = TempDir::new().unwrap();
        let ctx = Arc::new(ToolContext::new(tmp.path()));
        let result = exec_bash(ctx, r#"{"command":"sleep 2","timeout":1}"#.to_string()).await;
        assert!(result.is_error);
        assert!(result.content.contains("timed out"));
    }
}
