# Features and flags

rotary exposes **capabilities, not policy**. Scheduling, enabled flags, and lifecycle decisions are the host's job.

## Capabilities

- **Agent loop with streaming events** — see [EVENTS.md](EVENTS.md).
- **5 scopes** — `coding`, `research`, `plan`, `ask`, `computer_use`.
- **7 builtin tools** — `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls` (stdlib walk + regex; opt-in `fff` for indexed search).
- **13 computer-use tools** (`cu_*`) via [Praefectus](https://crates.io/crates/praefectus) — native Rust, no FFI. See [COMPUTER-USE.md](COMPUTER-USE.md).
- **MCP client** — JSON-RPC 2.0 over stdio; tools prefixed `mcp__{server}__{tool}`.
- **Session tree** — fork/merge with JSONL persistence; optional SQLite via `sqlite-sessions` (`save_sqlite` / `load_sqlite`); Codex-friendly `export_codex_jsonl` / `import_codex_jsonl`.
- **Work packs** (`work-pack`) — specialist agent profiles as markdown data (`WorkPack`).
- **Stream-JSON CLI** — `rx4 exec --stream-json` emits NDJSON agent events.
- **Permission system** — `Policy` + `Approver`; `Policy::default()` and `Agent::new` use `workspace_write` (process tools require approval). `Policy.enable_os_sandbox` enables seatbelt/bwrap as a policy plugin. Hosts receive `Event::ApprovalRequired` with a rich `ApprovalRequest`.
- **Lifecycle hooks** — pluggable hook registry around the agent loop.
- **Context compaction** — token-estimate auto-compact via `estimate_messages` + `apply_compaction`. Projection compaction (`PrefixShape`, prune-then-fold, Raven JSONL archive) keeps the session append-only. See [PROJECTION.md](PROJECTION.md).
- **Parallel tool batches** — `JoinSet` for Read/Network; Write/Process serial.
- **Skill engine** (`skills`) — Beta-Binomial confidence; keyword + optional embedding activation. Host opt-in: `Agent::set_skill_registry` injects matching skill instructions into the system prompt each turn.
- **Background review** (`skills`) — heuristic learning signals. Host opt-in: `Agent::set_skill_engine` runs `BackgroundReviewer` after each `prompt`. (Manual `BackgroundReviewer` still available for custom schedules.)
- **Skill curator** (`skills`) — Active→Stale→Archived; host schedules audits.
- **Embeddings** (`skills` + `providers`) — Gemini / Ollama semantic matching.
- **Graph memory** (`graph-memory`) — pagerank + community detection. Host opt-in: `Agent::set_graph_memory` extracts nodes/edges after each run.
- **Dream scheduler** (`graph-memory`) — consolidation capability; host opt-in `Agent::enable_auto_dream(true)` runs one cycle after graph extract.
- **Autoresearch** — the legacy opt-in session tools persist `.auto/` metadata; `AutoresearchController` adds detached Git worktrees, checkpoint rollback, warmups/median aggregation, required guards, budgets, append-only typed events, and explicit final-patch acceptance. It is host-driven and never mutates the real checkout automatically. See [AUTORESEARCH.md](AUTORESEARCH.md).
- **Model router / multi-agent / cost / repo map / rollout** — library APIs for hosts; not auto-selected inside `Agent::prompt`.
- **Secret redaction** — pattern-based redaction applied to tool results.
- **Prompt caching** — Anthropic `cache_control` applied automatically on `OpenAIProvider` stream bodies when `provider_id == "anthropic"`; provider usage is surfaced through `Event::Usage` and `Agent::cache_stats()`.
- **Context discipline** — tool definitions are serialized in stable name order, and `ToolRegistry::definitions_fingerprint()` lets hosts detect loadout changes that can invalidate a cached prompt prefix.
- **OS sandbox** — optional seatbelt/bwrap wrap for bash via `Agent::enable_os_sandbox` (userspace `SandboxManager` still separate).
- **Slash command parsing** — `/command` parsing for host UIs.
- **Guardrails** — empty turn detection, repeated failure detection, tool-effect batch planning.
- **Structured extraction** — JSON contracts for typed tool outputs.
- **Subagent manager** — optional provider-driven `Agent::prompt` runs with workspace isolation directories.
- **LSP client** — diagnostics, references, definition via Language Server Protocol.
- **ACP host** — JSON-RPC session/prompt surface over an embedded agent.
- **Plugin registry + marketplace** — install with required sha256, blocklist, sanitized names; registry loads installed plugins.

## Scopes

| Scope | Tools | Policy |
|---|---|---|
| `coding` | FS + shell + find | workspace_write |
| `research` | read-only | read_only |
| `plan` | read-only | read_only |
| `ask` | none | deny_all |
| `computer_use` | Praefectus `cu_*` | full_access |

A scope is a work mode, not an agent name.

## Feature flags

| Feature | Default | Enables |
|---|---|---|
| `builtin-tools` | yes | read/write/edit/bash/grep/find/ls (stdlib + regex) |
| `fff` | no | fff-search indexed grep/find |
| `cli` | no | clap + `rx4` binary |
| `ipc` | no | Unix socket JSON-RPC, ACP, LSP (cancellation is always on) |
| `computer-use` | no | Praefectus `cu_*` tools (13 tools) |
| `providers` | no | reqwest SSE streaming for OpenAI/Anthropic/Ollama/custom |
| `memory` | no | SQLite-backed memory store |
| `mcp` | no | MCP client + `McpServerConfig` |
| `marketplace` | no | plugin installer (implies `mcp`) |
| `sqlite-sessions` | no | SQLite session save/load on `Session` |
| `skills` | no | skill engine, curator, background review, embeddings |
| `graph-memory` | no | graph memory, dream scheduler |
| `zkr-memory` | no | zkr-backed memory |
| `personality` | no | personality (implies `zkr-memory`) |
| `autoresearch` | no | AutoresearchSession + AutoresearchController |
| `routing` | no | ModelRouter + SmartRouter |
| `extract` | no | extract + ranking |
| `work-pack` | no | WorkPack markdown profiles |
| `sse` | no | hand-rolled SseParser |
| `multiagent` | no | MultiAgentCoordinator |

> `pi-compat` and `pi-extensions` have been **removed** — pi protocol compatibility now lives in the host (telekinesis).
