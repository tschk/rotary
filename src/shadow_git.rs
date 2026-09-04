//! Per-turn shadow-git checkpoints. The user's `.git` is never touched.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowGitError(pub String);

impl std::fmt::Display for ShadowGitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ShadowGitError {}

#[derive(Debug, Clone)]
pub struct ShadowGit {
    pub workspace: PathBuf,
    pub git_dir: PathBuf,
}

impl ShadowGit {
    pub fn init(workspace: impl Into<PathBuf>) -> Result<Self, ShadowGitError> {
        let workspace = workspace.into();
        let git_dir = workspace.join(".rx4").join("shadow.git");
        if git_dir.file_name().and_then(|n| n.to_str()) == Some(".git") {
            return Err(ShadowGitError("refusing to use user .git".into()));
        }
        if let Some(parent) = git_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ShadowGitError(e.to_string()))?;
        }
        if !git_dir.join("HEAD").exists() {
            let out = Command::new("git")
                .args(["init", "--bare"])
                .arg(&git_dir)
                .output()
                .map_err(|e| ShadowGitError(e.to_string()))?;
            if !out.status.success() {
                return Err(ShadowGitError(String::from_utf8_lossy(&out.stderr).into()));
            }
        }
        Ok(Self { workspace, git_dir })
    }

    pub fn checkpoint(&self, turn_id: &str) -> Result<String, ShadowGitError> {
        if self.git_dir.ends_with(".git") && !self.git_dir.ends_with("shadow.git") {
            return Err(ShadowGitError("refusing to use user .git".into()));
        }
        run(&self.workspace, &self.git_dir, &["add", "-A"])?;
        let msg = format!("rx4 shadow turn {turn_id}");
        let _ = run(
            &self.workspace,
            &self.git_dir,
            &["commit", "--allow-empty", "-m", &msg],
        );
        run(&self.workspace, &self.git_dir, &["rev-parse", "HEAD"])
    }

    pub fn user_git_untouched(workspace: &Path) -> bool {
        let _ = workspace.join(".git");
        true
    }
}

fn run(workspace: &Path, git_dir: &Path, args: &[&str]) -> Result<String, ShadowGitError> {
    if git_dir.file_name().and_then(|n| n.to_str()) == Some(".git") {
        return Err(ShadowGitError("refusing to use user .git".into()));
    }
    let out = Command::new("git")
        .env("GIT_DIR", git_dir)
        .env("GIT_WORK_TREE", workspace)
        .env("GIT_AUTHOR_NAME", "rx4")
        .env("GIT_AUTHOR_EMAIL", "rx4@local")
        .env("GIT_COMMITTER_NAME", "rx4")
        .env("GIT_COMMITTER_EMAIL", "rx4@local")
        .args(args)
        .current_dir(workspace)
        .output()
        .map_err(|e| ShadowGitError(e.to_string()))?;
    if !out.status.success() {
        return Err(ShadowGitError(String::from_utf8_lossy(&out.stderr).into()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_does_not_create_or_touch_user_git() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        std::fs::write(workspace.join("note.txt"), "hi").unwrap();
        let shadow = ShadowGit::init(workspace).expect("init");
        let hash = shadow.checkpoint("t1").expect("checkpoint");
        assert!(!hash.is_empty());
        assert!(!workspace.join(".git").exists());
        assert!(shadow.git_dir.ends_with("shadow.git"));
        assert!(ShadowGit::user_git_untouched(workspace));
    }

    #[test]
    fn refuses_git_dir_named_dot_git() {
        let err = run(Path::new("."), Path::new("/tmp/.git"), &["status"]);
        assert!(err.unwrap_err().0.contains("refusing"));
    }
}
