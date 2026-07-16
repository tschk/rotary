//! Sandbox: userspace filesystem, command, and environment validation layer.
//!
//! This is a validation-only layer — it checks paths, commands, and env vars
//! before they are executed and rejects anything that violates the active
//! [`SandboxProfile`]. It is NOT kernel-enforced sandboxing (no seccomp,
//! seatbelt, or AppArmor). A determined process can bypass it. Use it as a
//! first line of defense and audit trail, not as a security boundary.
//!
//! Modeled after the grok nono pattern: explicit allow/deny lists, workspace
//! confinement, read-only profiles, and a violation log.

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors raised when a sandboxed operation is rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SandboxError {
    #[error("path denied by sandbox: {0}")]
    PathDenied(String),
    #[error("write denied by sandbox: {0}")]
    WriteDenied(String),
    #[error("command denied by sandbox: {0}")]
    CommandDenied(String),
    #[error("network access denied by sandbox")]
    NetworkDenied,
    #[error("environment variable denied by sandbox: {0}")]
    EnvDenied(String),
}

/// Which confinement profile the sandbox enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfile {
    /// Restrict access to the workspace root, allow temp, deny everything else.
    Workspace,
    /// Read-only filesystem access — reads anywhere, writes denied.
    ReadOnly,
    /// Custom allow/deny lists supplied by the host.
    Custom,
}

/// Declarative configuration for a [`SandboxManager`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub profile: SandboxProfile,
    pub workspace_root: PathBuf,
    #[serde(default)]
    pub allow_paths: Vec<PathBuf>,
    #[serde(default)]
    pub deny_paths: Vec<PathBuf>,
    #[serde(default)]
    pub allow_network: bool,
    #[serde(default)]
    pub allow_env: Vec<String>,
}

impl SandboxConfig {
    /// Build a config for the given profile rooted at `workspace_root`.
    pub fn new(profile: SandboxProfile, workspace_root: PathBuf) -> Self {
        Self {
            profile,
            workspace_root,
            allow_paths: Vec::new(),
            deny_paths: Vec::new(),
            allow_network: false,
            allow_env: Vec::new(),
        }
    }
}

/// A recorded sandbox violation, written to the in-memory violation log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxViolation {
    pub timestamp: DateTime<Utc>,
    pub kind: ViolationKind,
    pub path_or_command: String,
    pub reason: String,
}

/// What kind of operation was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationKind {
    Path,
    Write,
    Command,
    Network,
    Env,
}

/// Userspace sandbox validator. See the module docs for the threat model.
#[derive(Debug)]
pub struct SandboxManager {
    profile: SandboxProfile,
    workspace_root: PathBuf,
    allow_paths: Vec<PathBuf>,
    deny_paths: Vec<PathBuf>,
    allow_network: bool,
    allow_env: Vec<String>,
    active: bool,
    violations: Mutex<Vec<SandboxViolation>>,
}

impl SandboxManager {
    /// Create a new manager for the given profile rooted at `workspace`.
    pub fn new(profile: SandboxProfile, workspace: PathBuf) -> Self {
        Self {
            profile,
            workspace_root: workspace,
            allow_paths: Vec::new(),
            deny_paths: Vec::new(),
            allow_network: false,
            allow_env: Vec::new(),
            active: true,
            violations: Mutex::new(Vec::new()),
        }
    }

    /// Create a manager from a full [`SandboxConfig`].
    pub fn from_config(config: SandboxConfig) -> Self {
        Self {
            profile: config.profile,
            workspace_root: config.workspace_root,
            allow_paths: config.allow_paths,
            deny_paths: config.deny_paths,
            allow_network: config.allow_network,
            allow_env: config.allow_env,
            active: true,
            violations: Mutex::new(Vec::new()),
        }
    }

    /// Returns `true` when the sandbox is actively enforced.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Deactivate the sandbox (e.g. for trusted local sessions).
    pub fn deactivate(&mut self) {
        self.active = false;
    }

