# rotary roadmap

## Done in 0.2

- Permission policy + approver callback
- Hooks registry (before_prompt/tool/after_tool/compact/error)
- Auto-compact + manual `Agent.compact`
- Project instruction loading (`AGENTS.md` / `CLAUDE.md` / `ROTARY.md`)
- Slash command parser
- Headless `rotary exec`
- Omi-style extraction + ranking helpers
- Guardrails state machine

## Next

### P0
1. **MCP client (stdio)** — standard tool servers, like crush/zero/dot
2. **Tool effects + parallel tool batches** — pi_agent_rust style non-barrier parallel
3. **Stream-JSON protocol** on IPC for CI (`exec` structured output)

### P1
4. **Specialists** — markdown manifests with tool scopes (zero)
5. **Snapshot/revert** for file writes (dot)
6. **Memory store** with FTS archival (dot/Omi gbrain patterns)

### P2
7. **Shell-hook executor** for external lifecycle scripts
8. **Permission grant store** (session + persistent prefixes)
9. **QuickJS or similar in-process plugins** only if Bun overhead becomes an issue

## Non-goals

- Becoming a full TUI product
- P2P multi-device mesh
- Hosted billing/gateway
