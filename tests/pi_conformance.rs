//! Pi protocol conformance tests — session format, RPC, tool mapping,
//! extension protocol, capability policy, and SDK surface.
//!
//! Gated behind the `pi-compat` feature.

#![cfg(feature = "pi-compat")]

use rx4::pi::extension::TrustState;
use rx4::pi::policy::PolicyDecision;
use rx4::pi::*;
use rx4::provider::Role;
use rx4::Agent;
use tempfile::TempDir;

const PI_VERSION: u32 = 3;

fn assert_state_event(json: &str, expected_model: &str) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_else(|e| {
        panic!("failed to parse state event JSON: {e}\nraw: {json}");
    });
    assert_eq!(v["type"], "state", "expected state event, got: {json}");
    assert!(v["model"].is_string(), "state event missing model: {json}");
    assert_eq!(v["model"].as_str().unwrap(), expected_model);
    assert!(v["scope"].is_string(), "state event missing scope: {json}");
    assert!(
        v["message_count"].is_number(),
        "state event missing message_count: {json}"
    );
    assert!(
        v["tool_count"].is_number(),
        "state event missing tool_count: {json}"
    );
}

fn assert_error_event(json: &str) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_else(|e| {
        panic!("failed to parse error event JSON: {e}\nraw: {json}");
    });
    assert_eq!(v["type"], "error", "expected error event, got: {json}");
    assert!(
        v["message"].is_string(),
        "error event missing message: {json}"
    );
}

fn assert_compaction_event(json: &str) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_else(|e| {
        panic!("failed to parse compaction event JSON: {e}\nraw: {json}");
    });
    assert_eq!(
        v["type"], "compaction",
        "expected compaction event, got: {json}"
    );
    assert!(
        v["summary"].is_string(),
        "compaction event missing summary: {json}"
    );
}

fn sample_manifest(id: &str, caps: &[&str], tools: &[&str]) -> PiExtensionManifest {
    PiExtensionManifest {
        id: id.into(),
        name: format!("{id} display"),
        version: "1.0.0".into(),
        description: "test extension".into(),
        author: Some("tester".into()),
        capabilities: caps.iter().map(|s| s.to_string()).collect(),
        tools: tools.iter().map(|s| s.to_string()).collect(),
        providers: vec![],
        entry: "index.js".into(),
        homepage: None,
        license: Some("MIT".into()),
    }
}

mod session_format {
    use super::*;

    #[test]
    fn header_version_is_three() {
        let session = PiSession::new("/demo/project", "gpt-4o");
        assert_eq!(session.header.version, PI_VERSION);
        assert_eq!(session.header.version, PI_SESSION_VERSION);
    }

    #[test]
    fn append_and_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut session = PiSession::new("/demo/project", "gpt-4o");
        session.append_message(Role::User, "hello world");
        session.append_message(Role::Assistant, "greetings");
        session.append_tool_result("call-1", "tool output");
        session.append_model_change("gpt-4o", "claude-3-opus");
        session.append_label("demo");

        let path = session.save_jsonl(tmp.path()).unwrap();
        let loaded = PiSession::load_jsonl(&path).unwrap();