    /// Reactivate the sandbox.
    pub fn activate(&mut self) {
        self.active = true;
    }

    /// The workspace root this sandbox is confined to.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Validate that `path` may be accessed, with `write` indicating whether
    /// the caller intends to mutate it.
    pub fn validate_path(&self, path: &Path, write: bool) -> Result<(), SandboxError> {
        if !self.active {
            return Ok(());
        }

        let canonical = self.canonicalize(path);
        if self.matches_deny(&canonical) {
            let reason = "path matches deny list";
            self.log_violation(&SandboxViolation {
                timestamp: Utc::now(),
                kind: if write {
                    ViolationKind::Write
                } else {
                    ViolationKind::Path
                },
                path_or_command: canonical.display().to_string(),
                reason: reason.to_string(),
            });
            return Err(SandboxError::PathDenied(canonical.display().to_string()));
        }

        match self.profile {
            SandboxProfile::Workspace => {
                let in_workspace = canonical.starts_with(&self.workspace_root);
                let in_temp = is_temp_path(&canonical);
                let in_allow = self.matches_allow(&canonical);
                if !in_workspace && !in_temp && !in_allow {
                    let reason = "path outside workspace, temp, and allow list";
                    self.log_violation(&SandboxViolation {
                        timestamp: Utc::now(),
                        kind: ViolationKind::Path,
                        path_or_command: canonical.display().to_string(),
                        reason: reason.to_string(),
                    });
                    return Err(SandboxError::PathDenied(canonical.display().to_string()));
                }
                if write && !in_workspace && !in_temp && !in_allow {
                    let reason = "write outside workspace, temp, and allow list";
                    self.log_violation(&SandboxViolation {
                        timestamp: Utc::now(),
                        kind: ViolationKind::Write,
                        path_or_command: canonical.display().to_string(),
                        reason: reason.to_string(),
                    });
                    return Err(SandboxError::WriteDenied(canonical.display().to_string()));
                }
                Ok(())
            }
            SandboxProfile::ReadOnly => {
                if write {
                    let reason = "write denied under read-only profile";
                    self.log_violation(&SandboxViolation {
                        timestamp: Utc::now(),
                        kind: ViolationKind::Write,
                        path_or_command: canonical.display().to_string(),
                        reason: reason.to_string(),
                    });
                    return Err(SandboxError::WriteDenied(canonical.display().to_string()));
                }
                Ok(())
            }
            SandboxProfile::Custom => {
                if !self.matches_allow(&canonical) {
                    let reason = "path not in custom allow list";
                    self.log_violation(&SandboxViolation {
                        timestamp: Utc::now(),
                        kind: ViolationKind::Path,
                        path_or_command: canonical.display().to_string(),
                        reason: reason.to_string(),
                    });
                    return Err(SandboxError::PathDenied(canonical.display().to_string()));
                }
                Ok(())
            }
        }
    }

    /// Validate that `cmd` is not on the sandbox command blocklist.
    pub fn validate_command(&self, cmd: &str) -> Result<(), SandboxError> {
        if !self.active {
            return Ok(());
        }
        let normalized = cmd.trim();
        if is_blocked_command(normalized) {
            self.log_violation(&SandboxViolation {
                timestamp: Utc::now(),
                kind: ViolationKind::Command,
                path_or_command: normalized.to_string(),
                reason: "command matches blocklist pattern".to_string(),
            });
            return Err(SandboxError::CommandDenied(normalized.to_string()));
        }
        Ok(())
    }

    /// Validate that network access is permitted by the sandbox.
    pub fn validate_network(&self) -> Result<(), SandboxError> {
        if !self.active {
            return Ok(());
        }
        if !self.allow_network {
            self.log_violation(&SandboxViolation {
                timestamp: Utc::now(),
                kind: ViolationKind::Network,
                path_or_command: String::new(),
                reason: "network access not allowed".to_string(),
            });
            return Err(SandboxError::NetworkDenied);
        }
        Ok(())
    }

