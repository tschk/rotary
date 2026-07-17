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
3. [x] Tool effects + parallel tool batches (`JoinSet`, 0.3.3+)
4. Session export/import (Codex rollout-friendly JSONL)
5. Consolidation auto-run wiring (host-driven, capability already exists)
6. [x] Skills/graph host hooks on `Agent` (`set_skill_registry` / `set_graph_memory`, 0.3.5)
7. [x] Prompt-cache on Anthropic stream path (0.3.5)
8. [x] OS sandbox wrap for bash (`enable_os_sandbox` seatbelt/bwrap, 0.3.5)

### P2
9. OS sandbox as policy-plugin surface (today: explicit `enable_os_sandbox`)
10. Specialist work packs as data (markdown), not named hard-coded agents
11. Hosts that reimplement `Provider` (e.g. custom SSE) must call `apply_cache_control` themselves

## Non-goals

- Shipping a TUI (telekinesis)
- Cloud billing / hosted gateway
- Multi-device P2P product features
- Pi protocol compat (owned by telekinesis host)
- Scheduling policy (host's job — rotary only exposes capabilities)
