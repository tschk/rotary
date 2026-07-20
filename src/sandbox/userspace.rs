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
    /// A process attempted to escape the OS-level sandbox boundary.
    OsEscape,
}

impl SandboxViolation {
    /// Record an OS-level sandbox escape attempt.
    pub fn os_escape(path_or_command: &str, reason: &str) -> Self {
        Self {
            timestamp: Utc::now(),
            kind: ViolationKind::OsEscape,
            path_or_command: path_or_command.to_string(),
            reason: reason.to_string(),
        }
    }
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

    /// Allow or deny network tools checked via [`Self::validate_network`].
    pub fn set_allow_network(&mut self, allow: bool) {
        self.allow_network = allow;
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
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        };
        // Resolve symlinks when path exists; for create targets resolve parent.
        if let Ok(c) = std::fs::canonicalize(&abs) {
            return c;
        }
        if let Some(parent) = abs.parent() {
            if let Ok(cp) = std::fs::canonicalize(parent) {
                if let Some(name) = abs.file_name() {
                    return cp.join(name);
                }
            }
        }
        abs
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
/// Delegates root-wipe / pipe-to-shell detection to permissions helpers so
/// `rm -rf /tmp/...` is not false-positive denied.
fn is_blocked_command(cmd: &str) -> bool {
    if crate::permissions::is_dangerous_shell_command(cmd) {
        return true;
    }
    let lower = cmd.to_lowercase();
    let patterns = [
        "chmod 777",
        "chmod -r 777",
        "sudo ",
        "su root",
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
    ];
    patterns.iter().any(|p| lower.contains(p))
}

// ---------------------------------------------------------------------------
// OS-level sandbox enforcement
// ---------------------------------------------------------------------------
