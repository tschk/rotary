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

mod os;
mod userspace;

pub use os::*;
pub use userspace::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

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
