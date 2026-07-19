# Changelog

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
