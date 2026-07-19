# Changelog

## [0.3.17] — 2026-07-19

### Added
- `AsyncApprover` + `ChannelAsyncApprover` (pi-style async beforeToolCall)
- `Agent::set_async_approver` / `clear_async_approver` (preferred over sync Approver for UI)
- Lightweight shell AST: `shell_ast`, `shell_argv`, `shell_simples`, `ShellNode`, `ShellSimple`
- CLI `--approval always_allow|always_deny|ask` (exec default always_allow)
- CLI `--full-access` to elevate policy (incl. computer_use)

### Changed
- ComputerUse scope default policy is `workspace_write` (not full_access); elevate with `--full-access` / host policy
- Tool gate: policy evaluate first, then async/sync Approver on Ask (pi order)

## [0.3.16] — 2026-07-19

### Added
- `ChannelApprover` — blocking channel-based Approver for interactive hosts
- IPC: `set_scope`, `get_policy`, `set_policy`, `set_approver`, `clear_authorizer`
- IPC `state` includes policy/approver flags

### Docs
- Ask semantics: blocking Approver required for same-turn Allow; `ApprovalRequired` alone does not resume
- Custom Authorizer snapshots not auto-refreshed on `set_policy`/`set_scope`

## [0.3.15] — 2026-07-19

### Fixed
- `Agent::set_scope` no longer wipes host-owned shell allow/deny lists
- `Policy::apply_scope` / `with_scope` — mode + sandbox only; preserves host fields

## [0.3.14] — 2026-07-19

### Added
- `Authorizer` trait + `PolicyAuthorizer` (pi `beforeToolCall` shape; host-replaceable gate)
- `Agent::set_authorizer` / `clear_authorizer`
- `shell_segments`, `shell_command_matches_any`, `shell_command_matches_all` pure helpers
- `Policy.enforce_dangerous_shell` (hosts can disable built-in dangerous hard-deny)

### Changed
- Shell allow requires **every** segment to match (deny still any-segment)
- Shell allow/deny documented as host-owned lists; engine only matches
- AGENTS.md: capability vs policy gate clarified

## [0.3.13] — 2026-07-19

### Added
- Quote/pipe-aware shell segment matching for allow/deny and dangerous-shell checks
- `graph_memory` module split: `graph` / `extract` / `scan` / `dream`

### Changed
- Graph internals (`nodes`/`edges`/`adjacency`/`remove_*`) are `pub(crate)` for submodule access

## [0.3.12] — 2026-07-19

### Added
- `Policy.shell_deny` + `with_shell_deny` (deny globs before allow)
- `McpClient::connect_sse_get` — long-lived SSE GET channel + POST requests (fallback to streamable HTTP)
- Module splits under 1k where practical: `agent/{mod,tool_types}`, `tools/{mod,fs,extended,common}`, `sandbox/{mod,userspace,os}`, `skill_engine/{mod,engine,registry}`

### Changed
- `mcp` feature enables `futures` for SSE byte streams

## [0.3.11] — 2026-07-19

### Added
- `Policy.shell_allow` + `shell_rule_matches` / `shell_command_allowed` (e.g. `git *`)
- `McpClient::connect_config` from marketplace host config
- Public crate reexports: context, extract, ranking, slash, subagent, multiagent, permissions helpers, ACP (ipc)
- `extract_*_loose` + `rank_with_query`
- Tokio always-on for agent loop (feature matrix fixed)

### Fixed
- Dangerous shell deny no longer flags `rm -rf /tmp/...` as root wipe
- ACP module gated on `ipc` (needs tokio net/sync extras)
- `no-default-features` build compiles again

## [0.3.10] — 2026-07-19

### Added
- Dangerous shell command hard-deny under non-`FullAccess` modes (`is_dangerous_shell_command`)
- Project context merges all present instruction files (AGENTS/CLAUDE/CRUSH/GEMINI/.cursor/rules)

### Removed
- Unused optional `rmcp` dependency from `mcp` feature (hand-rolled client only)

### Fixed
- `rx4` bin unused imports / dead non-providers `setup_provider` stub
- Docs: MCP feature no longer claims `rmcp`

## [0.3.9] — 2026-07-19

### Added
- Gating lifecycle hooks (`HookDecision`, `run_before_tool`) — hooks can deny or modify tool args
- Path-aware write authorization (`path_outside_workspace` / `authorize_with_workspace`)
- Approver receives real tool arguments (no empty dummy call)
- Builtin tools: `web_fetch`, `todo`, `spawn_agent`, `enter_plan_mode` / `exit_plan_mode`, `lsp_*`
- MCP remote transports: `McpClient::connect_http` / `connect_sse` (streamable HTTP + SSE body parse)
- Marketplace `McpServerConfig.transport` (`stdio` | `http` | `sse`) + optional `url` / `headers`
- `register_spawn_agent_tool` host helper; `ToolContext` provider/tools/pending_scope/lsp fields

