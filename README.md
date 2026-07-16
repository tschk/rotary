# rotary (rx4) — The agent harness engine

[![crates.io](https://img.shields.io/crates/v/rotary.svg)](https://crates.io/crates/rotary)
[![License: MPL-2.0](https://img.shields.io/badge/License-MPL--2.0-blue.svg)](LICENSE)
[![MSRV: 1.75](https://img.shields.io/badge/MSRV-1.75-blue.svg)](https://blog.rust-lang.org/2023/12/28/Rust-1.75.0.html)

Pure agent harness engine. Models write; rotary gives them tools, memory, loops, permissions, sessions, and control planes. Hosts (CLIs, TUIs, IDEs, desktop apps) embed rotary.

## Install

```toml
[dependencies]
rotary = { version = "0.3", features = ["ipc", "builtin-tools", "pi-compat", "providers", "computer-use"] }
```

Or via the CLI:

```bash
cargo add rotary --features ipc,builtin-tools,pi-compat,providers,computer-use
```

## Quick start

```rust
use rotary::{Agent, Scope, ToolRegistry, register_builtin_tools};

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

JSON-RPC methods: `ping`, `state`, `prompt`, `set_model`, `tools`, `plugins`, `messages`, `session_list`, `session_clear`.

> `rotary serve` starts the Unix socket JSON-RPC server. Hosts connect to the socket and drive the agent loop remotely — the host never owns agent logic.

## Features

- **Agent loop with streaming events** — 11 event types (`AgentStart`, `TurnStart`, `MessageStart`, `MessageDelta`, `MessageEnd`, `ToolCall`, `ToolExecutionStart`, `ToolExecutionEnd`, `TurnEnd`, `AgentEnd`, `Error`).
- **5 scopes** — `coding`, `research`, `plan`, `ask`, `computer_use` (see table below).
- **7 builtin tools** — `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls` (rayon parallel search).
- **13 computer-use tools** (`cu_*`) via [rs_peekaboo](https://crates.io/crates/rs_peekaboo) — native Rust, no FFI.
- **MCP client** — JSON-RPC 2.0 over stdio; tools prefixed `mcp__{server}__{tool}`.
- **Pi protocol compatibility** — JSONL v3 sessions, RPC over stdin/stdout, pi tool name mapping, extension protocol via QuickJS, capability policy, SDK surface.
- **Session tree** — fork/merge with JSONL and SQLite persistence.
- **Permission system** — `Policy` + `Approver`, allow/deny/ask decisions.
- **Lifecycle hooks** — pluggable hook registry around the agent loop.
- **Context compaction** — auto-compact when the context window fills.
- **Skill engine** — self-improving skills with bayesian confidence scoring.
- **Graph memory** — knowledge graph with pagerank and community detection.
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
| `pi-compat` | yes | pi JSONL v3 sessions, RPC, tool name mapping, capability policy |
| `computer-use` | no | rs_peekaboo `cu_*` tools (13 tools) |
| `providers` | no | reqwest SSE streaming for OpenAI/Anthropic/Ollama/custom |
| `memory` | no | SQLite-backed memory store |
| `mcp` | no | MCP client (rmcp, JSON-RPC 2.0 over stdio) |
| `sqlite-sessions` | no | SQLite session persistence |
| `pi-extensions` | no | pi extension protocol with QuickJS runtime |

## Providers

rotary ships a provider abstraction over OpenAI-compatible chat completions endpoints:

- **OpenAI** — `gpt-4o`, `gpt-4o-mini`, etc.
- **Anthropic** — Claude models via the Anthropic API.
- **Ollama** — local models via `http://localhost:11434`.
- **Custom OpenAI-compatible endpoints** — any server implementing the `/v1/chat/completions` schema.

Use `with_base_url` to point at a custom endpoint:

```rust
use rx4::provider::ProviderRegistry;

let mut registry = ProviderRegistry::new();
registry.register("custom", "my-model", "sk-...")
    .with_base_url("https://my-llm.example.com/v1");
```

## Computer-use

Powered by [rs_peekaboo](https://crates.io/crates/rs_peekaboo) — native Rust, no FFI:

```toml
rotary = { version = "0.3", features = ["computer-use"] }
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

## Comparison

| | rotary | Codex | OpenCode | t3code | Crush | pi |
|---|---|---|---|---|---|---|
| Role | harness library | product agent | product agent | multi-provider GUI shell | terminal agent | coding harness |
| Language | Rust | Rust/TS | TS | TS | Go | TS |
| Embeddable | yes | limited | limited | no (host) | limited | package-level |
| Loop + tools | yes | yes | yes | wraps providers | yes | yes |
| Permissions | modes + approver | sandbox + approvals | permissions | via provider | hooks | extensions |
| Sessions | tree fork/merge | threads/rollouts | sessions | multi-session UX | sessions | session tree |
| Computer-use | rs_peekaboo embed | OS features | plugins | N/A | N/A | N/A |
| MCP client | yes | yes | yes | no | yes | no |
| Memory | graph + SQLite | context window | sessions | via provider | sessions | session tree |
| Skill engine | bayesian | skill packages | no | no | no | no |
| Cost tracking | per-model registry | no | no | no | no | no |
| Pi compat | yes | no | no | no | no | native |
| Host protocol | JSON-RPC IPC | app-server | SDK/HTTP | WebSocket contracts | TUI-native | RPC |

See [docs/COMPARISON.md](docs/COMPARISON.md) for the full breakdown.

## Hosts

Current hosts built on rotary:

- **[telekinesis](https://github.com/tschk/telekinesis)** — CLI/TUI product on top of rotary.
- **omi desktop** — desktop application embedding rotary.

## License

MPL-2.0
