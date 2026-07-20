use super::userspace::SandboxError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
        lines.push(format!("(allow file-read* (subpath \"{workspace}\"))"));
        lines.push("(allow file-read* (subpath \"/usr\"))".to_string());
        lines.push("(allow file-read* (subpath \"/bin\"))".to_string());
        lines.push("(allow file-read* (subpath \"/sbin\"))".to_string());
        lines.push("(allow file-read* (subpath \"/opt\"))".to_string());
        lines.push("(allow file-read* (subpath \"/Library\"))".to_string());
        lines.push("(allow file-read* (subpath \"/System\"))".to_string());
        lines.push("(allow file-read* (subpath \"/private/var/db/dyld\"))".to_string());
        lines.push("(allow file-read* (literal \"/dev/null\"))".to_string());
        lines.push("(allow file-read* (literal \"/dev/urandom\"))".to_string());
        lines.push(format!("(allow file-write* (subpath \"{workspace}\"))"));
        if config.allow_tmp {
            lines.push("(allow file-read* (subpath \"/tmp\"))".to_string());
            lines.push("(allow file-read* (subpath \"/private/tmp\"))".to_string());
            lines.push("(allow file-read* (subpath \"/var/tmp\"))".to_string());
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
        lines.push(format!("(deny file-read* (subpath \"{home}/.netrc\"))"));
        lines.push(format!("(deny file-read* (subpath \"{home}/.gnupg\"))"));
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
    pub(crate) config: OsSandboxConfig,
    /// Path to the written seatbelt profile (macOS only).
    pub(crate) profile_path: Option<PathBuf>,
}

impl OsSandboxRunner {
    /// Create a runner for the given config. On macOS this writes the
    /// seatbelt profile to `/tmp` so that [`Self::wrap_command`] can refer
    /// to it.
    pub fn new(config: OsSandboxConfig) -> Result<Self, SandboxError> {
        let profile_path = match config.mode {
            OsSandbox::MacosSeatbelt => {
                if !has_seatbelt() {
                    return Err(SandboxError::PathDenied(
                        "macOS seatbelt (sandbox-exec) not available; refuse fail-open".into(),
                    ));
                }
                let gen = SandboxProfileGenerator::new(config.clone());
                let dir = if config.allow_tmp {
                    PathBuf::from("/tmp")
                } else {
                    std::env::temp_dir()
                };
                Some(gen.write_profile(&dir)?)
            }
            OsSandbox::LinuxBubblewrap => {
                if !has_bubblewrap() {
                    return Err(SandboxError::PathDenied(
                        "Linux bwrap not available; refuse fail-open".into(),
                    ));
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
