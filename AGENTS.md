# rotary (rx4) — agent harness engine

All responses must be in English.

## Position

rotary is a **pure agent harness engine**. It owns the agent loop, tools,
providers, sessions, permissions, computer-use, and IPC. It contains **no
product UI**, **no scheduling policy**, and **no pi protocol compat** (that
moved to the host — telekinesis).

Hosts (telekinesis CLI/TUI, omi desktop, IDEs, CI) embed it via
`cargo add rx4` or connect to `rotary serve` over JSON-RPC IPC.

**Key principle: rotary exposes CAPABILITIES, not POLICY.** Scheduling,
enabled flags, and lifecycle decisions are the host's job. Modules like
`dream_scheduler` and `skill_curator` provide the *capability* to run a
cycle or audit; the host decides *when*.

## Stack

- Rust 2021 (MSRV 1.88), `#![forbid(unsafe_code)]`
- tokio (async runtime, feature-gated)
- serde / serde_json
- rs_peekaboo 0.3.2 (crates.io, computer-use, feature-gated)
- reqwest (providers, feature-gated)
- MPL-2.0

## Architecture

```mermaid
graph TD
  Host["Hosts<br/>telekinesis · omi · IDEs · CI"]
  Host -->|cargo add rx4 / IPC| Loop

  subgraph Loop["agent loop (agent.rs)"]
    direction TB
    Ctx["context.rs<br/>AGENTS.md + system prompt"]
    Mode["mode.rs<br/>scope selection"]
    Prov["provider.rs<br/>OpenAI-compatible SSE"]
    Tools["tools.rs / computer_use.rs<br/>permission-gated, scope-filtered"]
    Guard["guardrails.rs<br/>empty-turn / repeat-failure"]
    Hooks["hooks.rs<br/>lifecycle hooks"]
  end

  Loop --> Persist["session.rs · memory.rs · graph_memory.rs (graph-memory)"]
  Loop --> Skills["skill_engine.rs · skill_curator.rs · background_review.rs (skills)"]
  Loop --> Dream["dream_scheduler.rs (graph-memory) · embeddings.rs (skills)"]
  Loop --> Ctrl["ipc.rs · acp.rs · lsp.rs · mcp.rs · marketplace.rs"]
```

## Module dependency graph

```mermaid
graph LR
  agent --> provider
  agent --> tools
  agent --> session
  agent --> permissions
  agent --> hooks
  agent --> mode
  agent --> context
  agent --> guardrails
  agent --> slash
  agent --> compaction
  agent --> cost
  agent --> secrets
  agent --> skill_engine
  agent --> model_router
  agent --> multiagent
  agent --> subagent
  agent --> routing
  agent --> rollout

  skill_engine --> embeddings
  skill_engine --> background_review
  skill_curator --> skill_engine
  background_review --> skill_engine
  dream_scheduler --> graph_memory
  graph_memory --> memory

  provider --> sse
  provider --> http
  provider --> models
  provider --> prompt_cache
  tools --> computer_use
  ipc --> lsp
  plugin --> marketplace
  context --> repomap
  extract --> ranking
```

## Modules

| File | Responsibility |
|---|---|
| `agent.rs` | event-driven loop, tool registry, streaming, JoinSet parallel tool batches |
| `provider.rs` | multi-provider OpenAI-compatible client, HTTP connection prewarm |
| `tools.rs` | built-in FS/shell/find tools (7) |
| `session.rs` | session tree (fork/merge) + JSONL persistence |
| `permissions.rs` | policy pattern, allow/deny, host approver |
| `hooks.rs` | lifecycle hooks |
| `mode.rs` | scopes (coding/research/plan/ask/computer_use) |
| `context.rs` | AGENTS.md loading, system prompt assembly |
| `slash.rs` | slash command parser |
| `guardrails.rs` | empty-turn detection, repeated-failure detection |
| `extract.rs` | structured extraction (JSON contracts) |
| `ranking.rs` | proactive ranking |
| `computer_use.rs` | rs_peekaboo native integration (no FFI), 13 `cu_*` tools |
| `ipc.rs` | Unix socket JSON-RPC server |
| `config.rs` | config file + env |
| `plugin.rs` | plugin registry |
| `acp.rs` | ACP host |
| `lsp.rs` | LSP manager (diagnostics, references, definition) |
| `skill_engine.rs` | skill engine (Beta-Binomial confidence, keyword/semantic activate) — `skills` feature |
| `background_review.rs` | background review loop — heuristic learning signals from turns — `skills` feature |
| `skill_curator.rs` | skill lifecycle curator — Active→Stale→Archived, consolidation — `skills` feature |
| `dream_scheduler.rs` | dream cycle runner — graph consolidation capability (host schedules) — `graph-memory` feature |
| `embeddings.rs` | vector embeddings for semantic skill matching (Gemini / Ollama) — `skills` feature |
| `graph_memory.rs` | knowledge graph (pagerank, community detection, dream consolidation) — `graph-memory` feature |
| `memory.rs` | SQLite persistent memory store |
| `compaction.rs` | context compaction (auto-compact) |
| `cost.rs` | cost tracking (per-model pricing) |
| `secrets.rs` | secret redaction (pattern-based) |
| `sse.rs` | SSE stream parser |
| `marketplace.rs` | plugin marketplace index + installer |
| `http.rs` | reqwest HTTP client (providers feature) |
| `mcp.rs` | MCP client (JSON-RPC 2.0 over stdio) |
| `model_router.rs` | tiered model routing (lite/standard/heavy/subagent) |
| `models.rs` | model registry + compat config |
| `multiagent.rs` | multi-agent coordination (coordinator/worker/reviewer/researcher) |
| `prompt_cache.rs` | Anthropic ephemeral cache_control (auto on Anthropic provider stream) |
| `repomap.rs` | pagerank-ranked symbol extraction (host API) |
| `rollout.rs` | rollout tracking + trace writer (host API) |
| `routing.rs` | smart routing (host API; not auto inside Agent loop) |
| `sandbox.rs` | userspace SandboxManager + optional OsSandboxRunner (seatbelt/bwrap) |
| `subagent.rs` | subagent management (git worktree isolation) |

