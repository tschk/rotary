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

### Engine capability gaps

These are proposals, not completed features. Hosts continue to choose policy
and scheduling. Each item needs an end-to-end contract test before release.

- **Authority remains patched, not structurally proven.** Child construction
  now intersects inherited policy, scope, tool selection and approval, and
  scope changes retain the inherited ceiling. Regression tests cover two
  previously escaping paths. However, authority is still distributed across
  mutable `Agent` fields, policy, authorizers and tool-specific bridges; it is
  not encoded as a single non-widening capability in the type system. Audit
  every delegation and tool-dispatch path, including nested children and script
  bridges, against one authority contract. Move enforcement to a mandatory
  boundary that cannot be replaced by a scope transition or forgotten by a
  new tool. Keep host-owned policy and approval semantics. See
  `src/agent/mod.rs`, `src/subagent.rs`, and `src/permissions.rs`.
- **Resource budgets for delegated work.** Script helpers have a Linux
  per-process address-space cap and bounded parent bridge operations. Those
  limits do not bound aggregate parent memory across concurrent scripts or
  blocking host callbacks; hosts must limit concurrency and parent resources.
  Provide a shared, host-configurable aggregate budget with cancellation and
  accounting across helpers and bridges. See `src/tools/script.rs` and
  `src/tools/script/worker.rs`.
- **Non-Linux script support.** The supervised Lua helper fails closed outside
  Linux because its address-space quota is not implemented there. Hosts on
  those platforms cannot use the `script` tool, including its Lua processing
  and read/list/grep bridges. Add an equivalent enforced quota before enabling
  it; do not silently run without one.
- **Recoverable sessions.** `Session::save_jsonl` overwrites the existing file
  and `load_jsonl` skips malformed records. Add an atomic save and an explicit
  recovery report so a host can distinguish a complete history from a damaged
  one. Preserve fork, projection, and artifact references during recovery. See
  `src/session.rs`.
- **MCP concurrency and rich resources.** `McpClient` holds one mutex across
  each request, and `read_resource` returns the first text item. Discovery
  follows tool and resource pagination. Support concurrent request IDs,
  cancellation, and typed resource contents without discarding additional items.
  Test against a server that sends out-of-order replies and multi-part resources.
  See `src/mcp.rs`.
- **Observable provider retries.** The agent retries transient stream failures
  with fixed backoff in `Agent::prompt`. Expose retry attempts and terminal
  reasons to hosts, and verify that partial text or tool calls are not repeated
  as completed work after a stream failure. See `src/agent/mod.rs` and
  `src/provider.rs`.
- **Composable long-running work.** Background tool results, bounded tool
  pipelines and durable worker handles exist. Extend recovery beyond restoring
  state: define which work can be resumed without replaying side effects, and
  expose checkpoints and cancellation to hosts. Keep approval and scheduling
  with the host. See `src/agent/mod.rs`, `src/subagent.rs`, and
  `src/agent/pipeline.rs`.

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
