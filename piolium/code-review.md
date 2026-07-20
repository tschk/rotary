# Code review — rx4 0.3.19 (HEAD)

Focus: `src/agent/mod.rs`, `src/agent/tool_types.rs`, `src/tools/fs.rs`, `src/permissions.rs`, `src/sandbox/**`, `src/ipc.rs`, CHANGELOG / features.

## Summary

0.3.19 hardens async tool dispatch (spawn_blocking Approver, async bash, safer `effect_of` default) and recent permission gates, but several production-path correctness bugs remain around bash I/O, tool-result IDs, workspace/sandbox drift, and a userspace command-blocklist false positive.

## Critical

### C1. Bash waits without draining pipes → hang until timeout
- **Location:** `src/tools/fs.rs` `exec_bash` (~L183–224)
- **Problem:** Child spawned with `stdout`/`stderr` piped. Loop only `try_wait` + sleep. No concurrent `read` of pipes. Large output fills OS pipe buffer → child blocks on write → parent never sees exit → waits full timeout (default 120s), then kill. High-output `cargo test` / `find` / logs look like flaky timeouts.
- **Fix:** Prefer `tokio::process::Command::output()` under `timeout`, or `tokio::join!` of `wait` + `read_to_end` on both pipes *before* exit, not after. Keep cancel/timeout kill path.

### C2. Tool results use static tool name as `id`, not `ToolCall.id`
- **Location:** `src/tools/fs.rs` / `extended.rs` (`ToolResult::ok("bash", …)`, `"read"`, …); agent then `Message::tool(&result.id, …)` in `src/agent/mod.rs`
- **Problem:** Provider protocol needs `tool_call_id` = model call id. Results stamp `"bash"` / `"read"`. Multi-tool turns and OpenAI-compatible backends mis-associate outputs; parallel reads collide on id.
- **Fix:** Pass `call.id` into executors (or rewrite `result.id = call.id.clone()` in `run_tool_call` after execute, including cache hits). Never hardcode tool name as id.

### C3. Userspace command blocklist false-positive on `rm -rf /tmp/...`
- **Location:** `src/sandbox/userspace.rs` `is_blocked_command` (`"rm -rf /"` substring)
- **Problem:** Permissions layer already special-cases root wipe vs `/tmp` (`is_rm_rf_root`). Userspace still treats any `rm -rf /…` as blocked. Agent always attaches userspace sandbox → safe `rm -rf /tmp/build` denied at tool layer even when policy allows.
- **Fix:** Reuse `permissions::is_dangerous_shell_command` / `is_rm_rf_root`, or same root-only predicate. Add regression test `rm -rf /tmp/x` allowed, `rm -rf /` denied.

### C4. `set_workspace_root` does not refresh sandbox / OS sandbox roots
- **Location:** `src/agent/mod.rs` `set_workspace_root` (~L223–225); `ensure_userspace_sandbox` only creates if `sandbox.is_none()`
- **Problem:** Host changes workspace after `Agent::new` (subagent path, multi-root hosts). Userspace + seatbelt/bwrap still confine old root. FS tools resolve against new root; OS wrap still binds old workspace → false denies or wrong mount.
- **Fix:** On `set_workspace_root`, rebuild `SandboxManager` and re-`enable_os_sandbox` / rewrite profile when OS sandbox enabled. Document that hosts must call a single “set root” that refreshes both.

## Major

### M1. Sync FS I/O on async runtime
- **Location:** `src/tools/fs.rs` `exec_read` / `write` / `edit` / `ls` / `grep` / `find` (`std::fs::*`, rayon inside async)
- **Problem:** Large files / slow disks stall multi-thread runtime workers; parallel Read batch multiplies stalls.
- **Fix:** `tokio::task::spawn_blocking` around blocking FS work (or `tokio::fs`). Keep rayon inside blocking section only.

### M2. `spawn_agent` blocks async with `wait_sync`
- **Location:** `src/tools/extended.rs` `exec_spawn_agent`; `register_spawn_agent_tool` in `tools/mod.rs`
- **Problem:** Nested agent wait on runtime thread under JoinSet → deadlock risk with limited workers / Approver.
- **Fix:** Async wait API on handle, or `spawn_blocking` for sync wait.

### M3. IPC connection path is blocking-on-async + fire-and-forget prompt
- **Location:** `src/ipc.rs` `handle_connection` (`BufReader::lines` on std `UnixStream`), `prompt` method (`tokio::spawn` + `let _ = a.prompt`)
- **Problem:** Blocking line reads inside async task starve runtime. Prompt errors discarded; host gets `"prompt accepted"` with no completion/error channel. `attach_agent` races via separate spawn.
- **Fix:** `tokio::net::UnixListener` + async read lines; await prompt under agent mutex or return job id + status RPC; attach_agent should be sync under lock or documented race.

