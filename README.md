# rotary

**General-purpose agent harness in Zig.**

`Agent = Model + Harness.` The model writes; the harness gives it tools, memory, loops, skills, plugins, permissions, and control surfaces. **rotary** is the harness.

## Why rotary

- **Event-driven agent loop** (pi-compatible lifecycle) with **hooks**
- **Permission modes** (full / read-only / workspace-write / deny-all)
- **Built-in coding tools** + registry
- **Provider gateway** with OpenAI-compatible HTTP + SSE
- **Session trees** (fork / merge)
- **Pi plugins** (Bun + JSONL) and **SKILL.md** skills
- **Context packing**: `AGENTS.md` loading + auto-compact
- **Slash commands** + headless `exec`
- **Omi-inspired extraction + ranking** helpers
- **ACP host**, **LSP manager**, **JSON-RPC IPC**
- **Zig 0.16** — explicit allocators, `std.Io`, single binary friendly

## Quick start

```bash
zig build
zig build test
zig build run -- agent
zig build run -- exec /help
zig build run -- serve
```

Library:

```zig
const rotary = @import("rotary");

var agent = rotary.Agent.init(allocator, io);
defer agent.deinit();
agent.setPolicy(.{ .mode = .workspace_write });

var tools = rotary.ToolRegistry.init(allocator);
defer tools.deinit();
try rotary.tools.registerBuiltins(&tools);
agent.setTools(&tools);

try agent.prompt("Summarize this repository");
```

## Product integration

telekinesis includes rotary as a **git submodule** at `vendor/rotary` and should import the harness rather than re-implementing the loop.

See:
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- [docs/COMPARISON.md](docs/COMPARISON.md) — vs codex, zero, crush, grok-build, dot, pi_agent_rust, openclaude, mods
- [docs/ROADMAP.md](docs/ROADMAP.md)

## Status

| Area | State |
|---|---|
| agent loop + permissions + hooks | solid |
| providers + streaming | solid |
| tools / sessions / plugins | solid |
| extract / ranking / slash / context | solid |
| IPC / LSP | solid |
| ACP | scaffolding |
| MCP | planned |

## License

MIT
