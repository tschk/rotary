use super::userspace::SandboxError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
            env_whitelist: ["PATH", "HOME", "USER", "LANG", "TERM"]
                .into_iter()
                .map(String::from)
                .collect(),
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
        lines.push("(allow file-write* (literal \"/dev/null\"))".to_string());
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
        lines.push(format!(
            "(allow file-read* (literal \"{home}/.gitconfig\"))"
        ));
        lines.push(format!(
            "(allow file-read* (literal \"{home}/.gitignore_global\"))"
        ));
        lines.push(format!(
            "(allow file-read* (literal \"{home}/.config/git/config\"))"
        ));
        lines.push(format!(
            "(allow file-read* (literal \"{home}/.config/git/ignore\"))"
        ));
        for extra in &config.extra_ro_paths {
            lines.push(format!(
                "(allow file-read* (subpath \"{}\"))",
                extra.display()
            ));
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
    pub(crate) profile_path: Option<Arc<PathBuf>>,
}

impl Drop for OsSandboxRunner {
    fn drop(&mut self) {
        if let Some(path) = self.profile_path.take() {
            if Arc::strong_count(&path) == 1 {
                let _ = std::fs::remove_file(path.as_path());
            }
        }
    }
}

impl OsSandboxRunner {
    /// Create a runner for the given config. On macOS this writes the
    /// seatbelt profile to `/tmp` so that [`Self::wrap_command`] can refer
    /// to it.
    pub fn new(config: OsSandboxConfig) -> Result<Self, SandboxError> {
        let profile_path = match config.mode {
            OsSandbox::MacosSeatbelt => {
                #[cfg(not(target_os = "macos"))]
                return Err(SandboxError::PathDenied(
                    "macOS seatbelt (sandbox-exec) not available; refuse fail-open".into(),
                ));

                #[cfg(target_os = "macos")]
                {
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
            profile_path: profile_path.map(Arc::new),
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

    pub fn config(&self) -> &OsSandboxConfig {
        &self.config
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
                // Private-by-default: mount only essential system paths + workspace.
                // Do NOT bind host root — that would expose all of /home, /root, etc.
                let mut v: Vec<String> = vec![
                    "bwrap".to_string(),
                    // Essential runtime directories read-only.
                    "--ro-bind".to_string(),
                    "/usr".to_string(),
                    "/usr".to_string(),
                    "--ro-bind".to_string(),
                    "/lib".to_string(),
                    "/lib".to_string(),
                    "--ro-bind".to_string(),
                    "/lib64".to_string(),
                    "/lib64".to_string(),
                    "--ro-bind".to_string(),
                    "/bin".to_string(),
                    "/bin".to_string(),
                    "--ro-bind".to_string(),
                    "/sbin".to_string(),
                    "/sbin".to_string(),
                    "--ro-bind".to_string(),
                    "/etc".to_string(),
                    "/etc".to_string(),
                    // Device and process filesystems.
                    "--dev".to_string(),
                    "/dev".to_string(),
                    "--proc".to_string(),
                    "/proc".to_string(),
                    // Temp storage.
                    "--tmpfs".to_string(),
                    "/tmp".to_string(),
                    // Workspace mounted read-write.
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
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        _mutex_guard: std::sync::MutexGuard<'static, ()>,
        original_path: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let guard = ENV_LOCK.lock().unwrap();
            let original_path = std::env::var_os("PATH");
            Self {
                _mutex_guard: guard,
                original_path,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(ref p) = self.original_path {
                std::env::set_var("PATH", p);
            } else {
                std::env::remove_var("PATH");
            }
        }
    }

    #[test]
    fn test_detect_sandbox_returns_valid_mode() {
        let mode = detect_sandbox();
        assert!(matches!(
            mode,
            OsSandbox::MacosSeatbelt | OsSandbox::LinuxBubblewrap | OsSandbox::UserspaceOnly
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_detect_sandbox_macos() {
        if has_seatbelt() {
            assert_eq!(detect_sandbox(), OsSandbox::MacosSeatbelt);
        } else {
            assert_eq!(detect_sandbox(), OsSandbox::UserspaceOnly);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_detect_sandbox_linux() {
        if has_bubblewrap() {
            assert_eq!(detect_sandbox(), OsSandbox::LinuxBubblewrap);
        } else {
            assert_eq!(detect_sandbox(), OsSandbox::UserspaceOnly);
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn test_detect_sandbox_other_platforms() {
        assert_eq!(detect_sandbox(), OsSandbox::UserspaceOnly);
    }

    #[test]
    fn test_has_bubblewrap_found() {
        let _guard = EnvGuard::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let bwrap_path = temp_dir.path().join("bwrap");
        std::fs::write(&bwrap_path, "").unwrap();

        let path_var = temp_dir.path();
        std::env::set_var("PATH", path_var);

        assert!(has_bubblewrap());
    }

    #[test]
    fn test_has_bubblewrap_not_found() {
        let _guard = EnvGuard::new();
        let temp_dir = tempfile::tempdir().unwrap();

        let path_var = temp_dir.path();
        std::env::set_var("PATH", path_var);

        assert!(!has_bubblewrap());
    }

    #[test]
    fn test_has_bubblewrap_is_dir() {
        let _guard = EnvGuard::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let bwrap_path = temp_dir.path().join("bwrap");
        std::fs::create_dir(&bwrap_path).unwrap();

        let path_var = temp_dir.path();
        std::env::set_var("PATH", path_var);

        assert!(!has_bubblewrap());
    }

    #[test]
    fn test_has_bubblewrap_empty_path() {
        let _guard = EnvGuard::new();
        std::env::remove_var("PATH");

        assert!(!has_bubblewrap());
    }

    #[test]
    fn test_has_bubblewrap_multiple_paths() {
        let _guard = EnvGuard::new();
        let temp_dir1 = tempfile::tempdir().unwrap();
        let temp_dir2 = tempfile::tempdir().unwrap();
        let temp_dir3 = tempfile::tempdir().unwrap();

        // Put bwrap in the second directory
        let bwrap_path = temp_dir2.path().join("bwrap");
        std::fs::write(&bwrap_path, "").unwrap();

        let paths = vec![
            temp_dir1.path().to_path_buf(),
            temp_dir2.path().to_path_buf(),
            temp_dir3.path().to_path_buf(),
        ];

        let new_path = std::env::join_paths(paths).unwrap();
        std::env::set_var("PATH", new_path);

        assert!(has_bubblewrap());
    }
}
