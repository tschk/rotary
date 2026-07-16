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

// ---------------------------------------------------------------------------
// OS-level sandbox enforcement
// ---------------------------------------------------------------------------

/// Which OS-level sandbox backend to use for enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsSandbox {
    /// macOS sandbox-exec (seatbelt) enforcement.
    MacosSeatbelt,
    /// Linux bwrap (Bubblewrap) enforcement.
    LinuxBubblewrap,
    /// Fallback: userspace validation only (current behavior).
    UserspaceOnly,
}

/// Declarative configuration for an [`OsSandboxRunner`].
#[derive(Debug, Clone)]
pub struct OsSandboxConfig {
    /// Which sandbox backend to use.
    pub mode: OsSandbox,
    /// Workspace root that will be mounted read-write.
    pub workspace: PathBuf,
    /// Whether to allow network access inside the sandbox.
    pub allow_network: bool,
    /// Whether to allow read-write access to `/tmp`.
    pub allow_tmp: bool,
    /// Extra paths to bind mount read-only.
    pub extra_ro_paths: Vec<PathBuf>,
    /// Environment variables to pass through into the sandbox.
    pub env_whitelist: Vec<String>,
}

impl OsSandboxConfig {
    /// Build a config for the given `mode` rooted at `workspace`.
    pub fn new(mode: OsSandbox, workspace: PathBuf) -> Self {
        Self {
            mode,
            workspace,
            allow_network: false,
            allow_tmp: true,
            extra_ro_paths: Vec::new(),
            env_whitelist: Vec::new(),
        }
    }
}

/// Generates macOS seatbelt sandbox profile (`.sb`) files.
#[derive(Debug, Clone)]
pub struct SandboxProfileGenerator {
    config: OsSandboxConfig,
}

impl SandboxProfileGenerator {
    /// Create a generator for the given config.
    pub fn new(config: OsSandboxConfig) -> Self {
        Self { config }
    }

    /// Render the seatbelt profile text for `config`.
    pub fn generate_seatbelt_profile(config: &OsSandboxConfig) -> String {
        let workspace = config.workspace.display().to_string();
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let mut lines: Vec<String> = Vec::new();
        lines.push("(version 1)".to_string());
        lines.push("(deny default)".to_string());
        lines.push("(allow process-exec)".to_string());
        lines.push("(allow process-fork)".to_string());
        lines.push("(allow file-read*)".to_string());
        lines.push(format!("(allow file-write* (subpath \"{workspace}\"))"));
        if config.allow_tmp {
            lines.push("(allow file-write* (subpath \"/tmp\"))".to_string());
            lines.push("(allow file-write* (subpath \"/private/tmp\"))".to_string());
            lines.push("(allow file-write* (subpath \"/var/tmp\"))".to_string());
        }
        if config.allow_network {
            lines.push("(allow network*)".to_string());
        } else {
            lines.push("(deny network*)".to_string());
        }
        lines.push(format!("(deny file-read* (subpath \"{home}/.ssh\"))"));
        lines.push(format!("(deny file-read* (subpath \"{home}/.aws\"))"));
        lines.push(format!(
            "(deny file-read* (subpath \"{home}/.config/gcloud\"))"
        ));
        lines.join("\n") + "\n"
    }

    /// Write the profile to `dir/rx4-sandbox-{uuid}.sb` and return the path.
    pub fn write_profile(&self, dir: &Path) -> Result<PathBuf, SandboxError> {
        let contents = Self::generate_seatbelt_profile(&self.config);
        let id = uuid::Uuid::new_v4();
        let path = dir.join(format!("rx4-sandbox-{id}.sb"));
        std::fs::write(&path, contents).map_err(|e| SandboxError::PathDenied(e.to_string()))?;
        Ok(path)
    }
}

/// Executes commands within an OS-level sandbox.
#[derive(Debug, Clone)]
pub struct OsSandboxRunner {
    config: OsSandboxConfig,
    /// Path to the written seatbelt profile (macOS only).
    profile_path: Option<PathBuf>,
}

impl OsSandboxRunner {
    /// Create a runner for the given config. On macOS this writes the
    /// seatbelt profile to `/tmp` so that [`Self::wrap_command`] can refer
    /// to it.
    pub fn new(mut config: OsSandboxConfig) -> Result<Self, SandboxError> {
        let profile_path = match config.mode {
            OsSandbox::MacosSeatbelt => {
                if !has_seatbelt() {
                    config.mode = OsSandbox::UserspaceOnly;
                    None
                } else {
                    let gen = SandboxProfileGenerator::new(config.clone());
                    let dir = if config.allow_tmp {
                        PathBuf::from("/tmp")
                    } else {
                        std::env::temp_dir()
                    };
                    Some(gen.write_profile(&dir)?)
                }
            }
            OsSandbox::LinuxBubblewrap => {
                if !has_bubblewrap() {
                    config.mode = OsSandbox::UserspaceOnly;
                }
                None
            }
            OsSandbox::UserspaceOnly => None,
        };
        Ok(Self {
            config,
            profile_path,
        })
    }

