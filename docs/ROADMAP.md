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
1. [x] Stream-JSON / noninteractive protocol — `rx4 exec --stream-json` NDJSON events (0.3.8)
2. [x] Approval UX hooks — `Event::ApprovalRequired(ApprovalRequest)` rich ask payload (0.3.8)

### P1
3. [x] Tool effects + parallel tool batches (`JoinSet`, 0.3.3+)
4. [x] Session export/import Codex JSONL (`export_codex_jsonl` / `import_codex_jsonl`, 0.3.8)
5. [x] Consolidation auto-run via `enable_auto_dream` (0.3.6); host still owns schedule policy
6. [x] Skills/graph host hooks on `Agent` (`set_skill_registry` / `set_graph_memory`, 0.3.5)
7. [x] Prompt-cache on Anthropic stream path (0.3.5)
8. [x] OS sandbox wrap for bash (`enable_os_sandbox` seatbelt/bwrap, 0.3.5)
9. [x] Background review on prompt via `set_skill_engine` (0.3.6)

### P2
9. [x] OS sandbox as policy-plugin surface — `Policy.enable_os_sandbox` + `with_os_sandbox` (0.3.8)
10. [x] Specialist work packs as markdown data (`WorkPack`, 0.3.8)
11. Hosts that reimplement `Provider` (e.g. custom SSE) must call `apply_cache_control` themselves (documented; tele applies)


### P3 (0.3.9)
12. [x] Gating hooks + path-aware write auth + real approver args
13. [x] Extended tools (web_fetch, todo, spawn_agent, plan mode, lsp)
14. [x] MCP remote HTTP/SSE transports

## Non-goals

- Shipping a TUI (telekinesis)
- Cloud billing / hosted gateway
- Multi-device P2P product features
- Pi protocol compat (owned by telekinesis host)
- Scheduling policy (host's job — rotary only exposes capabilities)
