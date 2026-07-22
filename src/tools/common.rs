use crate::agent::ToolContext;
use std::path::{Path, PathBuf};

pub(crate) fn parse_str_field(args: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get(field)?.as_str().map(|s| s.to_string())
}

pub(crate) fn parse_num_field(args: &str, field: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get(field)?.as_u64()
}

pub(crate) fn resolve_path(ctx: &ToolContext, path: &str, write: bool) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
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

/// Validate that `candidate` is a safe filesystem identifier (no path separators,
/// no dot components, not empty, not an absolute path).
/// Used for session IDs, skill IDs, and other values that become filenames.
pub fn validate_identifier(candidate: &str) -> Result<(), String> {
    if candidate.is_empty() {
        return Err("identifier must not be empty".into());
    }
    if Path::new(candidate).is_absolute() {
        return Err(format!("identifier must not be absolute: {candidate}"));
    }
    for component in Path::new(candidate).components() {
        match component {
            std::path::Component::ParentDir => {
                return Err(format!("identifier must not contain '..': {candidate}"));
            }
            std::path::Component::Normal(_) => {}
            _ => {
                return Err(format!(
                    "identifier contains invalid path component: {candidate}"
                ));
            }
        }
    }
    // Reject path separators that wouldn't show as components on all platforms.
    if candidate.contains('/') || candidate.contains('\\') {
        return Err(format!(
            "identifier must not contain path separators: {candidate}"
        ));
    }
    // Reject null bytes.
    if candidate.contains('\0') {
        return Err("identifier must not contain null bytes".into());
    }
    Ok(())
}

/// Resolve a path for write operations, rejecting symlink escapes.
/// Canonicalizes the nearest existing ancestor, verifies it stays within
/// workspace, then appends remaining components. Never follows a symlink
/// that points outside the workspace.
pub fn resolve_write_path(ctx: &ToolContext, path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
    let requested = if p.is_absolute() {
        p
    } else {
        ctx.workspace_root.join(p)
    };
    // Walk up to the nearest existing ancestor and canonicalize it.
    let (ancestor, remainder) = find_existing_ancestor(&requested)?;
    let canonical_ws = ctx
        .workspace_root
        .canonicalize()
        .unwrap_or_else(|_| ctx.workspace_root.clone());
    let canonical_ancestor = ancestor
        .canonicalize()
        .map_err(|e| format!("cannot resolve path: {e}"))?;
    if !canonical_ancestor.starts_with(&canonical_ws) {
        return Err(format!(
            "path escapes workspace: {} is outside {}",
            requested.display(),
            ctx.workspace_root.display()
        ));
    }
    let full = canonical_ancestor.join(remainder);
    if let Some(sb) = ctx.sandbox.as_ref() {
        sb.validate_path(&full, true).map_err(|e| e.to_string())?;
    }
    Ok(full)
}

/// Find the nearest existing ancestor of `path` and return it plus the
/// remaining components that don't exist yet.
fn find_existing_ancestor(path: &Path) -> Result<(PathBuf, PathBuf), String> {
    if path.exists() {
        return Ok((path.to_path_buf(), PathBuf::new()));
    }
    let mut ancestor = path.to_path_buf();
    let mut tail = PathBuf::new();
    while let Some(parent) = ancestor.parent() {
        if parent.exists() {
            // Build the non-existent suffix relative to this ancestor.
            if let Ok(rel) = path.strip_prefix(parent) {
                tail = rel.to_path_buf();
            }
            return Ok((parent.to_path_buf(), tail));
        }
        if let Some(name) = ancestor.file_name() {
            tail = if tail.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                Path::new(name).join(&tail)
            };
        }
        ancestor = parent.to_path_buf();
    }
    Ok((ancestor, tail))
}

/// Assert that `resolved` (after canonicalization) stays within `workspace`.
/// Returns an error string if the path escapes.
#[allow(dead_code)] // used by P1 scan/repomap fixes
pub fn assert_within_workspace(resolved: &Path, workspace: &Path) -> Result<(), String> {
    let canonical_ws = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let canonical_resolved = resolved
        .canonicalize()
        .unwrap_or_else(|_| resolved.to_path_buf());
    if !canonical_resolved.starts_with(&canonical_ws) {
        return Err(format!(
            "path escapes workspace: {} is outside {}",
            resolved.display(),
            workspace.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_identifier_accepts_simple() {
        assert!(validate_identifier("my-skill-123").is_ok());
        assert!(validate_identifier("session_abc").is_ok());
    }

    #[test]
    fn validate_identifier_rejects_empty() {
        assert!(validate_identifier("").is_err());
    }

    #[test]
    fn validate_identifier_rejects_absolute() {
        assert!(validate_identifier("/etc/passwd").is_err());
    }

    #[test]
    fn validate_identifier_rejects_dotdot() {
        assert!(validate_identifier("../etc/passwd").is_err());
        assert!(validate_identifier("foo/../bar").is_err());
    }

    #[test]
    fn validate_identifier_rejects_separators() {
        assert!(validate_identifier("foo/bar").is_err());
        assert!(validate_identifier("foo\\bar").is_err());
    }

    #[test]
    fn validate_identifier_rejects_null() {
        assert!(validate_identifier("foo\0bar").is_err());
    }
}
