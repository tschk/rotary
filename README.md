# rotary (rx4) — the agent harness engine

[![crates.io](https://img.shields.io/crates/v/rotary.svg)](https://crates.io/crates/rotary)
[![License: MPL-2.0](https://img.shields.io/badge/License-MPL--2.0-blue.svg)](LICENSE)
[![MSRV: 1.75](https://img.shields.io/badge/MSRV-1.75-blue.svg)](https://blog.rust-lang.org/2023/12/28/Rust-1.75.0.html)

Pure agent harness engine. Models write; rotary gives them tools, memory,
loops, permissions, sessions, and control planes. **No product UI, no
scheduling policy, no pi protocol** — hosts own those.

rotary exposes **capabilities, not policy**. Scheduling, enabled flags, and
lifecycle decisions are the host's job.

## Architecture

```mermaid
graph TD
  Host["Hosts<br/>telekinesis CLI/TUI · omi desktop · IDEs · CI"]
  Host -->|cargo add rx4| Embed["in-process embed"]
  Host -->|JSON-RPC| Serve["rotary serve (IPC)"]
  Embed --> Engine["rx4 agent harness engine"]
  Serve --> Engine
  Engine --> Loop["agent loop + streaming events"]
  Engine --> Tools["tools + computer-use + MCP"]
  Engine --> Prov["providers (OpenAI/Anthropic/Ollama)"]
  Engine --> Sess["sessions + memory + graph memory"]
  Engine --> Skills["skill engine + curator + background review"]
  Engine --> Ctrl["permissions · hooks · scopes · guardrails"]
```

## Install

```toml
[dependencies]
rx4 = { version = "0.3", features = ["ipc", "builtin-tools", "providers", "computer-use"] }
```

Or via the CLI:

```bash
cargo add rx4 --features ipc,builtin-tools,providers,computer-use
```

## Quick start

```rust
use rx4::{Agent, Scope, ToolRegistry, register_builtin_tools};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut agent = Agent::new();
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);
    agent.set_tools(tools);
    agent.set_scope(Scope::Coding);
    agent.prompt("fix the failing test").await?;
    Ok(())
}
```

## IPC server

```bash
rotary serve /tmp/rotary.sock
```

JSON-RPC methods: `ping`, `state`, `prompt`, `set_model`, `tools`,
`plugins`, `messages`, `session_list`, `session_clear`.

> `rotary serve` starts the Unix socket JSON-RPC server. Hosts connect to
> the socket and drive the agent loop remotely — the host never owns agent
> logic.

## Agent loop

```mermaid
flowchart TD
  Prompt["host calls prompt()"] --> Before["hooks: before_prompt"]
  Before --> Compact{"context full?"}
  Compact -->|yes| Auto["compaction auto-compact"]
  Compact -->|no| Start["AgentStart"]
  Auto --> Start
  Start --> Turn["TurnStart"]
  Turn --> Stream["provider streams message<br/>MessageStart/Delta/End"]
  Stream --> TC{"tool calls?"}
  TC -->|yes| Perm["permissions policy + approver"]
  Perm --> Scope["scope filter"]
  Scope --> Exec["execute tool<br/>ToolExecutionStart/End"]
  Exec --> Guard["guardrails check"]
  Guard --> Turn
  TC -->|no| TE["TurnEnd"]
  TE --> More{"more turns?"}
  More -->|yes| Turn
  More -->|no| End["AgentEnd"]
```

## Features

- **Agent loop with streaming events** — 11 event types (`AgentStart`,
  `TurnStart`, `MessageStart`, `MessageDelta`, `MessageEnd`, `ToolCall`,
  `ToolExecutionStart`, `ToolExecutionEnd`, `TurnEnd`, `AgentEnd`, `Error`).
- **5 scopes** — `coding`, `research`, `plan`, `ask`, `computer_use`.
- **7 builtin tools** — `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`
  (rayon parallel search).
- **13 computer-use tools** (`cu_*`) via
  [rs_peekaboo](https://crates.io/crates/rs_peekaboo) — native Rust, no FFI.
- **MCP client** — JSON-RPC 2.0 over stdio; tools prefixed
  `mcp__{server}__{tool}`.
- **Session tree** — fork/merge with JSONL and SQLite persistence.
- **Permission system** — `Policy` + `Approver`, allow/deny/ask decisions.
- **Lifecycle hooks** — pluggable hook registry around the agent loop.
- **Context compaction** — auto-compact when the context window fills.
- **Skill engine** — self-improving skills with bayesian confidence scoring.
- **Background review** — observes turns, distills learning signals, updates
  skills.
- **Skill curator** — lifecycle management (Active→Stale→Archived),
  consolidation.
- **Embeddings** — semantic skill matching (Gemini / Ollama).
- **Graph memory** — knowledge graph with pagerank and community detection.
- **Dream scheduler** — graph consolidation capability (host schedules).
- **Model router** — tiered routing (`lite`, `standard`, `heavy`, `subagent`).
- **Multi-agent coordination** — coordinator/worker/reviewer/researcher roles.
- **Cost tracking** — per-model pricing registry and session cost accounting.
- **Secret redaction** — pattern-based redaction of secrets and sensitive env vars.
- **Repo map** — pagerank-ranked symbol extraction for codebase context.
- **Prompt caching** — Anthropic ephemeral `cache_control` support.
- **Slash command parsing** — `/command` parsing for host UIs.
- **Guardrails** — empty turn detection and repeated failure detection.
- **Structured extraction** — JSON contracts for typed tool outputs.
- **Subagent manager** — git worktree isolation for parallel subagents.
- **LSP client** — diagnostics, references, definition via Language Server Protocol.
- **ACP host** — Agent Client Protocol host support.
- **Plugin registry + marketplace** — installable plugins and an index.

### Scopes

| Scope | Tools | Policy |
|---|---|---|
| `coding` | FS + shell + find | workspace_write |
| `research` | read-only | read_only |
| `plan` | read-only | read_only |
| `ask` | none | deny_all |
| `computer_use` | rs_peekaboo `cu_*` | full_access |

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

> `pi-compat` and `pi-extensions` have been **removed** — pi protocol
> compatibility now lives in the host (telekinesis).

## Providers

rotary ships a provider abstraction over OpenAI-compatible chat completions
endpoints:

- **OpenAI** — `gpt-4o`, `gpt-4o-mini`, etc.
- **Anthropic** — Claude models via the Anthropic API.
- **Ollama** — local models via `http://localhost:11434`.
- **Custom OpenAI-compatible endpoints** — any server implementing the
  `/v1/chat/completions` schema.

```mermaid
graph TD
  Reg["ProviderRegistry"] --> OpenAI["OpenAI"]
  Reg --> Anthropic["Anthropic (cache_control)"]
  Reg --> Ollama["Ollama (local)"]
  Reg --> Custom["Custom /v1/chat/completions"]
  OpenAI --> SSE["sse.rs stream parser"]
  Anthropic --> SSE
  Ollama --> SSE
  Custom --> SSE
  SSE --> Events["AgentEvent stream"]
  Router["model_router.rs"] --> Reg
  Models["models.rs compat"] --> Reg
```

Use `with_base_url` to point at a custom endpoint:

```rust
use rx4::provider::ProviderRegistry;

let mut registry = ProviderRegistry::new();
registry.register("custom", "my-model", "sk-...")
    .with_base_url("https://my-llm.example.com/v1");
```

## Computer-use

Powered by [rs_peekaboo](https://crates.io/crates/rs_peekaboo) — native
Rust, no FFI:

```toml
rx4 = { version = "0.3", features = ["computer-use"] }
```

13 tools:

| Tool | Description |
|---|---|
| `cu_call` | Invoke a named application method or open a target |
| `cu_see` | Capture a screenshot / visual snapshot of the screen |
| `cu_image` | Encode or transform an image for model input |
| `cu_click` | Click at screen coordinates |
| `cu_type` | Type text into the focused element |
| `cu_hotkey` | Press a keyboard hotkey / key combination |
| `cu_scroll` | Scroll at coordinates or in the focused element |
| `cu_window` | Focus, move, resize, or close a window |
| `cu_app` | Launch or switch to an application |
| `cu_list` | List open windows or running applications |
| `cu_open` | Open a file or URL in the default handler |
| `cu_clipboard` | Read from or write to the system clipboard |
| `cu_doctor` | Diagnose computer-use environment and permissions |

## Events

The agent loop emits 11 streaming event types:

| Event | Description |
|---|---|
| `AgentStart` | The agent loop has started |
| `TurnStart` | A new turn has begun (with turn index) |
| `MessageStart` | A message has started streaming (with role) |
| `MessageDelta` | A streaming text delta |
| `MessageEnd` | A message has finished (with role and full content) |
| `ToolCall` | The model requested a tool call |
| `ToolExecutionStart` | Tool execution has begun |
| `ToolExecutionEnd` | Tool execution has finished (with result) |
| `TurnEnd` | A turn has ended (with turn index) |
| `AgentEnd` | The agent loop has finished |
| `Error` | An error occurred (with message) |

## Hosts

Current hosts built on rotary:

- **[telekinesis](https://github.com/tschk/telekinesis)** — CLI/TUI product
  on top of rotary. Owns pi protocol compat.
- **omi desktop** — desktop application embedding rotary.

See [docs/HOSTS.md](docs/HOSTS.md) for the hosting guide.

## License

MPL-2.0
