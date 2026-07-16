# Roadmap — agent harness engine

## 0.3

- [x] Scopes (coding / research / plan / ask / computer_use)
- [x] Permissions + hooks + compact + slash parser
- [x] Embedded computer-use path (rs_peekaboo + C ABI)
- [x] equilibrium-oriented FFI header + generation docs

## Next

### P0
1. **MCP client (stdio)** — Codex/OpenCode/Crush parity for external tools
2. **Stream-JSON / noninteractive protocol** — Codex noninteractive + CI
3. **Approval UX hooks for hosts** — richer `ask` payloads (Codex approvals)

### P1
4. Tool effects + parallel tool batches  
5. Session export/import (Codex rollout-friendly JSONL)  
6. equilibrium CI: auto-regenerate `src/generated/peekaboo.zig` on header change  

### P2
7. Process sandbox backends (seatbelt/bwrap) exposed as policy plugins  
8. Specialist work packs as data (markdown), not named hard-coded agents  

## Non-goals

- Shipping a TUI (telekinesis)
- Cloud billing / hosted gateway
- Multi-device P2P product features
