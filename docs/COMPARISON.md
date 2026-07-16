# How rotary compares to other agent harnesses

Research snapshot against commonly cited terminals/harnesses. rotary is intentionally a **library-first Zig harness**, not a full TUI product.

| Product | Lang | Loop | Tools | Permissions | Plugins/Skills | Session | Standout |
|---|---|---|---|---|---|---|---|
| **rotary** | Zig 0.16 | pi-style events + hooks | built-ins + registry | full/read_only/workspace_write/deny_all | pi Bun extensions + SKILL.md | tree + fork/merge | embeddable harness, ACP host, IPC |
| [grok-build](https://github.com/xai-org/grok-build) | Rust | TUI/headless/ACP | rich toolkit | product policies | product extensions | JSONL + subagents | Grok-native TUI + ACP |
| [pi_agent_rust](https://github.com/Dicklesworthstone/pi_agent_rust) | Rust | structured concurrency | 8 tools + QuickJS | capability + command mediation | JS extension runtime | JSONL v3 trees | sub-100ms start, tool effects |
| [dot](https://github.com/plyght/dot) | Rust | conversation + subagents | batch + MCP | hooks can block | packages + skills | SQLite + memory FTS | 22 lifecycle hooks, snapshots |
| [codex](https://github.com/openai/codex) | Rust/TS | CLI/app/cloud | OpenAI tool stack | sandbox modes | MCP | cloud + local | multi-surface product |
| [openclaude](https://github.com/Gitlawb/openclaude) | TS | terminal agent | MCP + tools | product | slash/MCP | redis/local | multi-provider CLI |
| [zero](https://github.com/gitlawb/zero) | Go | guardrail-rich loop | sandbox + MCP | rich grants | specialists + skills | JSONL + compact | stream-json CI mode |
| [crush](https://github.com/charmbracelet/crush) | Go | Charm TUI | MCP + LSP | hooks JSON envelope | Agent Skills | sessions | TUI polish + hooks |
| [mods](https://github.com/charmbracelet/mods) | Go | pipeline stdin/stdout | light | N/A | N/A | SHA conversations | sunset; use crush run |

## Features rotary adopted from the field + Omi

| Feature | Source | Module |
|---|---|---|
| Permission modes | Zero / Codex | `permissions.zig` |
| Lifecycle hooks | Dot / Crush | `hooks.zig` |
| Context compaction | Zero / pi / Codex | `context.zig` + `Agent.compact` |
| Project instructions (`AGENTS.md`) | Dot / Zero / Codecs | `context.zig` |
| Slash commands | Omi / Crush / OpenClaude | `slash.zig` |
| Headless `exec` | Zero / mods | `main.zig` |
| Structured extraction | Omi assistants/meeting | `extract.zig` |
| Proactive ranking | Omi `proactive.rs` | `ranking.zig` |
| Guardrails | Zero | `guardrails.zig` |
| Pi plugins + skills | pi / Crush | `plugin.zig` |
| ACP host | Zed / Grok | `acp.zig` |

## Features deliberately deferred

- Full MCP client/server (crush/zero/dot have this; rotary keeps plugin + ACP first)
- QuickJS in-process extensions (pi_agent_rust)
- File snapshot/revert (dot)
- Specialist markdown agents (zero)
- Repo-map/PageRank context (openclaude)
- Product TUI (crush/grok/telekinesis UI layer)

## Design rule

rotary stays **embeddable**. Products (telekinesis, IDEs, CLIs) own UX, multi-device sync, and packaging; rotary owns the agent brain and tool spine.
