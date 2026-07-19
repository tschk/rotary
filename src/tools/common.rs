use crate::agent::ToolContext;

pub(crate) fn parse_str_field(args: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get(field)?.as_str().map(|s| s.to_string())
}

pub(crate) fn parse_num_field(args: &str, field: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get(field)?.as_u64()
}

pub(crate) fn resolve_path(
    ctx: &ToolContext,
    path: &str,
    write: bool,
) -> Result<std::path::PathBuf, String> {
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