    /// Detect which sandbox backend is available on the current system.
    pub fn is_available() -> OsSandbox {
        detect_sandbox()
    }

    /// Return the active sandbox mode.
    pub fn mode(&self) -> OsSandbox {
        self.config.mode
    }

    /// Return the full command vector with the sandbox wrapper prepended.
    pub fn wrap_command(&self, cmd: &str, args: &[&str]) -> Vec<String> {
        match self.config.mode {
            OsSandbox::MacosSeatbelt => {
                let profile = self
                    .profile_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "/tmp/rx4-sandbox.sb".to_string());
                let mut v: Vec<String> = vec![
                    "sandbox-exec".to_string(),
                    "-f".to_string(),
                    profile,
                    "--".to_string(),
                    cmd.to_string(),
                ];
                v.extend(args.iter().map(|a| a.to_string()));
                v
            }
            OsSandbox::LinuxBubblewrap => {
                let workspace = self.config.workspace.display().to_string();
                let mut v: Vec<String> = vec![
                    "bwrap".to_string(),
                    "--ro-bind".to_string(),
                    "/".to_string(),
                    "/".to_string(),
                    "--dev".to_string(),
                    "/dev".to_string(),
                    "--proc".to_string(),
                    "/proc".to_string(),
                    "--tmpfs".to_string(),
                    "/tmp".to_string(),
                    "--bind".to_string(),
                    workspace.clone(),
                    workspace,
                ];
                for extra in &self.config.extra_ro_paths {
                    let p = extra.display().to_string();
                    v.push("--ro-bind".to_string());
                    v.push(p.clone());
                    v.push(p);
                }
                v.push("--unshare-all".to_string());
                if self.config.allow_network {
                    v.push("--share-net".to_string());
                }
                v.push("--clearenv".to_string());
                for name in &self.config.env_whitelist {
                    if let Ok(value) = std::env::var(name) {
                        v.push("--setenv".to_string());
                        v.push(name.clone());
                        v.push(value);
                    }
                }
                v.push("--".to_string());
                v.push(cmd.to_string());
                v.extend(args.iter().map(|a| a.to_string()));
                v
            }
            OsSandbox::UserspaceOnly => {
                let mut v: Vec<String> = vec![cmd.to_string()];
                v.extend(args.iter().map(|a| a.to_string()));
                v
            }
        }
    }

    /// Build a [`std::process::Command`] with the sandbox wrapper applied.
    pub fn command(&self, cmd: &str, args: &[&str]) -> Result<std::process::Command, SandboxError> {
        let wrapped = self.wrap_command(cmd, args);
        if wrapped.is_empty() {
            return Err(SandboxError::CommandDenied(
                "empty sandbox command".to_string(),
            ));
        }
        let mut command = std::process::Command::new(&wrapped[0]);
        for arg in &wrapped[1..] {
            command.arg(arg);
        }
        Ok(command)
    }
}

/// Returns true if `bwrap` (Bubblewrap) is available in `PATH`.
pub fn has_bubblewrap() -> bool {
    find_in_path("bwrap")
}

/// Returns true if `sandbox-exec` (seatbelt) is available. Always true on
/// macOS.
#[cfg(target_os = "macos")]
pub fn has_seatbelt() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub fn has_seatbelt() -> bool {
    find_in_path("sandbox-exec")
}

/// Detect the best available sandbox backend for the current platform.
pub fn detect_sandbox() -> OsSandbox {
    #[cfg(target_os = "macos")]
    {
        if has_seatbelt() {
            return OsSandbox::MacosSeatbelt;
        }
    }
    #[cfg(target_os = "linux")]
    {
        if has_bubblewrap() {
            return OsSandbox::LinuxBubblewrap;
        }
    }
    let _ = has_seatbelt();
    let _ = has_bubblewrap();
    OsSandbox::UserspaceOnly
}