        assert_eq!(loaded.header.version, PI_VERSION);
        assert_eq!(loaded.header.model, "gpt-4o");
        assert_eq!(loaded.header.project, "/demo/project");
        assert_eq!(loaded.entry_count(), session.entry_count());
        assert_eq!(loaded.message_count(), session.message_count());
    }

    #[test]
    fn entry_ids_are_sequential() {
        let mut session = PiSession::new("/demo", "gpt-4o");
        let id0 = session.append_message(Role::User, "a");
        let id1 = session.append_message(Role::Assistant, "b");
        let id2 = session.append_label("c");

        assert_eq!(id0, 1);
        assert_eq!(id1, 2);
        assert_eq!(id2, 3);

        for (i, entry) in session.entries.iter().enumerate() {
            assert_eq!(entry.id, (i + 1) as u64);
        }
    }

    #[test]
    fn parent_id_chain_is_linear() {
        let mut session = PiSession::new("/demo", "gpt-4o");
        assert!(session.entries.is_empty());

        let _ = session.append_message(Role::User, "first");
        let first_id = session.entries[0].id;
        assert!(session.entries[0].parent_id.is_none());

        let _ = session.append_message(Role::Assistant, "second");
        assert_eq!(session.entries[1].parent_id, Some(first_id));

        let _ = session.append_label("third");
        assert_eq!(session.entries[2].parent_id, Some(session.entries[1].id));
    }

    #[test]
    fn roundtrip_preserves_entry_types() {
        let tmp = TempDir::new().unwrap();
        let mut session = PiSession::new("/demo", "gpt-4o");
        session.append_message(Role::User, "hello");
        session.append_compaction("summary text", 1);
        session.append_model_change("gpt-4o", "claude");

        let path = session.save_jsonl(tmp.path()).unwrap();
        let loaded = PiSession::load_jsonl(&path).unwrap();

        assert!(matches!(
            loaded.entries[0].entry_type,
            PiEntryType::Message { .. }
        ));
        assert!(matches!(
            loaded.entries[1].entry_type,
            PiEntryType::Compaction { .. }
        ));
        assert!(matches!(
            loaded.entries[2].entry_type,
            PiEntryType::ModelChange { .. }
        ));
    }
}

mod rpc_protocol {
    use super::*;

    fn server() -> PiRpcServer {
        let agent = Agent::new();
        PiRpcServer::new(agent)
    }

