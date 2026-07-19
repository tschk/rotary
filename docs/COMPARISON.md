# rotary vs other harnesses

rotary = **engine only**. Telekinesis (CLI/TUI) and omi desktop are product
hosts on top.

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
| Skill engine | bayesian + curator | skill packages | no | no | no | no |
| Cost tracking | per-model registry | no | no | no | no | no |
| Pi compat | moved to host | no | no | no | no | native |
| Host protocol | JSON-RPC IPC | app-server | SDK/HTTP | WebSocket contracts | TUI-native | RPC |

## Taken from Codex specifically

1. **Approvals / sandbox modes** → `permissions.Policy` + host `Approver`
2. **Non-interactive exec** → `rx4 exec` demo surface; hosts can expose their own non-interactive command
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
3. Extensibility (plugins / MCP)

## Deferred (intentionally host or later)

- Web/desktop multi-client pairing (t3code) — telekinesis product
- Pi protocol compat (JSONL v3, RPC, extensions) — telekinesis host

## Implemented in rotary (was deferred)

- OS sandbox process jail via `Policy.enable_os_sandbox` + seatbelt/bwrap (`sandbox` module)