Host opt-in on `Agent` (0.3.5+): `set_skill_registry`, `set_graph_memory`,
`enable_os_sandbox` / `set_os_sandbox`, `set_sandbox`. Model router / multiagent
/ cost are library APIs — hosts choose when to call them. |

## Agent loop

```mermaid
flowchart TD
  Prompt["host calls prompt()"] --> Before["hooks: before_prompt"]
  Before --> Compact{"context full?"}
  Compact -->|yes| AutoCompact["compaction.rs auto-compact"]
  Compact -->|no| Start["event: AgentStart"]
  AutoCompact --> Start
  Start --> Turn["event: TurnStart"]
  Turn --> Msg["provider.rs streams message<br/>event: MessageStart/Delta/End"]
  Msg --> ToolCall{"tool calls?"}
  ToolCall -->|yes| Perm["permissions.rs policy + approver"]
  Perm --> Scope["mode.rs scope filter"]
  Scope --> Exec["execute tool<br/>event: ToolExecutionStart/End"]
  Exec --> Guard["guardrails.rs check"]
  Guard --> Turn
  ToolCall -->|no| TurnEnd["event: TurnEnd"]
  TurnEnd --> More{"more turns?"}
  More -->|yes| Turn
  More -->|no| End["event: AgentEnd"]
```

## Skill lifecycle

```mermaid
flowchart LR
  Create["skill_engine.rs<br/>create from conversation"] --> Active[("Active")]
  Active -->|background_review detects gap/outdated| Update["update instructions<br/>adjust confidence"]
  Update --> Active
  Active -->|idle 30d| Curator["skill_curator.rs audit"]
  Curator -->|stale| Stale[("Stale")]
  Stale -->|idle 90d| Archived[("Archived")]
  Curator -->|consolidate| Merge["merge into umbrella skill"]
  Pinned["pinned skill"] -.bypass.-> Active
```

## Session tree

```mermaid
graph TD
  Root["session root"] --> A["turn 1"]
  A --> B["turn 2"]
  B -->|fork| C["branch A · turn 3"]
  B -->|fork| D["branch B · turn 3"]
  C --> E["turn 4"]
  D -->|merge back| B
  Root -->|compact| Summary["compaction summary<br/>subtree re-parented"]
```

## Provider abstraction

```mermaid
graph TD
  subgraph Provider["provider.rs"]
    Reg["ProviderRegistry"]
    Reg --> OpenAI["OpenAI<br/>gpt-4o, gpt-4o-mini"]
    Reg --> Anthropic["Anthropic<br/>Claude (cache_control)"]
    Reg --> Ollama["Ollama<br/>local models"]
    Reg --> Custom["Custom<br/>any /v1/chat/completions"]
  end
  OpenAI --> SSE["sse.rs stream parser"]
  Anthropic --> SSE
  Ollama --> SSE
  Custom --> SSE
  SSE --> Events["AgentEvent stream"]
  Models["models.rs<br/>compat config"] --> Reg
  Router["model_router.rs<br/>lite/standard/heavy/subagent"] --> Reg
  Cache["prompt_cache.rs<br/>ephemeral cache_control"] --> Anthropic
```

## Feature flags

| Feature | Default | Enables |
|---|---|---|
| `ipc` | yes | tokio runtime, Unix socket JSON-RPC server, LSP client |
| `builtin-tools` | yes | read/write/edit/bash/grep/find/ls with rayon parallel search |
| `computer-use` | no | rs_peekaboo `cu_*` tools (13 tools) |
| `providers` | no | reqwest SSE streaming for OpenAI/Anthropic/Ollama/custom |
| `memory` | no | SQLite-backed memory store |
| `mcp` | no | MCP client (rmcp, JSON-RPC 2.0 over stdio) |
| `sqlite-sessions` | no | SQLite session persistence |
| `skills` | no | skill engine, skill curator, background review, embeddings (serde_yaml + dirs) |
| `graph-memory` | no | graph memory (pagerank, community detection), dream scheduler |

> `pi-compat` and `pi-extensions` have been **removed** — pi protocol
> compatibility (JSONL v3 sessions, RPC, extensions, QuickJS) now lives in
> the host (telekinesis).

## Rules

- No hard-coded API keys, no telemetry.
- New agent features land in rotary first, then surface to hosts via IPC/slash.
- computer-use uses the crates.io `rs_peekaboo` dependency — no vendoring, no FFI.
- A scope is a work mode, not an agent name.
- rotary exposes capabilities, not policy — scheduling and lifecycle decisions belong to the host.
- Keep MPL-2.0.

## Validation

```bash
cargo build
cargo build --all-features
cargo test --all-features
cargo clippy --all-features
cargo fmt --check
```

## Commits

English Conventional Commits, e.g. `feat(agent): stream tool call deltas`.
