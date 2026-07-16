# rotary

**General-purpose agent harness in Zig.**

`Agent = Model + Harness.` The model writes; the harness gives it tools, memory, loops, skills, plugins, and control surfaces. **rotary** is the harness — extracted from the telekinesis agent core and refined as a standalone library + CLI.

## Why rotary

- **Event-driven agent loop** (pi-compatible lifecycle): `before_agent_start` → turns → streaming messages → tool execution → end.
- **Built-in coding tools**: read/write files, list dirs, shell, find, subagent spawn, LSP code intel.
- **Provider gateway**: OpenAI-compatible HTTP + SSE streaming for OpenAI, Anthropic, Gemini, local Ollama, custom base URLs.
- **Session trees**: fork / merge / parent-linked messages for branching conversations.
- **Pi plugins**: Bun subprocess + JSONL/JSON-RPC so existing TypeScript pi extensions can load without a rewrite.
- **Skills**: passive `SKILL.md` packages injected into the system prompt.
- **ACP host**: spawn external agents over the Agent Client Protocol.
- **IPC server**: JSON-RPC Unix socket for TUI/GUI/IDE shells.
- **Zig 0.16**: explicit allocators, `std.Io`, single static binary friendly.

## Not rotary

rotary is **not** a multi-surface product UI, not a P2P mesh, and not a hoster of cloud billing. Those remain product layers (e.g. telekinesis). rotary is the reusable engine underneath.

## Layout

```
rotary/
  src/
    root.zig      library surface
    main.zig      CLI
    agent.zig     event loop + tool registry
    provider.zig  LLM HTTP client
    tools.zig     built-in tools
    session.zig   session tree + persistence
    plugin.zig    pi extensions + skills
    lsp.zig       language server manager
    acp.zig       ACP host
    ipc.zig       JSON-RPC server
    config.zig    config + env
    db.zig        optional JSON-RPC SQLite helper client
  plugins/        pi shim + example extension
  docs/           architecture
```

## Build

Requires Zig **0.16.0+**.

```bash
zig build
zig build test
zig build run -- agent
zig build run -- provider
zig build run -- serve
```

Install:

```bash
zig build -Doptimize=ReleaseSafe
# binary at zig-out/bin/rotary
```

## Quick start (library)

```zig
const rotary = @import("rotary");

var agent = rotary.Agent.init(allocator, io);
defer agent.deinit();

var tools = rotary.ToolRegistry.init(allocator);
defer tools.deinit();
try rotary.tools.registerBuiltins(&tools);
agent.setTools(&tools);

var client = rotary.ProviderClient.init(allocator, io);
// configure provider + keys via rotary.config / env
agent.setProvider(&client, provider_config);
try agent.subscribe(null, onEvent);
try agent.prompt("Summarize this repository");
```

## Quick start (CLI)

```bash
export OPENAI_API_KEY=...
rotary serve                    # IPC at /tmp/rotary.sock
rotary agent                    # offline loop demo
rotary plugin                   # plugin registry
```

Config defaults to `~/.rotary/config.json` with env overrides:
`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `ROTARY_PROVIDER`, `ROTARY_MODEL`.

## Influences

| Project | Contribution |
|---|---|
| [pi](https://github.com/badlogic/pi-mono) | Event loop, skills, TypeScript extensions |
| [Zed ACP](https://agentclientprotocol.com) | External agent protocol |
| [t3code](https://github.com/pingdotgg/t3code) | Typed event boundary for UIs |
| [OpenCode](https://github.com/anomalyco/opencode) / [Crush](https://github.com/charmbracelet/crush) | Provider + tools patterns |
| telekinesis | Original multi-surface host that incubated this core |

## Status

| Module | State |
|---|---|
| agent loop | solid |
| providers + streaming | solid |
| tools | solid |
| sessions | solid |
| plugins + skills | solid |
| IPC | solid |
| LSP | solid |
| ACP | scaffolding |
| DB helper | optional client; JSONL fallback |

## License

MIT