    #[test]
    fn get_state_returns_state_event() {
        let srv = server();
        let resp = srv.handle_command(r#"{"method":"get-state"}"#);
        let json = resp.expect("get-state should return an event");
        assert_state_event(&json, "gpt-4o");
    }

    #[test]
    fn set_model_updates_model() {
        let srv = server();
        let json = srv
            .handle_command(r#"{"method":"set-model","provider":"openai","model":"claude-3-opus"}"#)
            .expect("set-model should return an event");
        assert_state_event(&json, "claude-3-opus");

        let state_json = srv
            .handle_command(r#"{"method":"get-state"}"#)
            .expect("get-state should return an event");
        assert_state_event(&state_json, "claude-3-opus");
    }

    #[test]
    fn compact_returns_compaction_event() {
        let srv = server();
        let json = srv
            .handle_command(r#"{"method":"compact"}"#)
            .expect("compact should return an event");
        assert_compaction_event(&json);
    }

    #[test]
    fn abort_returns_none() {
        let srv = server();
        assert!(
            srv.handle_command(r#"{"method":"abort"}"#).is_none(),
            "abort should signal shutdown by returning None"
        );
    }

    #[test]
    fn invalid_json_returns_error_event() {
        let srv = server();
        let json = srv
            .handle_command("{not valid json")
            .expect("invalid JSON should still return an error event");
        assert_error_event(&json);
    }

    #[test]
    fn unknown_command_returns_error_event() {
        let srv = server();
        let json = srv
            .handle_command(r#"{"method":"do-something-unknown"}"#)
            .expect("unknown command should return an error event");
        assert_error_event(&json);
    }
}

mod tool_name_mapping {
    use super::*;

    #[test]
    fn pi_to_rx4_known_mappings() {
        assert_eq!(pi_to_rx4_tool("read_file"), "read");
        assert_eq!(pi_to_rx4_tool("write_file"), "write");
        assert_eq!(pi_to_rx4_tool("list_dir"), "ls");
        assert_eq!(pi_to_rx4_tool("run_command"), "bash");
        assert_eq!(pi_to_rx4_tool("find_files"), "find");
        assert_eq!(pi_to_rx4_tool("code_intel"), "grep");
        assert_eq!(pi_to_rx4_tool("hashline_edit"), "edit");
        assert_eq!(pi_to_rx4_tool("spawn_agent"), "spawn_agent");
    }

    #[test]
    fn pi_to_rx4_extra_aliases() {
        assert_eq!(pi_to_rx4_tool("search_replace"), "edit");
        assert_eq!(pi_to_rx4_tool("apply_patch"), "edit");
    }

    #[test]
    fn unknown_tools_pass_through_unchanged() {
        assert_eq!(pi_to_rx4_tool("custom_tool"), "custom_tool");
        assert_eq!(pi_to_rx4_tool("my_snippet"), "my_snippet");
    }

    #[test]
    fn rx4_to_pi_known_mappings() {
        assert_eq!(rx4_to_pi_tool("read"), "read_file");
        assert_eq!(rx4_to_pi_tool("write"), "write_file");
        assert_eq!(rx4_to_pi_tool("ls"), "list_dir");
        assert_eq!(rx4_to_pi_tool("bash"), "run_command");
        assert_eq!(rx4_to_pi_tool("find"), "find_files");
        assert_eq!(rx4_to_pi_tool("grep"), "code_intel");
        assert_eq!(rx4_to_pi_tool("edit"), "hashline_edit");
        assert_eq!(rx4_to_pi_tool("spawn_agent"), "spawn_agent");
    }

    #[test]
    fn rx4_to_pi_unknown_passthrough() {
        assert_eq!(rx4_to_pi_tool("custom_tool"), "custom_tool");
    }

    #[test]
    fn roundtrip_pi_to_rx4_to_pi() {
        for name in [
            "read_file",
            "write_file",
            "list_dir",
            "run_command",
            "find_files",
            "code_intel",
            "hashline_edit",
            "spawn_agent",
        ] {
            let rx4_name = pi_to_rx4_tool(name);
            let back = rx4_to_pi_tool(rx4_name);
            assert_eq!(back, name, "roundtrip failed for {name}");
        }
    }

    #[test]
    fn is_pi_tool_name_recognizes_known() {
        for name in [
            "read_file",
            "write_file",
            "list_dir",
            "run_command",
            "find_files",
            "code_intel",
            "hashline_edit",
            "spawn_agent",
            "read",
            "write",
            "ls",
            "bash",
            "find",
            "grep",
            "edit",
        ] {
            assert!(is_pi_tool_name(name), "expected {name} to be a pi tool");
        }
        assert!(!is_pi_tool_name("definitely_not_a_tool"));
    }

    #[test]
    fn pi_tool_names_returns_known_set() {
        let names = pi_tool_names();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"list_dir"));
        assert!(names.contains(&"run_command"));
        assert!(names.contains(&"find_files"));
        assert!(names.contains(&"code_intel"));
        assert!(names.contains(&"hashline_edit"));
        assert!(names.contains(&"spawn_agent"));
    }
}

mod extension_protocol {
    use super::*;

    #[test]
    fn load_extension_from_manifest() {
        let tmp = TempDir::new().unwrap();
        let ext_dir = tmp.path().join("ext-store");
        std::fs::create_dir_all(&ext_dir).unwrap();

        let manager = PiExtensionManager::new(ext_dir);
        let manifest = sample_manifest("loader", &["read", "tool"], &["fetch"]);
        manager.load(manifest).unwrap();

        assert_eq!(manager.count(), 1);
        let ext = manager
            .get("loader")
            .expect("extension should be registered");
        assert_eq!(ext.manifest.id, "loader");
        assert_eq!(ext.manifest.version, "1.0.0");
        assert!(ext.manifest.has_capability(PiCapability::Read));
        assert!(ext.manifest.has_capability(PiCapability::Tool));
        assert!(!ext.manifest.has_capability(PiCapability::Exec));
        assert_eq!(ext.trust, TrustState::Pending);
    }

    #[test]
    fn capability_check_allow_and_deny() {
        let tmp = TempDir::new().unwrap();
        let manager = PiExtensionManager::new(tmp.path().to_path_buf());
        let manifest = sample_manifest("capped", &["read"], &[]);
        manager.load(manifest).unwrap();

        assert_eq!(
            manager.check_capability("capped", PiCapability::Read),
            PolicyDecision::Allow
        );
        assert_eq!(
            manager.check_capability("capped", PiCapability::Exec),
            PolicyDecision::Deny
        );
        assert_eq!(
            manager.check_capability("nonexistent", PiCapability::Read),
            PolicyDecision::Deny
        );
    }

    #[test]
    fn trust_lifecycle_pending_acknowledged_trusted_killed() {
        let tmp = TempDir::new().unwrap();
        let manager = PiExtensionManager::new(tmp.path().to_path_buf());
        let manifest = sample_manifest("lifecycle", &[], &[]);
        manager.load(manifest).unwrap();

        {
            let ext = manager.get("lifecycle").unwrap();
            assert_eq!(ext.trust, TrustState::Pending);
            assert!(!ext.is_alive());
        }

        manager.acknowledge("lifecycle").unwrap();
        {
            let ext = manager.get("lifecycle").unwrap();
            assert_eq!(ext.trust, TrustState::Acknowledged);
            assert!(ext.is_alive());
        }

        manager.trust("lifecycle").unwrap();
        {
            let ext = manager.get("lifecycle").unwrap();
            assert_eq!(ext.trust, TrustState::Trusted);
            assert!(ext.is_alive());
        }

        manager.kill("lifecycle").unwrap();
        {
            let ext = manager.get("lifecycle").unwrap();
            assert_eq!(ext.trust, TrustState::Killed);
            assert!(!ext.is_alive());
        }
    }

    #[test]
    fn discovery_from_directory() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("extensions");
        let alpha = root.join("alpha");
        let beta = root.join("beta");
        std::fs::create_dir_all(&alpha).unwrap();
        std::fs::create_dir_all(&beta).unwrap();
        std::fs::write(
            alpha.join("manifest.json"),
            r#"{"id":"alpha","name":"Alpha","version":"0.1.0","capabilities":["read"],"tools":["a_tool"],"entry":"index.js"}"#,
        )
        .unwrap();
        std::fs::write(
            beta.join("manifest.json"),
            r#"{"id":"beta","name":"Beta","version":"0.2.0","capabilities":["exec"],"tools":["b_tool"],"entry":"index.js"}"#,
        )
        .unwrap();

        let manager = PiExtensionManager::new(root);
        let found = manager.discover();
        assert_eq!(found.len(), 2);

        let ids: Vec<&str> = found.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"alpha"));
        assert!(ids.contains(&"beta"));

        for manifest in found {
            manager.load(manifest).unwrap();
        }
        assert_eq!(manager.count(), 2);
    }

    #[test]
    fn list_returns_registered_extensions() {
        let tmp = TempDir::new().unwrap();
        let manager = PiExtensionManager::new(tmp.path().to_path_buf());
        manager.load(sample_manifest("a", &[], &[])).unwrap();
        manager.load(sample_manifest("b", &[], &[])).unwrap();

        let listed = manager.list();
        assert_eq!(listed.len(), 2);
        for (id, _, state) in &listed {
            assert_eq!(*state, TrustState::Pending);
            assert!(id == "a" || id == "b");
        }
    }
}

mod capability_policy {
    use super::*;

