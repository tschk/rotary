//! Session-scoped payload storage for context that is omitted from provider requests.

use crate::agent::{ToolContext, ToolFuture, ToolResult};
use crate::session::{ContextArtifact, ContextArtifactKind, Session};
use parking_lot::RwLock;
use serde::Deserialize;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

pub const DEFAULT_RETRIEVAL_BYTES: usize = 8 * 1024;
pub const MAX_RETRIEVAL_BYTES: usize = 16 * 1024;
const MAX_ARTIFACT_SCAN_BYTES: u64 = 16 * 1024 * 1024;

/// Stores payload files beneath `.rx4/artifacts/<session-id>/` and records
/// their opaque references in the matching [`Session`].
pub struct ContextArtifactStore {
    workspace_root: PathBuf,
    session: Arc<RwLock<Session>>,
}

impl ContextArtifactStore {
    pub fn new(workspace_root: impl Into<PathBuf>, session: Arc<RwLock<Session>>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            session,
        }
    }

    pub fn store(
        &self,
        kind: ContextArtifactKind,
        content: &str,
    ) -> Result<ContextArtifact, String> {
        let session_id = self.session.read().id.clone();
        crate::tools::common::validate_identifier(&session_id)?;
        let dir = self.artifact_dir(&session_id)?;
        fs::create_dir_all(&dir).map_err(|error| format!("create artifact directory: {error}"))?;
        self.ensure_dir_within_workspace(&dir)?;

        let reference = format!("ctxa-{}", uuid::Uuid::new_v4());
        let path = dir.join(format!("{reference}.txt"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("create artifact: {error}"))?;
        std::io::Write::write_all(&mut file, content.as_bytes())
            .map_err(|error| format!("write artifact: {error}"))?;

        let artifact = ContextArtifact {
            reference,
            kind,
            bytes: content.len(),
            lines: content.lines().count(),
        };
        self.session.write().record_artifact(artifact.clone());
        Ok(artifact)
    }

    fn artifact_for(&self, reference: &str) -> Result<(String, ContextArtifact), String> {
        let session = self.session.read();
        let Some(artifact) = session
            .artifacts
            .iter()
            .find(|artifact| artifact.reference == reference)
            .cloned()
        else {
            return Err("unknown artifact reference".to_string());
        };
        crate::tools::common::validate_identifier(&session.id)?;
        crate::tools::common::validate_identifier(&artifact.reference)?;
        Ok((session.id.clone(), artifact))
    }

    fn read(
        &self,
        reference: &str,
        ctx: &ToolContext,
    ) -> Result<(ContextArtifact, String), String> {
        let (session_id, artifact) = self.artifact_for(reference)?;
        let dir = self.artifact_dir(&session_id)?;
        self.ensure_dir_within_workspace(&dir)?;
        let path = dir.join(format!("{}.txt", artifact.reference));
        let metadata = fs::symlink_metadata(&path).map_err(|_| "artifact payload unavailable")?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err("artifact payload unavailable".to_string());
        }
        if metadata.len() > MAX_ARTIFACT_SCAN_BYTES {
            return Err(format!(
                "artifact is too large to scan (max {MAX_ARTIFACT_SCAN_BYTES} bytes)"
            ));
        }
        if let Some(sandbox) = &ctx.sandbox {
            sandbox
                .validate_path(&path, false)
                .map_err(|_| "artifact payload unavailable")?;
        }
        let mut content = String::new();
        fs::File::open(&path)
            .and_then(|mut file| file.read_to_string(&mut content))
            .map_err(|_| "artifact payload unavailable")?;
        Ok((artifact, content))
    }

    fn artifact_dir(&self, session_id: &str) -> Result<PathBuf, String> {
        crate::tools::common::validate_identifier(session_id)?;
        Ok(self
            .workspace_root
            .join(".rx4")
            .join("artifacts")
            .join(session_id))
    }

    fn ensure_dir_within_workspace(&self, dir: &std::path::Path) -> Result<(), String> {
        let workspace = self
            .workspace_root
            .canonicalize()
            .map_err(|error| format!("resolve workspace: {error}"))?;
        let canonical_dir = dir
            .canonicalize()
            .map_err(|error| format!("resolve artifact directory: {error}"))?;
        if canonical_dir.starts_with(workspace) {
            Ok(())
        } else {
            Err("artifact directory escapes workspace".to_string())
        }
    }
}

#[derive(Deserialize)]
struct RetrieveArgs {
    reference: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    max_bytes: Option<usize>,
}

pub(crate) fn retrieve_tool(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let args: RetrieveArgs = match serde_json::from_str(&args) {
            Ok(args) => args,
            Err(error) => {
                return ToolResult::err(
                    "retrieve_context_artifact",
                    format!("invalid json: {error}"),
                );
            }
        };
        let Some(store) = &ctx.context_artifacts else {
            return ToolResult::err(
                "retrieve_context_artifact",
                "context artifact retrieval is not configured",
            );
        };
        match retrieve(store, &ctx, args) {
            Ok(content) => ToolResult::ok("retrieve_context_artifact", content),
            Err(error) => ToolResult::err("retrieve_context_artifact", error),
        }
    })
}

