# rotary agent guide

## Purpose

rotary is a **general-purpose coding agent harness** implemented in Zig 0.16. It owns the agent loop, tools, providers, sessions, plugins, permissions, hooks, ACP, LSP, and IPC. Product UIs and multi-device meshes live elsewhere (e.g. telekinesis).

## Stack

- Zig 0.16 (`std.Io`, juicy main, explicit allocators)
- `httpx` for provider HTTP
- Optional Bun for pi-compatible TypeScript extensions
- Optional language servers via `lsp.zig`

## Module map

| File | Role |
|---|---|
| `agent.zig` | Event-driven loop, tools, permissions, hooks, compact |
| `provider.zig` | Multi-provider OpenAI-compatible client |
| `tools.zig` | Built-in FS/shell/subagent/code_intel tools |
| `session.zig` | Session tree + store |
| `plugin.zig` | Pi extensions + skills |
| `permissions.zig` | Policy modes + approver gate |
| `hooks.zig` | Lifecycle hooks |
| `context.zig` | AGENTS.md load + compaction |
| `slash.zig` | Slash command parser |
| `extract.zig` | Structured extraction (Omi-style) |
| `ranking.zig` | Proactive ranking (Omi-style) |
| `guardrails.zig` | Empty-turn / failure tripwires |
| `lsp.zig` / `acp.zig` / `ipc.zig` | Language servers, external agents, daemon |
| `config.zig` / `db.zig` | Config + optional SQLite helper |

## Rules

- Pass allocators explicitly. No global allocator.
- Use `std.log.scoped(.module)` per file.
- Prefer explicit error sets.
- Keep plugins as Bun subprocesses (pi compatibility).
- ACP = external agents; `plugin.zig` = extensions. Do not merge.
- Data dir default: `~/.rotary`. Socket: `/tmp/rotary.sock`.
- No hardcoded API keys, no telemetry.
- Prefer stdlib; deps must be mature and pure-Zig when possible.

## Verification

```bash
zig build
zig build test
zig build run -- agent
zig build run -- exec /help
zig build run -- provider
```

## Commits

English Conventional Commits, e.g. `feat(agent): gate tools through permission policy`.