### M4. `web_fetch` ignores sandbox network policy
- **Location:** `src/tools/extended.rs` `exec_web_fetch`; sandbox default `allow_network: false`
- **Problem:** Workspace sandbox claims network denied; Network-effect tool still HTTP GETs when `providers` on. Policy/read-only hosts can be surprised.
- **Fix:** Call `ctx.sandbox.validate_network()` (and optional host allowlist) before request; fail closed.

### M5. Seatbelt profile is read-wide
- **Location:** `src/sandbox/os.rs` `generate_seatbelt_profile` — `(allow file-read*)` + deny only `.ssh`/`.aws`/`.config/gcloud`
- **Problem:** OS bash can read secrets outside those three paths (`.gnupg`, `.netrc`, keychains paths, other home dirs). Userspace path checks do not apply inside child.
- **Fix:** Default-deny file-read with explicit ro subpaths (workspace, `/usr`, `/System`, toolchain paths) or document as intentional weak profile and expand denylist.

### M6. Tool cache key ignores workspace / policy
- **Location:** `src/agent/mod.rs` `run_tool_call` cache_key `format!("{}:{}", resolved_name, call.arguments)`
- **Problem:** Reused Agent after root change (or same relative path different tree) can serve stale Read hits until Write/Process invalidates.
- **Fix:** Include workspace root (and maybe policy mode) in key; clear cache on `set_workspace_root`.

### M7. Host footgun: IPC / CLI default Approver allow
- **Location:** `src/ipc.rs` `set_approver` default `always_allow`; `src/bin/rotary.rs` `--approval` default `always_allow`
- **Problem:** Documented for CI, but remote socket client can flip AlwaysAllow if token weak/missing. Without `RX4_IPC_TOKEN`, any local user connecting to socket can set always_allow and prompt.
- **Fix:** Default IPC approver mode to `clear`/`ask`; require token when binding; refuse `always_allow` over IPC unless explicit env opt-in.

### M8. Security-sensitive test gaps
- Missing tests: bash large-output / timeout-with-output; tool result id = call id; userspace `rm -rf /tmp` vs `/`; `set_workspace_root` updates sandbox root; web_fetch + network deny; authorize path + symlink if ever canonicalize; parallel Approver `spawn_blocking` under multi-thread; IPC token reject.
- Permissions coverage for shell/allowlist is strong; sandbox blocklist and agent↔tool id wiring lag.

## Minor

### m1. Fragile `"approval required"` string match
- **Location:** `src/agent/mod.rs` `execute_tools_parallel`
- **Fix:** Typed error / flag on `ToolResult` or dedicated enum variant for Ask.

### m2. JoinSet panic → generic `"tool execution failed"`
- **Location:** join `Err` arm only warns; hole filled later with generic err
- **Fix:** Map join error into result content with call id + panic message.

### m3. Seatbelt profile files under `/tmp` never unlinked
- **Location:** `OsSandboxRunner::new` / `write_profile`
- **Fix:** `Drop` impl remove profile path.

### m4. `bwrap --clearenv` with empty whitelist
- Child may lack `PATH`/`HOME`/`USER` → cryptic bash failures when OS sandbox on Linux.
- **Fix:** Default whitelist minimal (`PATH`, `HOME`, `USER`, `LANG`, `TERM`) or document host must set `env_whitelist`.

### m5. Cancel only when `ipc` feature
- Bash cancel check is `#[cfg(feature = "ipc")]`. Embedders without ipc cannot cancel bash via token.
- **Fix:** Always-on cancel token type, or feature-independent abort handle.

### m6. Authorizer snapshot cleared on every `set_policy`/`set_scope`
- Documented; still easy host bug if custom Authorizer holds extra state.
- Prefer re-wrapping live policy or callback Authorizer.

## Positive

- `effect_of` unknown → `Process` (no accidental parallel/cache) — correct fail-safe.
- Sync `Approver` via `spawn_blocking` — right fix for ChannelApprover under JoinSet.
- Allowlist no longer skips path/shell hard-deny (0.3.18).
- `set_scope` / `Policy::apply_scope` preserve host shell lists.
- `Policy::default` / `Agent::new` = `workspace_write`; process tools Ask.
- Quote/pipe-aware shell segments + allow-all-segments semantics.
- `kill_on_drop(true)` on bash child.
- Socket mode `0o600`; optional `RX4_IPC_TOKEN`.
- Solid unit tests for shell rules, allowlist, path escape, parallel reads, cache invalidation, bash timeout/cancel.

## Questions

1. Is seatbelt global `file-read*` intentional product choice, or leftover MVP?
2. Should `ApprovalRequired` ever *resume* same tool call, or is fail-closed + model retry permanent design?
3. Is IPC meant for multi-user machine without mandatory token, or single-user local only?
4. Should `web_fetch` be host-gated only (policy Ask) rather than sandbox network flag?

## Verdict: **Request Changes**

Block on C1–C4 (bash pipe wait, tool_call id, userspace `rm -rf /tmp` false positive, workspace/sandbox root drift). M1–M4 should land soon after; M5–M8 before marketing OS sandbox as a security boundary.
