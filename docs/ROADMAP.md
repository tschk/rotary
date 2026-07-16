# Roadmap — agent harness engine

## 0.3

- [x] Scopes (coding / research / plan / ask / computer_use)
- [x] Permissions + hooks + compact + slash parser
- [x] Embedded computer-use path (rs_peekaboo, no FFI)
- [x] MCP client (stdio)
- [x] Skill engine + background review + skill curator
- [x] Embeddings (Gemini / Ollama) for semantic skill matching
- [x] Dream scheduler (graph consolidation capability)
- [x] Pi protocol compat removed from rotary → moved to telekinesis host

## Next

### P0
1. **Stream-JSON / noninteractive protocol** — Codex noninteractive + CI
2. **Approval UX hooks for hosts** — richer `ask` payloads (Codex approvals)

### P1
3. Tool effects + parallel tool batches
4. Session export/import (Codex rollout-friendly JSONL)
5. Consolidation auto-run wiring (host-driven, capability already exists)

### P2
6. Process sandbox backends (seatbelt/bwrap) exposed as policy plugins
7. Specialist work packs as data (markdown), not named hard-coded agents

## Non-goals

- Shipping a TUI (telekinesis)
- Cloud billing / hosted gateway
- Multi-device P2P product features
- Pi protocol compat (owned by telekinesis host)
- Scheduling policy (host's job — rotary only exposes capabilities)