    /// Filter `vars` down to the set of env vars the sandbox permits.
    pub fn validate_env(&self, vars: &[(String, String)]) -> Vec<(String, String)> {
        if !self.active {
            return vars.to_vec();
        }
        vars.iter()
            .filter(|(name, _)| self.allow_env.iter().any(|allowed| allowed == name))
            .cloned()
            .collect::<Vec<_>>()
    }

    /// Record a violation in the in-memory log.
    pub fn log_violation(&self, violation: &SandboxViolation) {
        let mut guard = self.violations.lock();
        guard.push(violation.clone());
    }

    /// Return a snapshot of all recorded violations.
    pub fn violations(&self) -> Vec<SandboxViolation> {
        self.violations.lock().clone()
    }

    fn canonicalize(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        }
    }

    fn matches_allow(&self, path: &Path) -> bool {
        self.allow_paths
            .iter()
            .any(|allowed| glob_matches(allowed, path))
    }

    fn matches_deny(&self, path: &Path) -> bool {
        self.deny_paths
            .iter()
            .any(|denied| glob_matches(denied, path))
    }
}

/// Returns true when `pattern` (a simple glob with `*` and `**`) matches `path`.
fn glob_matches(pattern: &Path, path: &Path) -> bool {
    let pattern_str = pattern.to_string_lossy();
    let path_str = path.to_string_lossy();
    if pattern_str == path_str {
        return true;
    }
    if !pattern_str.contains('*') && !pattern_str.contains('?') {
        return path_str.starts_with(&*pattern_str)
            && path_str[pattern_str.len()..]
                .chars()
                .next()
                .map(|c| c == '/')
                .unwrap_or(true);
    }
    glob_match_segments(pattern_str.as_ref(), path_str.as_ref())
}

/// Recursive glob matcher supporting `*` (within a path segment), `**`
/// (across segments), and `?` (single char).
fn glob_match_segments(pattern: &str, text: &str) -> bool {
    glob_match(pattern.as_bytes(), text.as_bytes())
}

fn glob_match(pat: &[u8], txt: &[u8]) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_pi, mut star_ti): (Option<usize>, usize) = (None, 0);
    let (mut glob_pi, mut glob_ti): (Option<usize>, usize) = (None, 0);
    while ti < txt.len() {
        if pi < pat.len() && pat[pi] == b'*' && pi + 1 < pat.len() && pat[pi + 1] == b'*' {
            glob_pi = Some(pi);
            glob_ti = ti;
            pi += 2;
            if pi < pat.len() && pat[pi] == b'/' {
                pi += 1;
            }
            continue;
        }
        if pi < pat.len() && pat[pi] == b'*' {
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
            continue;
        }
        if pi < pat.len() && (pat[pi] == b'?' || pat[pi] == txt[ti]) && pat[pi] != b'/' {
            pi += 1;
            ti += 1;
            continue;
        }
        if pi < pat.len() && pat[pi] == txt[ti] {
            pi += 1;
            ti += 1;
            continue;
        }
        if let Some(spi) = star_pi {
            if txt[star_ti] == b'/' {
                star_pi = None;
            } else {
                star_ti += 1;
                ti = star_ti;
                pi = spi + 1;
                continue;
            }
        }
        if let Some(gpi) = glob_pi {
            glob_ti += 1;
            ti = glob_ti;
            pi = gpi + 2;
            if pi < pat.len() && pat[pi] == b'/' {
                pi += 1;
            }
            continue;
        }
        return false;
    }
    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

/// Returns true for well-known temporary directories.
fn is_temp_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("/tmp/")
        || s.starts_with("/var/tmp/")
        || s.starts_with("/private/tmp/")
        || s.starts_with("/private/var/tmp/")
        || s.starts_with("/dev/shm/")
}

