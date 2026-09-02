use crate::agent::{ToolContext, ToolFuture, ToolResult};
use std::sync::Arc;

pub fn apply_unified_diff(source: &str, diff: &str) -> Result<String, String> {
    if !diff.contains("@@") {
        return Err("apply_patch requires a unified diff with hunk headers".into());
    }
    let mut lines: Vec<String> = source.lines().map(str::to_string).collect();
    let mut hunks = 0usize;
    let mut i = 0usize;
    let diff_lines: Vec<&str> = diff.lines().collect();
    while i < diff_lines.len() {
        if !diff_lines[i].starts_with("@@") {
            i += 1;
            continue;
        }
        hunks += 1;
        let header = diff_lines[i];
        let start = parse_hunk_start(header)?;
        i += 1;
        let mut cursor = start.saturating_sub(1);
        while i < diff_lines.len() && !diff_lines[i].starts_with("@@") {
            let line = diff_lines[i];
            if let Some(rest) = line.strip_prefix('+') {
                if !rest.starts_with('+') && !line.starts_with("+++") {
                    if cursor > lines.len() {
                        lines.push(rest.to_string());
                    } else {
                        lines.insert(cursor, rest.to_string());
                    }
                    cursor += 1;
                }
            } else if let Some(_rest) = line.strip_prefix('-') {
                if !line.starts_with("---") && cursor < lines.len() {
                    lines.remove(cursor);
                }
            } else if let Some(rest) = line.strip_prefix(' ') {
                if cursor < lines.len() && lines[cursor] != rest {
                    return Err(format!("hunk context mismatch at line {}", cursor + 1));
                }
                cursor += 1;
            }
            i += 1;
        }
    }
    if hunks == 0 {
        return Err("apply_patch found no hunks".into());
    }
    let mut out = lines.join("\n");
    if source.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn hunk_bodies(diff: &str) -> Vec<String> {
    let mut hunks = Vec::new();
    let mut current: Option<String> = None;
    for line in diff.lines() {
        if line.starts_with("@@") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current = Some(line.to_string());
        } else if let Some(hunk) = current.as_mut() {
            hunk.push('\n');
            hunk.push_str(line);
        }
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    hunks
}

fn parse_hunk_start(header: &str) -> Result<usize, String> {
    let after = header.split("@@").nth(1).unwrap_or("").trim();
    let old = after.split_whitespace().next().unwrap_or("");
    let num = old.trim_start_matches('-').split(',').next().unwrap_or("1");
    num.parse::<usize>()
        .map_err(|_| format!("invalid hunk header: {header}"))
}

pub(crate) fn exec_apply_patch(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let v: serde_json::Value = match serde_json::from_str(&args) {
            Ok(v) => v,
            Err(e) => return ToolResult::err("apply_patch", format!("invalid json: {e}")),
        };
        let path = match v.get("path").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => return ToolResult::err("apply_patch", "path required"),
        };
        let diff = match v.get("diff").and_then(|d| d.as_str()) {
            Some(d) => d,
            None => return ToolResult::err("apply_patch", "diff required"),
        };
        let full = match crate::tools::common::resolve_path(&ctx, path, true) {
            Ok(p) => p,
            Err(e) => return ToolResult::err("apply_patch", e),
        };
        let source = match std::fs::read_to_string(&full) {
            Ok(s) => s,
            Err(e) => return ToolResult::err("apply_patch", e.to_string()),
        };
        match apply_unified_diff(&source, diff) {
            Ok(next) => match std::fs::write(&full, next) {
                Ok(()) => {
                    if let Some(buf) = &ctx.patch_hunks {
                        for hunk in hunk_bodies(diff) {
                            buf.lock().push(crate::agent::PatchHunkNotice {
                                path: path.to_string(),
                                hunk,
                            });
                        }
                    }
                    ToolResult::ok("apply_patch", format!("patched {}", full.display()))
                }
                Err(e) => ToolResult::err("apply_patch", e.to_string()),
            },
            Err(e) => ToolResult::err("apply_patch", e),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_simple_hunk() {
        let src = "alpha\nbeta\ngamma\n";
        let diff = "@@ -1,3 +1,3 @@\n alpha\n-beta\n+delta\n gamma\n";
        let out = apply_unified_diff(src, diff).unwrap();
        assert_eq!(out, "alpha\ndelta\ngamma\n");
    }

    #[test]
    fn rejects_missing_hunk() {
        assert!(apply_unified_diff("a\n", "not a diff").is_err());
    }

    #[tokio::test]
    async fn apply_patch_queues_patch_hunks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let hunks = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let mut ctx = ToolContext::new(dir.path());
        ctx.patch_hunks = Some(std::sync::Arc::clone(&hunks));
        let args = serde_json::json!({
            "path": "f.txt",
            "diff": "@@ -1,3 +1,3 @@\n alpha\n-beta\n+delta\n gamma\n"
        })
        .to_string();
        let result = exec_apply_patch(std::sync::Arc::new(ctx), args).await;
        assert!(!result.is_error);
        let queued = hunks.lock();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].path, "f.txt");
        assert!(queued[0].hunk.starts_with("@@ -1,3 +1,3 @@"));
        assert!(queued[0].hunk.contains("-beta"));
        assert!(queued[0].hunk.contains("+delta"));
    }
}
