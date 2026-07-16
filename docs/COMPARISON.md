# rotary vs other harnesses

rotary = **engine only**. Telekinesis = product CLI/TUI on top.

| | rotary | Codex | OpenCode | t3code | Crush | pi |
|---|---|---|---|---|---|---|
| Role | harness library | product agent | product agent | multi-provider GUI shell | terminal agent | coding harness |
| Language | Zig | Rust/TS | TS | TS | Go | TS |
| Embeddable | yes | limited | limited | no (host) | limited | package-level |
| Loop + tools | yes | yes | yes | wraps providers | yes | yes |
| Permissions | modes + approver | sandbox + approvals | permissions | via provider | hooks | extensions |
| Sessions | tree fork/merge | threads/rollouts | sessions | multi-session UX | sessions | session tree |
| Computer-use | rs_peekaboo embed | OS features | plugins | N/A | N/A | N/A |
| Host protocol | JSON-RPC IPC | app-server | SDK/HTTP | WebSocket contracts | TUI-native | RPC |

## Taken from Codex specifically

1. **Approvals / sandbox modes** → `permissions.Policy` + host `Approver`
2. **Non-interactive exec** → host `telekinesis exec` / rotary `exec` demo surface
3. **Skills packages** → `plugin` skill loader (SKILL.md)
4. **Bounded context** → compact + auto-compact
5. **Tool failure hygiene** → `guardrails` (empty turns, repeated tool failures)

## Taken from t3code

1. **Typed event boundary** between runtime and UI (IPC `event` notifications)
2. **Thin product shell** — UI never owns agent loop logic
3. **Provider-agnostic host** — swap models without UI rewrites

## Taken from OpenCode

1. Multi-provider model switching mid-session (`set_model`)
2. Durable local sessions
3. Extensibility (plugins / MCP on roadmap)

## Deferred (intentionally host or later)

- Full MCP client (Codex/OpenCode/Crush) — rotary P0 roadmap
- OS sandbox process jail (Codex seatbelt/bwrap) — telekinesis policy later
- Web/desktop multi-client pairing (t3code) — telekinesis product