/// Returns true when `cmd` matches a known sandbox-escape pattern.
fn is_blocked_command(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    let patterns = [
        "rm -rf /",
        "rm -rf /*",
        "rm -rf ~",
        "rm -rf $home",
        "chmod 777",
        "chmod -r 777",
        "sudo ",
        "sudo",
        "su ",
        "su root",
        "kill -9",
        "kill -9 -1",
        ":(){:|:&};:",
        "mkfs",
        "dd if=/dev/zero of=/dev/",
        "shutdown",
        "reboot",
        "halt",
        "init 0",
        "init 6",
        "> /dev/sda",
        "curl | sh",
        "curl | bash",
        "wget | sh",
        "wget | bash",
    ];
    patterns.iter().any(|p| lower.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn workspace() -> PathBuf {
        PathBuf::from("/workspace/project")
    }

    #[test]
    fn workspace_profile_allows_read_inside_workspace() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        let path = Path::new("/workspace/project/src/main.rs");
        assert!(mgr.validate_path(path, false).is_ok());
    }

    #[test]
    fn workspace_profile_allows_write_inside_workspace() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        let path = Path::new("/workspace/project/src/main.rs");
        assert!(mgr.validate_path(path, true).is_ok());
    }

    #[test]
    fn workspace_profile_denies_read_outside_workspace() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        let path = Path::new("/etc/passwd");
        let err = mgr.validate_path(path, false).unwrap_err();
        assert_eq!(err, SandboxError::PathDenied("/etc/passwd".to_string()));
    }

    #[test]
    fn workspace_profile_denies_write_outside_workspace() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        let path = Path::new("/etc/hosts");
        let err = mgr.validate_path(path, true).unwrap_err();
        assert!(matches!(
            err,
            SandboxError::PathDenied(_) | SandboxError::WriteDenied(_)
        ));
    }

    #[test]
    fn workspace_profile_allows_temp_writes() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        let path = Path::new("/tmp/build-output.log");
        assert!(mgr.validate_path(path, true).is_ok());
    }

    #[test]
    fn workspace_profile_denies_path_in_deny_list() {
        let config = SandboxConfig {
            profile: SandboxProfile::Workspace,
            workspace_root: workspace(),
            allow_paths: vec![],
            deny_paths: vec![PathBuf::from("/workspace/project/secret.key")],
            allow_network: false,
            allow_env: vec![],
        };
        let mgr = SandboxManager::from_config(config);
        let path = Path::new("/workspace/project/secret.key");
        assert!(mgr.validate_path(path, false).is_err());
    }

    #[test]
    fn readonly_profile_allows_reads_anywhere() {
        let mgr = SandboxManager::new(SandboxProfile::ReadOnly, workspace());
        let path = Path::new("/etc/passwd");
        assert!(mgr.validate_path(path, false).is_ok());
    }

    #[test]
    fn readonly_profile_denies_writes() {
        let mgr = SandboxManager::new(SandboxProfile::ReadOnly, workspace());
        let path = Path::new("/workspace/project/src/main.rs");
        let err = mgr.validate_path(path, true).unwrap_err();
        assert_eq!(
            err,
            SandboxError::WriteDenied("/workspace/project/src/main.rs".to_string())
        );
    }

    #[test]
    fn custom_profile_allows_listed_paths() {
        let config = SandboxConfig {
            profile: SandboxProfile::Custom,
            workspace_root: workspace(),
            allow_paths: vec![PathBuf::from("/data/cache")],
            deny_paths: vec![],
            allow_network: false,
            allow_env: vec![],
        };
        let mgr = SandboxManager::from_config(config);
        assert!(mgr
            .validate_path(Path::new("/data/cache/item.bin"), false)
            .is_ok());
        assert!(mgr.validate_path(Path::new("/etc/passwd"), false).is_err());
    }

    #[test]
    fn custom_profile_glob_deny() {
        let config = SandboxConfig {
            profile: SandboxProfile::Custom,
            workspace_root: workspace(),
            allow_paths: vec![PathBuf::from("/data/**")],
            deny_paths: vec![PathBuf::from("/data/secret/**")],
            allow_network: false,
            allow_env: vec![],
        };
        let mgr = SandboxManager::from_config(config);
        assert!(mgr
            .validate_path(Path::new("/data/cache/item.bin"), false)
            .is_ok());
        assert!(mgr
            .validate_path(Path::new("/data/secret/key.pem"), false)
            .is_err());
    }

    #[test]
    fn command_blocklist_denies_rm_rf_root() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        assert!(mgr.validate_command("rm -rf /").is_err());
    }

    #[test]
    fn command_blocklist_denies_sudo() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        assert!(mgr.validate_command("sudo apt-get install foo").is_err());
    }

    #[test]
    fn command_blocklist_denies_chmod_777() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        assert!(mgr.validate_command("chmod 777 /workspace").is_err());
    }

    #[test]
    fn command_blocklist_denies_kill_9() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        assert!(mgr.validate_command("kill -9 1234").is_err());
    }

    #[test]
    fn command_blocklist_allows_safe_commands() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        assert!(mgr.validate_command("cargo build").is_ok());
        assert!(mgr.validate_command("ls -la").is_ok());
    }

    #[test]
    fn env_filter_keeps_allowed_vars() {
        let config = SandboxConfig {
            profile: SandboxProfile::Workspace,
            workspace_root: workspace(),
            allow_paths: vec![],
            deny_paths: vec![],
            allow_network: false,
            allow_env: vec!["PATH".to_string(), "HOME".to_string()],
        };
        let mgr = SandboxManager::from_config(config);
        let vars = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), "/home/user".to_string()),
            ("SECRET_TOKEN".to_string(), "s3cr3t".to_string()),
        ];
        let filtered = mgr.validate_env(&vars);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().any(|(k, _)| k == "PATH"));
        assert!(filtered.iter().any(|(k, _)| k == "HOME"));
        assert!(!filtered.iter().any(|(k, _)| k == "SECRET_TOKEN"));
    }

    #[test]
    fn env_filter_passes_through_when_inactive() {
        let mut mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        mgr.deactivate();
        let vars = vec![("SECRET".to_string(), "value".to_string())];
        let filtered = mgr.validate_env(&vars);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn network_denied_by_default() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        assert!(matches!(
            mgr.validate_network(),
            Err(SandboxError::NetworkDenied)
        ));
    }

    #[test]
    fn network_allowed_when_configured() {
        let config = SandboxConfig {
            profile: SandboxProfile::Workspace,
            workspace_root: workspace(),
            allow_paths: vec![],
            deny_paths: vec![],
            allow_network: true,
            allow_env: vec![],
        };
        let mgr = SandboxManager::from_config(config);
        assert!(mgr.validate_network().is_ok());
    }

    #[test]
    fn violation_logging_records_denied_path() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        let _ = mgr.validate_path(Path::new("/etc/passwd"), false);
        let violations = mgr.violations();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].kind, ViolationKind::Path);
        assert_eq!(violations[0].path_or_command, "/etc/passwd");
    }

    #[test]
    fn violation_logging_records_denied_command() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        let _ = mgr.validate_command("rm -rf /");
        let violations = mgr.violations();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].kind, ViolationKind::Command);
        assert_eq!(violations[0].path_or_command, "rm -rf /");
    }

    #[test]
    fn inactive_sandbox_allows_everything() {
        let mut mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        mgr.deactivate();
        assert!(!mgr.is_active());
        assert!(mgr.validate_path(Path::new("/etc/passwd"), true).is_ok());
        assert!(mgr.validate_command("rm -rf /").is_ok());
    }

    #[test]
    fn relative_path_resolved_against_workspace() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        assert!(mgr.validate_path(Path::new("src/main.rs"), true).is_ok());
    }
}
