# Changelog

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
- Pi protocol compatibility (JSONL v3, RPC, tool mapping, extensions)
- Memory module (SQLite-backed)
- Graph memory (knowledge graph with pagerank)
- Skill engine (self-improving, bayesian confidence)
- Model router (tiered routing)
- Multi-agent coordination (coordinator/worker/reviewer/researcher)
- Cost tracking (per-model pricing)
- Secret redaction
- Repo map (pagerank-ranked symbols)
- Prompt caching (Anthropic ephemeral)
- Guardrails (empty turn, repeated failure detection)
- Subagent manager (git worktree isolation)
- LSP client
- ACP host
- Plugin registry + marketplace
- Context compaction (auto-compact)
- Structured extraction (JSON contracts)
- Slash command parsing

### Changed
- Default features now include pi-compat
- MSRV: 1.75

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