fn find_in_path(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    for dir in path.split([':', ';']) {
        if dir.is_empty() {
            continue;
        }
        if Path::new(dir).join(name).is_file() {
            return true;
        }
    }
    false
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

    #[test]
    fn os_escape_violation_is_recorded() {
        let mgr = SandboxManager::new(SandboxProfile::Workspace, workspace());
        mgr.log_violation(&SandboxViolation::os_escape(
            "/etc/passwd",
            "process attempted read outside sandbox",
        ));
        let violations = mgr.violations();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].kind, ViolationKind::OsEscape);
        assert_eq!(violations[0].path_or_command, "/etc/passwd");
    }

    #[test]
    fn seatbelt_profile_denies_network_when_disabled() {
        let config = OsSandboxConfig::new(OsSandbox::MacosSeatbelt, workspace());
        let profile = SandboxProfileGenerator::generate_seatbelt_profile(&config);
        assert!(profile.contains("(deny network*)"));
        assert!(!profile.contains("(allow network*)"));
    }

    #[test]
    fn seatbelt_profile_allows_network_when_enabled() {
        let mut config = OsSandboxConfig::new(OsSandbox::MacosSeatbelt, workspace());
        config.allow_network = true;
        let profile = SandboxProfileGenerator::generate_seatbelt_profile(&config);
        assert!(profile.contains("(allow network*)"));
        assert!(!profile.contains("(deny network*)"));
    }

    #[test]
    fn seatbelt_profile_allows_workspace_writes() {
        let config = OsSandboxConfig::new(OsSandbox::MacosSeatbelt, workspace());
        let profile = SandboxProfileGenerator::generate_seatbelt_profile(&config);
        assert!(profile.contains("(allow file-write* (subpath \"/workspace/project\"))"));
    }

    #[test]
    fn seatbelt_profile_denies_secrets_dirs() {
        let config = OsSandboxConfig::new(OsSandbox::MacosSeatbelt, workspace());
        let profile = SandboxProfileGenerator::generate_seatbelt_profile(&config);
        assert!(profile.contains(".ssh"));
        assert!(profile.contains(".aws"));
        assert!(profile.contains(".config/gcloud"));
    }

    #[test]
    fn seatbelt_profile_omits_tmp_when_disabled() {
        let mut config = OsSandboxConfig::new(OsSandbox::MacosSeatbelt, workspace());
        config.allow_tmp = false;
        let profile = SandboxProfileGenerator::generate_seatbelt_profile(&config);
        assert!(!profile.contains("(allow file-write* (subpath \"/tmp\"))"));
    }

    #[test]
    fn seatbelt_wrap_command_prepends_sandbox_exec() {
        let config = OsSandboxConfig::new(OsSandbox::MacosSeatbelt, workspace());
        let runner = OsSandboxRunner {
            config,
            profile_path: Some(PathBuf::from("/tmp/rx4-sandbox-test.sb")),
        };
        let wrapped = runner.wrap_command("cargo", &["build", "--release"]);
        assert_eq!(wrapped[0], "sandbox-exec");
        assert_eq!(wrapped[1], "-f");
        assert_eq!(wrapped[2], "/tmp/rx4-sandbox-test.sb");
        assert_eq!(wrapped[3], "--");
        assert_eq!(wrapped[4], "cargo");
        assert_eq!(wrapped[5], "build");
        assert_eq!(wrapped[6], "--release");
    }

    #[test]
    fn bubblewrap_wrap_command_includes_core_args() {
        let config = OsSandboxConfig::new(OsSandbox::LinuxBubblewrap, workspace());
        let runner = OsSandboxRunner {
            config,
            profile_path: None,
        };
        let wrapped = runner.wrap_command("ls", &["-la"]);
        assert_eq!(wrapped[0], "bwrap");
        assert!(wrapped.contains(&"--ro-bind".to_string()));
        assert!(wrapped.contains(&"--dev".to_string()));
        assert!(wrapped.contains(&"--proc".to_string()));
        assert!(wrapped.contains(&"--tmpfs".to_string()));
        assert!(wrapped.contains(&"--bind".to_string()));
        assert!(wrapped.contains(&"/workspace/project".to_string()));
        assert!(wrapped.contains(&"--unshare-all".to_string()));
        assert!(wrapped.contains(&"--clearenv".to_string()));
        assert!(wrapped.contains(&"--".to_string()));
        assert!(wrapped.contains(&"ls".to_string()));
        assert!(wrapped.contains(&"-la".to_string()));
    }

    #[test]
    fn bubblewrap_wrap_command_shares_net_when_network_allowed() {
        let mut config = OsSandboxConfig::new(OsSandbox::LinuxBubblewrap, workspace());
        config.allow_network = true;
        let runner = OsSandboxRunner {
            config,
            profile_path: None,
        };
        let wrapped = runner.wrap_command("curl", &["https://example.com"]);
        assert!(wrapped.contains(&"--share-net".to_string()));
    }

    #[test]
    fn bubblewrap_wrap_command_omits_share_net_when_network_denied() {
        let config = OsSandboxConfig::new(OsSandbox::LinuxBubblewrap, workspace());
        let runner = OsSandboxRunner {
            config,
            profile_path: None,
        };
        let wrapped = runner.wrap_command("curl", &["https://example.com"]);
        assert!(!wrapped.contains(&"--share-net".to_string()));
    }

    #[test]
    fn bubblewrap_wrap_command_includes_extra_ro_paths() {
        let mut config = OsSandboxConfig::new(OsSandbox::LinuxBubblewrap, workspace());
        config.extra_ro_paths = vec![PathBuf::from("/opt/data"), PathBuf::from("/usr/local")];
        let runner = OsSandboxRunner {
            config,
            profile_path: None,
        };
        let wrapped = runner.wrap_command("cat", &["file.txt"]);
        let ro_bind_count = wrapped.iter().filter(|s| *s == "--ro-bind").count();
        assert_eq!(ro_bind_count, 3);
        assert!(wrapped.contains(&"/opt/data".to_string()));
        assert!(wrapped.contains(&"/usr/local".to_string()));
    }

    #[test]
    fn bubblewrap_wrap_command_filters_env_whitelist() {
        let mut config = OsSandboxConfig::new(OsSandbox::LinuxBubblewrap, workspace());
        config.env_whitelist = vec!["RX4_TEST_VAR".to_string()];
        std::env::set_var("RX4_TEST_VAR", "test-value");
        let runner = OsSandboxRunner {
            config,
            profile_path: None,
        };
        let wrapped = runner.wrap_command("env", &[]);
        let setenv_idx = wrapped
            .iter()
            .position(|s| s == "--setenv")
            .expect("expected --setenv in wrapped command");
        assert_eq!(wrapped[setenv_idx + 1], "RX4_TEST_VAR");
        assert_eq!(wrapped[setenv_idx + 2], "test-value");
        std::env::remove_var("RX4_TEST_VAR");
    }

    #[test]
    fn bubblewrap_wrap_command_omits_unset_env_vars() {
        let mut config = OsSandboxConfig::new(OsSandbox::LinuxBubblewrap, workspace());
        config.env_whitelist = vec!["RX4_DEFINITELY_UNSET_VAR".to_string()];
        let runner = OsSandboxRunner {
            config,
            profile_path: None,
        };
        let wrapped = runner.wrap_command("env", &[]);
        assert!(!wrapped.contains(&"--setenv".to_string()));
    }

    #[test]
    fn userspace_only_wrap_command_passes_through() {
        let config = OsSandboxConfig::new(OsSandbox::UserspaceOnly, workspace());
        let runner = OsSandboxRunner {
            config,
            profile_path: None,
        };
        let wrapped = runner.wrap_command("cargo", &["test"]);
        assert_eq!(wrapped, vec!["cargo".to_string(), "test".to_string()]);
    }

    #[test]
    fn userspace_only_command_builds_without_wrapper() {
        let config = OsSandboxConfig::new(OsSandbox::UserspaceOnly, workspace());
        let runner = OsSandboxRunner {
            config,
            profile_path: None,
        };
        let cmd = runner.command("echo", &["hello"]).expect("command build");
        assert_eq!(cmd.get_program(), "echo");
    }

    #[test]
    fn detect_sandbox_returns_a_valid_variant() {
        let mode = detect_sandbox();
        assert!(matches!(
            mode,
            OsSandbox::MacosSeatbelt | OsSandbox::LinuxBubblewrap | OsSandbox::UserspaceOnly
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn detect_sandbox_prefers_seatbelt_on_macos() {
        assert_eq!(detect_sandbox(), OsSandbox::MacosSeatbelt);
        assert!(has_seatbelt());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detect_sandbox_on_linux() {
        if has_bubblewrap() {
            assert_eq!(detect_sandbox(), OsSandbox::LinuxBubblewrap);
        } else {
            assert_eq!(detect_sandbox(), OsSandbox::UserspaceOnly);
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn detect_sandbox_falls_back_on_other_platforms() {
        assert_eq!(detect_sandbox(), OsSandbox::UserspaceOnly);
    }

    #[test]
    fn os_sandbox_runner_is_available_matches_detect() {
        assert_eq!(OsSandboxRunner::is_available(), detect_sandbox());
    }

    #[test]
    fn os_sandbox_runner_new_userspace_only_keeps_mode() {
        let config = OsSandboxConfig::new(OsSandbox::UserspaceOnly, workspace());
        let runner = OsSandboxRunner::new(config).expect("runner build");
        assert_eq!(runner.mode(), OsSandbox::UserspaceOnly);
        assert!(runner.profile_path.is_none());
    }

    #[test]
    fn write_profile_creates_sb_file() {
        let tmp = std::env::temp_dir();
        let config = OsSandboxConfig::new(OsSandbox::MacosSeatbelt, workspace());
        let gen = SandboxProfileGenerator::new(config);
        let path = gen.write_profile(&tmp).expect("write profile");
        assert!(path.extension().is_some_and(|e| e == "sb"));
        assert!(path.exists());
        let contents = std::fs::read_to_string(&path).expect("read profile");
        assert!(contents.contains("(version 1)"));
        let _ = std::fs::remove_file(&path);
    }
}
