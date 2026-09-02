use super::userspace::SandboxError;
use super::{OsSandbox, OsSandboxConfig, OsSandboxRunner};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxLayer {
    Userspace,
    NestedFs,
    GitReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalateRetry {
    pub from: SandboxLayer,
    pub to: SandboxLayer,
    pub retry_reason: String,
}

pub fn next_layer(current: SandboxLayer) -> Option<SandboxLayer> {
    match current {
        SandboxLayer::Userspace => Some(SandboxLayer::NestedFs),
        SandboxLayer::NestedFs => Some(SandboxLayer::GitReadOnly),
        SandboxLayer::GitReadOnly => None,
    }
}

pub fn escalate_on_deny(
    current: SandboxLayer,
    deny: &SandboxError,
) -> Result<EscalateRetry, SandboxError> {
    match next_layer(current) {
        Some(to) => Ok(EscalateRetry {
            from: current,
            to,
            retry_reason: format!("sandbox deny at {current:?}: {deny}; escalate to {to:?}"),
        }),
        None => Err(deny.clone()),
    }
}

pub fn layered_os_config(base: OsSandboxConfig, layer: SandboxLayer) -> OsSandboxConfig {
    let mut config = base;
    match layer {
        SandboxLayer::Userspace => {
            config.mode = OsSandbox::UserspaceOnly;
        }
        SandboxLayer::NestedFs | SandboxLayer::GitReadOnly => {
            if config.mode == OsSandbox::UserspaceOnly {
                config.mode = match std::env::consts::OS {
                    "macos" => OsSandbox::MacosSeatbelt,
                    "linux" => OsSandbox::LinuxBubblewrap,
                    _ => OsSandbox::UserspaceOnly,
                };
            }
            if layer == SandboxLayer::GitReadOnly {
                let git = config.workspace.join(".git");
                if !config.extra_ro_paths.iter().any(|p| p == &git) {
                    config.extra_ro_paths.push(git);
                }
            }
        }
    }
    config
}

pub fn git_readonly_sbpl(workspace: &std::path::Path) -> String {
    let git = workspace.join(".git");
    format!("(deny file-write* (subpath \"{}\"))\n", git.display())
}

pub fn apply_layer_to_runner(
    runner: &OsSandboxRunner,
    layer: SandboxLayer,
) -> Result<OsSandboxRunner, SandboxError> {
    let config = layered_os_config(runner.config().clone(), layer);
    OsSandboxRunner::new(config)
}

pub fn bubblewrap_git_ro_args(workspace: PathBuf) -> Vec<String> {
    let git = workspace.join(".git");
    let p = git.display().to_string();
    vec!["--ro-bind".to_string(), p.clone(), p]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn deny_then_escalate() {
        let deny = SandboxError::PathDenied("/etc/passwd".into());
        let retry = escalate_on_deny(SandboxLayer::Userspace, &deny).unwrap();
        assert_eq!(retry.from, SandboxLayer::Userspace);
        assert_eq!(retry.to, SandboxLayer::NestedFs);
        assert!(retry.retry_reason.contains("escalate"));
        let retry = escalate_on_deny(SandboxLayer::NestedFs, &deny).unwrap();
        assert_eq!(retry.to, SandboxLayer::GitReadOnly);
    }

    #[test]
    fn never_silent_pass() {
        let deny = SandboxError::WriteDenied("/tmp/x".into());
        let err = escalate_on_deny(SandboxLayer::GitReadOnly, &deny).unwrap_err();
        assert_eq!(err, deny);
        assert!(next_layer(SandboxLayer::GitReadOnly).is_none());
    }

    #[test]
    fn layered_config_marks_git_readonly() {
        let base = OsSandboxConfig::new(
            OsSandbox::UserspaceOnly,
            PathBuf::from("/workspace/project"),
        );
        let nested = layered_os_config(base.clone(), SandboxLayer::NestedFs);
        assert!(!nested.extra_ro_paths.iter().any(|p| p.ends_with(".git")));
        let git_ro = layered_os_config(base, SandboxLayer::GitReadOnly);
        assert!(git_ro.extra_ro_paths.iter().any(|p| p.ends_with(".git")));
        let sbpl = git_readonly_sbpl(std::path::Path::new("/workspace/project"));
        assert!(sbpl.contains("(deny file-write*"));
        assert!(sbpl.contains(".git"));
        let args = bubblewrap_git_ro_args(PathBuf::from("/workspace/project"));
        assert_eq!(args[0], "--ro-bind");
        assert!(args[1].ends_with(".git"));
    }
}