### Changed
- `HookFn` now returns `HookDecision` (breaking for hosts that registered unit hooks)
- `VERSION` tracks `CARGO_PKG_VERSION`

### Fixed
- Docs: OS sandbox marked implemented (not deferred)

## [0.3.8] — 2026-07-17

### Added
- `rx4 exec --stream-json` NDJSON event stream (noninteractive / CI)
- `Event::ApprovalRequired(ApprovalRequest)` rich approval UX payload
- `Session::export_codex_jsonl` / `import_codex_jsonl` Codex-friendly session IO
- `Policy.enable_os_sandbox` policy-plugin flag (`with_os_sandbox`)
- `WorkPack` — specialist agents as markdown + YAML frontmatter data

## [0.3.7] — 2026-07-17

### Changed
- `Policy::default()` is `workspace_write` (matches `Agent::new`; not full access)

### Fixed
- Docs: `set_skill_engine` / `enable_auto_dream` honesty; IPC `RX4_IPC_TOKEN` docs

## [0.3.6] — 2026-07-17

### Added
- `Agent::set_skill_engine` — post-prompt `BackgroundReviewer` when attached
- `Agent::enable_auto_dream` — optional dream consolidation after graph extract
- CLI `build_agent` wires sandbox, OS bash wrap, skills, graph + auto_dream

### Fixed
- Hosts: telekinesis Anthropic stream applies `apply_cache_control`

## [0.3.5] — 2026-07-17

### Added
- Skills inject into system prompt via `Agent::set_skill_registry` + `auto_activate`
- Graph memory extraction after each agent run via `set_graph_memory`
- Prompt-cache `cache_control` applied on Anthropic provider stream bodies
- OS sandbox wrap for bash (`enable_os_sandbox` / seatbelt + bwrap)

### Fixed
- crates.io publish: keywords capped at 5

## [0.3.4] — 2026-07-17

### Added
- Tool sandbox wiring on builtin FS/bash paths; real bash timeouts
- IPC Unix socket mode `0o600` + optional `RX4_IPC_TOKEN` gate
- Marketplace: name sanitization, install blocklist, required sha256
- Session SQLite save/load (`sqlite-sessions`)
- ACP JSON-RPC host (`initialize`, `session/*`)
- Plugin registry loads marketplace installs / plugin dirs
- Subagent manager runs real `Agent::prompt` when a provider is attached
- `Agent::load_project_context` merges AGENTS.md into system prompt

### Changed
- Default policy is `workspace_write` (process tools Ask)
- Tool cache only for `ToolEffect::Read`; Write/Process invalidate cache
- Tool results redacted via `secrets::Redactor`

## [0.3.3] — 2026-03-16

### Added
- Parallel tool dispatch via `JoinSet` for `ToolEffect::Read` and `Network` batches
- `ConfidencePrior` (Beta-Binomial posterior) for skill confidence
- `apply_compaction` and token-based auto-compact trigger (`estimate_messages`)
- `SkillRegistry::with_semantic_client` and `auto_activate_async` (embedding rank + keyword fallback)

### Changed
- `Agent::compact` uses `compaction::apply_compaction` instead of first/last truncation
- `auto_compact_after` is interpreted as estimated **token** threshold before prompt (was message count)

## [0.3.2] — 2025-01

### Added
- Computer-use tools (13 cu_* tools via rs_peekaboo)
- MCP client (JSON-RPC 2.0 over stdio, rmcp)
- Memory module (SQLite-backed)
- Graph memory (knowledge graph with pagerank)
- Skill engine (self-improving, Beta-Binomial confidence)
- Model router (tiered routing)
- Multi-agent coordination (coordinator/worker/reviewer/researcher)
- Cost tracking (per-model pricing)
- Secret redaction module
- Repo map (pagerank-ranked symbols)
- Prompt caching helpers (Anthropic ephemeral)
- Guardrails (empty turn, repeated failure detection)
- Subagent manager (workspace isolation)
- LSP client
- ACP / plugin / marketplace scaffolding (filled out in 0.3.4)
- Context compaction (auto-compact)
- Structured extraction (JSON contracts)
- Slash command parsing

### Changed
- Pi protocol compatibility moved to host (telekinesis); not a rotary feature
- MSRV: 1.88

## [0.2.0] — 2024-12

### Added
- Agent loop with streaming events
- 5 scopes (coding, research, plan, ask, computer_use)
- 7 builtin tools (read, write, edit, bash, grep, find, ls)
- Permission system (Policy + Approver)
- Session tree (fork/merge)
- IPC server (Unix socket JSON-RPC)
- Provider abstraction (OpenAI-compatible)
- Lifecycle hooks
