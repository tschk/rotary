# rotary architecture

## Goal

Provide the best general-purpose **agent harness** in Zig:

- Host any LLM that speaks OpenAI-compatible chat (or dedicated adapters).
- Give the model tools, sessions, skills, plugins, permissions, hooks, and observability events.
- Remain embeddable as a library **or** runnable as a CLI/IPC daemon.
- Stay product-agnostic: no multi-device P2P, no UI framework, no billing.

## Layers

```
┌──────────────────────────────────────────┐
│ Host products (TUI / GUI / IDE / cloud)  │
│ telekinesis, editors, CI runners         │
└──────────────────┬───────────────────────┘
                   │ JSON-RPC IPC / library API
┌──────────────────▼───────────────────────┐
│ Control                                      │
│ agent · tools · plugins · skills · sessions  │
│ permissions · hooks · slash · guardrails     │
│ extract · ranking · context · acp · lsp      │
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
before_prompt hook
  → auto-compact if needed
  → before_agent_start
  → agent_start
    → turn_start
      → message_start / message_update* / message_end
      → before_tool hook + permission gate
      → tool_call → tool_execution_* → tool_result
    → turn_end / after_turn hook
  → agent_end
```

- Streaming path when no tools are registered.
- Tool-call path uses non-streaming completions and iterates until the model stops calling tools (`max_tool_iterations`, default 20).
- Subscribers observe every event for UIs, logging, and plugins.

## Permissions

`permissions.Policy` modes:

| Mode | Behavior |
|---|---|
| `full_access` | allow all tools |
| `read_only` | allow reads; `ask` on writes/shell |
| `workspace_write` | allow workspace FS; `ask` on shell |
| `deny_all` | deny unless allowlisted |

`ask` decisions go to an optional host `Approver` callback; without one they become `deny`.

## Context

- Load project instructions from `AGENTS.md`, `CLAUDE.md`, `ROTARY.md`, `.rotary/instructions.md`
- Compact long histories by keeping first N + last M messages with a summary placeholder
- Auto-compact when message count exceeds `Agent.auto_compact_after` (default 80)

## Omi-inspired helpers

- `extract.zig` — robust JSON block parsing + task/memory/goal extraction contracts
- `ranking.zig` — proactive ranking for tasks/insights/calendar-style events
- `slash.zig` — `/model`, `/compact`, `/permissions`, `/tools`, `/new`

## Tools

`ToolRegistry` maps name → `ToolDefinition` with JSON Schema parameters and an execute callback.

Built-ins (`tools.zig`): `read_file`, `write_file`, `list_dir`, `run_command`, `find_files`, `spawn_agent`, `code_intel`.

## Plugins and skills

- **Extensions**: TypeScript/JS under Bun, JSONL over stdio, shim maps pi `ExtensionAPI` onto rotary host messages.
- **Skills**: `SKILL.md` markdown packages injected into the system prompt as `<available_skills>`.

## Sessions

In-memory session tree with `parent_id` for fork/branch. Optional persistence via JSONL store or SQLite helper process (`db.zig`).

## ACP

Host spawns external agent binaries and speaks Agent Client Protocol JSON-RPC so Claude Code / Codex / OpenCode-style agents can be subordinated under rotary.

## Embedding in products

telekinesis vendors rotary as `vendor/rotary` and should depend on the `rotary` module for agent/provider/tools/session/plugin/ipc APIs, keeping only product code (UI, P2P `net.zig`, packaging).

```zig
const rotary = @import("rotary");
var agent = rotary.Agent.init(allocator, io);
agent.setPolicy(.{ .mode = .workspace_write });
```

## Why Zig

- One self-contained binary for the harness daemon.
- Explicit memory ownership for long-running agent processes.
- `std.Io` 0.16 I/O surface matches network + process workloads cleanly.
- No GC pauses during tool/stream churn.
