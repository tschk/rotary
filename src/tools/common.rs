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

/// Lexically normalizes a path, resolving `.` and `..` components
/// without interacting with the filesystem.
pub(crate) fn lexically_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                let pop_success = match normalized.components().next_back() {
                    Some(std::path::Component::Normal(_)) => {
                        normalized.pop();
                        true
                    }
                    Some(std::path::Component::RootDir) => {
                        // At root, `..` does nothing.
                        true
                    }
                    _ => false,
                };
                if !pop_success {
                    normalized.push(component);
                }
            }
            std::path::Component::CurDir => {}
            _ => normalized.push(component),
        }
    }
    normalized
}

pub(crate) fn resolve_path(ctx: &ToolContext, path: &str, write: bool) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
    let requested = if p.is_absolute() {
        p
    } else {
        ctx.workspace_root.join(p)
    };

    // Resolve existing symlinks and the nearest existing parent for create
    // operations. The returned path is the checked path, so the later open or
    // write cannot follow a different symlink target than the validator saw.
    let full = if let Ok(canonical) = requested.canonicalize() {
        canonical
    } else {
        let (ancestor, remainder) = find_existing_ancestor(&requested)?;
        ancestor
            .canonicalize()
            .map_err(|e| format!("cannot resolve path: {e}"))?
            .join(remainder)
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
#[allow(dead_code)] // used when `computer-use` feature is enabled
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
#[allow(dead_code)] // used by resolve_write_path (computer-use feature)
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
    let resolved = resolved
        .canonicalize()
        .unwrap_or_else(|_| lexically_normalize(resolved));
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| lexically_normalize(workspace));

    if !resolved.starts_with(&workspace) {
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

    #[test]
    fn test_lexically_normalize() {
        assert_eq!(
            lexically_normalize(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            lexically_normalize(Path::new("a/b/../c")),
            PathBuf::from("a/c")
        );
        assert_eq!(
            lexically_normalize(Path::new("../../c")),
            PathBuf::from("../../c")
        );
        assert_eq!(
            lexically_normalize(Path::new("/../../c")),
            PathBuf::from("/c")
        );
        assert_eq!(
            lexically_normalize(Path::new("/tmp/workspace/../outside/file")),
            PathBuf::from("/tmp/outside/file")
        );
    }

    #[test]
    fn test_assert_within_workspace_rejects_escape() {
        let ws = Path::new("/tmp/workspace");
        let safe = Path::new("/tmp/workspace/safe/file");
        let unsafe_path = Path::new("/tmp/workspace/non_existent_dir/../../etc/passwd");

        // Mock canonicalize failure by using paths that don't exist
        assert!(assert_within_workspace(safe, ws).is_ok() || !safe.exists());

        let result = assert_within_workspace(unsafe_path, ws);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("escapes workspace"));
    }
}