fn retrieve(
    store: &ContextArtifactStore,
    ctx: &ToolContext,
    args: RetrieveArgs,
) -> Result<String, String> {
    if args.reference.is_empty() {
        return Err("reference required".to_string());
    }
    let start_line = args.start_line.unwrap_or(1);
    let end_line = args.end_line.unwrap_or(usize::MAX);
    if start_line == 0 || end_line == 0 || end_line < start_line {
        return Err("line range must use positive inclusive line numbers".to_string());
    }
    let max_bytes = args.max_bytes.unwrap_or(DEFAULT_RETRIEVAL_BYTES);
    if max_bytes == 0 || max_bytes > MAX_RETRIEVAL_BYTES {
        return Err(format!(
            "max_bytes must be between 1 and {MAX_RETRIEVAL_BYTES}"
        ));
    }
    let query = args.query.filter(|query| !query.is_empty());
    let (artifact, content) = store.read(&args.reference, ctx)?;
    let mut output = String::new();
    append_bounded(
        &mut output,
        &format!(
            "artifact {} ({}, {} bytes, {} lines)\n",
            artifact.reference,
            artifact_kind_name(artifact.kind),
            artifact.bytes,
            artifact.lines,
        ),
        max_bytes,
    );
    let mut omitted = false;
    let mut matched = false;
    for (index, line) in content.lines().enumerate() {
        let line_number = index + 1;
        if line_number < start_line || line_number > end_line {
            continue;
        }
        if query.as_ref().is_some_and(|query| !line.contains(query)) {
            continue;
        }
        matched = true;
        if !append_bounded(&mut output, &format!("{line_number}: {line}\n"), max_bytes) {
            omitted = true;
            break;
        }
    }
    if !matched {
        append_bounded(&mut output, "no matching lines\n", max_bytes);
    }
    if omitted {
        append_bounded(
            &mut output,
            "…[output truncated; narrow the line range or search query]\n",
            max_bytes,
        );
    }
    Ok(output)
}

fn append_bounded(output: &mut String, text: &str, max_bytes: usize) -> bool {
    let remaining = max_bytes.saturating_sub(output.len());
    if text.len() <= remaining {
        output.push_str(text);
        return true;
    }
    let mut end = remaining;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    output.push_str(&text[..end]);
    false
}

fn artifact_kind_name(kind: ContextArtifactKind) -> &'static str {
    match kind {
        ContextArtifactKind::Spill => "spill",
        ContextArtifactKind::CompactedHistory => "compacted_history",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (
        tempfile::TempDir,
        Arc<RwLock<Session>>,
        ContextArtifactStore,
        ToolContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(RwLock::new(Session::new("session-a", "session-a")));
        let store = ContextArtifactStore::new(dir.path(), Arc::clone(&session));
        let mut ctx = ToolContext::new(dir.path());
        ctx.context_artifacts = Some(Arc::new(ContextArtifactStore::new(
            dir.path(),
            Arc::clone(&session),
        )));
        (dir, session, store, ctx)
    }

    #[test]
    fn retrieves_only_requested_lines_and_honors_output_bound() {
        let (_dir, _session, store, ctx) = fixture();
        let artifact = store
            .store(
                ContextArtifactKind::Spill,
                "one\ntwo needle\nthree\nfour needle\n",
            )
            .unwrap();
        let output = retrieve(
            &store,
            &ctx,
            RetrieveArgs {
                reference: artifact.reference,
                start_line: Some(2),
                end_line: Some(4),
                query: Some("needle".into()),
                max_bytes: Some(256),
            },
        )
        .unwrap();
        assert!(output.contains("2: two needle"));
        assert!(output.contains("4: four needle"));
        assert!(!output.contains("1: one"));
        assert!(output.len() <= 256);
    }

    #[test]
    fn rejects_unknown_references_and_invalid_ranges() {
        let (_dir, _session, store, ctx) = fixture();
        let unknown = retrieve(
            &store,
            &ctx,
            RetrieveArgs {
                reference: "../../etc/passwd".into(),
                start_line: None,
                end_line: None,
                query: None,
                max_bytes: None,
            },
        )
        .unwrap_err();
        assert_eq!(unknown, "unknown artifact reference");
        let range = retrieve(
            &store,
            &ctx,
            RetrieveArgs {
                reference: "unknown".into(),
                start_line: Some(3),
                end_line: Some(2),
                query: None,
                max_bytes: None,
            },
        )
        .unwrap_err();
        assert!(range.contains("line range"));
    }

    #[test]
    fn metadata_survives_session_persistence_and_resolves_payload() {
        let (dir, session, store, _ctx) = fixture();
        let artifact = store
            .store(ContextArtifactKind::CompactedHistory, "archived turn\n")
            .unwrap();
        let session_path = session.read().save_jsonl(dir.path()).unwrap();
        let restored = Arc::new(RwLock::new(Session::load_jsonl(&session_path).unwrap()));
        let restored_store = ContextArtifactStore::new(dir.path(), restored);
        let ctx = ToolContext::new(dir.path());
        let output = retrieve(
            &restored_store,
            &ctx,
            RetrieveArgs {
                reference: artifact.reference,
                start_line: None,
                end_line: None,
                query: None,
                max_bytes: None,
            },
        )
        .unwrap();
        assert!(output.contains("archived turn"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_replaced_payload_symlink() {
        let (dir, _session, store, ctx) = fixture();
        let artifact = store
            .store(ContextArtifactKind::Spill, "safe payload\n")
            .unwrap();
        let payload = dir
            .path()
            .join(".rx4")
            .join("artifacts")
            .join("session-a")
            .join(format!("{}.txt", artifact.reference));
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, "outside payload\n").unwrap();
        fs::remove_file(&payload).unwrap();
        std::os::unix::fs::symlink(&outside, &payload).unwrap();

        let error = retrieve(
            &store,
            &ctx,
            RetrieveArgs {
                reference: artifact.reference,
                start_line: None,
                end_line: None,
                query: None,
                max_bytes: None,
            },
        )
        .unwrap_err();
        assert_eq!(error, "artifact payload unavailable");
    }
}