    #[test]
    fn strict_mode_denies_by_default() {
        let policy = PiCapabilityPolicy::strict();
        assert_eq!(
            policy.check("ext", PiCapability::Exec),
            PolicyDecision::Deny
        );
        assert_eq!(
            policy.check("ext", PiCapability::Write),
            PolicyDecision::Deny
        );
        assert_eq!(
            policy.check("ext", PiCapability::Http),
            PolicyDecision::Deny
        );
    }

    #[test]
    fn strict_mode_allows_default_caps() {
        let policy = PiCapabilityPolicy::strict();
        assert_eq!(
            policy.check("ext", PiCapability::Read),
            PolicyDecision::Allow
        );
        assert_eq!(
            policy.check("ext", PiCapability::Session),
            PolicyDecision::Allow
        );
        assert_eq!(policy.check("ext", PiCapability::Ui), PolicyDecision::Allow);
    }

    #[test]
    fn permissive_mode_allows_by_default() {
        let policy = PiCapabilityPolicy::permissive();
        assert_eq!(
            policy.check("ext", PiCapability::Exec),
            PolicyDecision::Allow
        );
        assert_eq!(
            policy.check("ext", PiCapability::Write),
            PolicyDecision::Allow
        );
        assert_eq!(
            policy.check("ext", PiCapability::Http),
            PolicyDecision::Allow
        );
        assert_eq!(
            policy.check("ext", PiCapability::Tool),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn per_extension_allow_overrides_strict_default() {
        let mut policy = PiCapabilityPolicy::strict();
        policy.per_extension.insert(
            "special".into(),
            PiExtensionOverride {
                allow: vec!["exec".into()],
                deny: vec![],
                max_memory_mb: None,
            },
        );
        assert_eq!(
            policy.check("special", PiCapability::Exec),
            PolicyDecision::Allow
        );
        assert_eq!(
            policy.check("other", PiCapability::Exec),
            PolicyDecision::Deny
        );
    }

    #[test]
    fn per_extension_deny_overrides_permissive() {
        let mut policy = PiCapabilityPolicy::permissive();
        policy.per_extension.insert(
            "restricted".into(),
            PiExtensionOverride {
                allow: vec![],
                deny: vec!["exec".into(), "write".into()],
                max_memory_mb: Some(64),
            },
        );
        assert_eq!(
            policy.check("restricted", PiCapability::Exec),
            PolicyDecision::Deny
        );
        assert_eq!(
            policy.check("restricted", PiCapability::Write),
            PolicyDecision::Deny
        );
        assert_eq!(
            policy.check("restricted", PiCapability::Read),
            PolicyDecision::Allow
        );
        assert_eq!(
            policy.check("unrestricted", PiCapability::Exec),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn global_deny_overrides_per_extension_allow() {
        let mut policy = PiCapabilityPolicy::permissive();
        policy.deny_caps.push("exec".into());
        policy.per_extension.insert(
            "ext".into(),
            PiExtensionOverride {
                allow: vec!["exec".into()],
                deny: vec![],
                max_memory_mb: None,
            },
        );
        assert_eq!(
            policy.check("ext", PiCapability::Exec),
            PolicyDecision::Deny
        );
    }

    #[test]
    fn all_capability_types_handled() {
        let policy = PiCapabilityPolicy::permissive();
        for cap in [
            PiCapability::Exec,
            PiCapability::Write,
            PiCapability::Http,
            PiCapability::Read,
            PiCapability::Tool,
            PiCapability::Session,
            PiCapability::Ui,
        ] {
            assert_eq!(
                policy.check("ext", cap),
                PolicyDecision::Allow,
                "permissive should allow {:?}",
                cap
            );
        }

        let strict = PiCapabilityPolicy::strict();
        for cap in [
            PiCapability::Exec,
            PiCapability::Write,
            PiCapability::Http,
            PiCapability::Tool,
        ] {
            assert_eq!(
                strict.check("ext", cap),
                PolicyDecision::Deny,
                "strict should deny {:?}",
                cap
            );
        }
    }

    #[test]
    fn prompt_mode_asks_for_non_default() {
        let policy = PiCapabilityPolicy::prompt();
        assert_eq!(policy.check("ext", PiCapability::Exec), PolicyDecision::Ask);
        assert_eq!(
            policy.check("ext", PiCapability::Read),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn policy_save_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("policy.json");
        let mut policy = PiCapabilityPolicy::permissive();
        policy.deny_caps.push("exec".into());
        policy.save(&path).unwrap();

        let loaded = PiCapabilityPolicy::load(&path);
        assert_eq!(loaded.mode, PiPolicyMode::Permissive);
        assert_eq!(
            loaded.check("ext", PiCapability::Exec),
            PolicyDecision::Deny
        );
        assert_eq!(
            loaded.check("ext", PiCapability::Write),
            PolicyDecision::Allow
        );
    }
}

mod sdk_surface {
    use super::*;

    #[tokio::test]
    async fn create_session_with_default_options() {
        let handle = create_agent_session(AgentSessionOptions::default());
        assert_eq!(handle.model().await, "gpt-4o");
        assert_eq!(handle.message_count().await, 0);
        assert!(matches!(handle.transport(), SessionTransport::InProcess));
    }

    #[tokio::test]
    async fn custom_options_are_applied() {
        let tmp = TempDir::new().unwrap();
        let handle = create_agent_session(AgentSessionOptions {
            model: "claude-3-opus".into(),
            provider: Some("anthropic".into()),
            api_key: None,
            scope: "research".into(),
            workspace_root: Some(tmp.path().to_path_buf()),
            max_tool_iterations: 10,
            auto_compact_after: 5,
            transport: SessionTransport::InProcess,
        });
        assert_eq!(handle.model().await, "claude-3-opus");
        assert_eq!(handle.message_count().await, 0);
    }

    #[tokio::test]
    async fn set_model_updates_session() {
        let handle = create_agent_session(AgentSessionOptions::default());
        handle.set_model("openai", "gpt-4-turbo").await;
        assert_eq!(handle.model().await, "gpt-4-turbo");
    }

    #[tokio::test]
    async fn clear_resets_messages() {
        let handle = create_agent_session(AgentSessionOptions::default());
        handle.clear().await;
        assert_eq!(handle.message_count().await, 0);
    }

    #[tokio::test]
    async fn handle_can_be_cloned() {
        let handle = create_agent_session(AgentSessionOptions::default());
        let cloned = handle.clone();
        assert_eq!(handle.model().await, cloned.model().await);
    }

    #[cfg(feature = "providers")]
    mod with_mock_provider {
        use super::*;
        use async_trait::async_trait;
        use futures::stream;
        use rx4::provider::{Message, Provider, ProviderError, StreamEvent};
        use std::sync::Arc;

        struct MockProvider;

        #[async_trait]
        impl Provider for MockProvider {
            fn id(&self) -> &str {
                "mock"
            }

            fn name(&self) -> &str {
                "mock"
            }

            async fn stream(
                &self,
                _messages: &[Message],
                _system: &Option<String>,
                _model: &str,
                _tools: &[serde_json::Value],
            ) -> Result<
                Box<dyn futures::Stream<Item = Result<StreamEvent, ProviderError>> + Send + Unpin>,
                ProviderError,
            > {
                let events: Vec<Result<StreamEvent, ProviderError>> = vec![
                    Ok(StreamEvent::Delta("mock response".into())),
                    Ok(StreamEvent::Done),
                ];
                Ok(Box::new(stream::iter(events)))
            }
        }

        #[tokio::test]
        async fn send_prompt_with_mock_provider() {
            let mut agent = Agent::new();
            agent.set_provider(Arc::new(MockProvider));
            let handle = AgentSessionHandle::new(agent, SessionTransport::InProcess);

            let result = handle.prompt("hello", |_| {}).await;
            assert!(result.is_ok(), "prompt with mock provider should succeed");
            assert!(handle.message_count().await >= 2);
        }
    }

    #[tokio::test]
    async fn prompt_without_provider_returns_error() {
        let handle = create_agent_session(AgentSessionOptions::default());
        let result = handle.prompt("hello", |_| {}).await;
        assert!(
            result.is_err(),
            "prompt without a provider should return an error"
        );
    }
}
