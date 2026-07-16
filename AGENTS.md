# rotary agent guide

## Purpose

rotary is a **general-purpose coding agent harness** implemented in Zig 0.16. It owns the agent loop, tools, providers, sessions, plugins, ACP, LSP, and IPC. Product UIs and multi-device meshes live elsewhere.

## Stack

- Zig 0.16 (`std.Io`, juicy main, explicit allocators)
- `httpx` for provider HTTP
- Optional Bun for pi-compatible TypeScript extensions
- Optional language servers via `lsp.zig`

## Module map

| File | Role |
|---|---|
| `agent.zig` | Event-driven loop, tool registry, streaming |
| `provider.zig` | Multi-provider OpenAI-compatible client |
| `tools.zig` | Built-in filesystem / shell / subagent / code_intel tools |
| `session.zig` | Session tree (fork/merge) + store |
| `plugin.zig` | Pi extension host + skill loader |
| `lsp.zig` | Multi-language LSP manager |
| `acp.zig` | External agent processes |
| `ipc.zig` | Unix-socket JSON-RPC server |
| `config.zig` | Config file + env |
| `db.zig` | Optional SQLite helper process client |

## Rules

- Pass allocators explicitly. No global allocator.
- Use `std.log.scoped(.module)` per file.
- Prefer explicit error sets; avoid swallowing failures.
- Keep plugins as Bun subprocesses (pi compatibility); do not invent an in-process TS runtime.
- ACP is for **external agents**; `plugin.zig` is for **extensions**. Do not merge those protocols.
- Data dir default: `~/.rotary`. Session socket default: `/tmp/rotary.sock`.
- Do not hardcode API keys or add outbound telemetry.
- Prefer stdlib; only add deps that are mature and pure-Zig when possible.

## Verification

```bash
zig build
zig build test
zig build run -- agent
zig build run -- provider
```

## Commits

English Conventional Commits, e.g. `feat(agent): stream tool call deltas`.
