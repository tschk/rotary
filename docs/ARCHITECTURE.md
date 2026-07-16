# rotary architecture

## Goal

Provide the best general-purpose **agent harness** in Zig:

- Host any LLM that speaks OpenAI-compatible chat (or dedicated adapters).
- Give the model tools, sessions, skills, plugins, and observability events.
- Remain embeddable as a library **or** runnable as a CLI/IPC daemon.
- Stay product-agnostic: no multi-device P2P, no UI framework, no billing.

## Layers

```
┌──────────────────────────────────────────┐
│ Host products (TUI / GUI / IDE / cloud)  │
│ — not part of rotary                     │
└──────────────────┬───────────────────────┘
                   │ JSON-RPC IPC / library API
┌──────────────────▼───────────────────────┐
│ Control                                      │
│ agent · tools · plugins · skills · sessions  │
│ acp · lsp · ipc                              │
└──────────────────┬───────────────────────┘
                   │ HTTP / SSE
┌──────────────────▼───────────────────────┐
│ Providers                                    │
│ OpenAI · Anthropic · Gemini · local · custom │
└──────────────────────────────────────────────┘
```

## Agent loop

Lifecycle (pi-aligned):

```
before_agent_start
  → agent_start
    → turn_start
      → message_start / message_update* / message_end
      → tool_call → tool_execution_* → tool_result
    → turn_end
  → agent_end
```

- Streaming path when no tools are registered.
- Tool-call path uses non-streaming completions and iterates until the model stops calling tools (cap: 20).
- Subscribers observe every event for UIs, logging, and plugins.

## Tools

`ToolRegistry` maps name → `ToolDefinition` with JSON Schema parameters and an execute callback.

Built-ins (`tools.zig`): `read_file`, `write_file`, `list_dir`, `run_command`, `find_files`, `spawn_agent`, `code_intel`.

## Plugins and skills

- **Extensions**: TypeScript/JS under Bun, JSONL over stdio, shim maps pi `ExtensionAPI` onto rotary host messages.
- **Skills**: `SKILL.md` markdown packages (Agent Skills style) injected into the system prompt as `<available_skills>`.

## Sessions

In-memory session tree with `parent_id` for fork/branch. Optional persistence via JSONL store or SQLite helper process (`db.zig`).

## ACP

Host spawns external agent binaries and speaks Agent Client Protocol JSON-RPC so Claude Code / Codex / OpenCode-style agents can be subordinated under rotary.

## Why Zig

- One self-contained binary for the harness daemon.
- Explicit memory ownership for long-running agent processes.
- `std.Io` 0.16 I/O surface matches network + process workloads cleanly.
- No GC pauses during tool/stream churn.

## Boundary with telekinesis

telekinesis remains a multi-surface product (UI, P2P QUIC mesh, Crepuscularity shells). rotary is the extracted harness it incubated. New products should depend on rotary rather than telekinesis internals.
