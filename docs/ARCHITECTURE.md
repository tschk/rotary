# rotary — agent harness engine

## Mission

Be the best **general-purpose coding agent harness**: embeddable, light, extensible, protocol-friendly. Product shells live elsewhere (telekinesis).

## Layers

```
Hosts (telekinesis TUI/CLI, IDEs, CI)
        │  JSON-RPC events (t3code-style typed boundary)
        ▼
rotary control
  agent · tools · permissions · hooks · scopes
  sessions · plugins · skills · guardrails · context
  extract · ranking · slash (parser) · ACP · LSP · IPC
        │
        ▼
providers (HTTP/SSE)  ·  computer-use (rs_peekaboo C ABI)
```

## Influences (feature adoption)

| Project | Adopted in rotary |
|---|---|
| **Codex** | sandbox/approvals → `permissions`; non-interactive headless; skills pattern; bounded tool loops |
| **OpenCode** | multi-provider routing; session resume; plugin/tool surface |
| **t3code** | typed event boundary; thin host protocol over IPC |
| **pi / Crush** | event lifecycle; SKILL.md; hook envelopes |
| **rs_peekaboo** | embedded computer-use primitives |
| **equilibrium** | C FFI generation for Zig consumers of foreign crates |

## Core contracts

### Agent loop

```
before_prompt → (auto-compact) → agent_start → turn
  message stream | tool loop (permission-gated, scope-filtered)
→ after_turn → agent_end
```

### Scopes

Work **scopes** (not named product agents): `coding` | `research` | `plan` | `ask` | `computer_use`.

### Permissions

`full_access` | `read_only` | `workspace_write` | `deny_all` + allow/deny lists + host approver.

### Events

Tagged union pushed to subscribers and mirrored as IPC notifications (`method: "event"`). Hosts must treat events as the UI boundary (t3code pattern).

### FFI

Rust computer-use is reached only through a stable C header (`include/peekaboo.h`). equilibrium regenerates language bindings; Zig code uses `@cImport` or generated wrappers.

## Versioning

Semver for library API. Bump minor for new modules/tools; patch for harness fixes. Hosts pin submodule SHAs.
