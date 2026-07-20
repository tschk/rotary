# Thermonuclear re-audit — rx4 0.3.19
Date: 2026-07-19
Commit: 4ad7591 (HEAD at audit time)
Scope: library agent harness; untrusted model args; optional Unix IPC; CLI host

## Thermo Status (prior items)

| ID | Status | Evidence |
|---|---|---|
| P0 OS sandbox silent fail-open | **FIXED (partial)** | `OsSandboxRunner::new` errors if seatbelt/bwrap missing (`src/sandbox/os.rs`); `enable_os_sandbox` errors on UserspaceOnly (`src/agent/mod.rs`). Residual: on failure Agent clears `policy.enable_os_sandbox` and continues with userspace only — intentional soft-fail for embed, not silent bare-OS bash. |
| P0 FS outside workspace w/o SandboxManager | **FIXED** | `Agent::new` / `set_policy` / `set_scope` call `ensure_userspace_sandbox()`. |
| P0 IPC auth-by-default | **STILL OPEN** | `RX4_IPC_TOKEN` only enforced if set + non-empty (`src/ipc.rs` ~120). Mutating methods `set_policy` / `set_scope` / `set_approver` / `clear_authorizer` open without token. |
| P0 Allowlist bypass hard-deny | **FIXED** | Allowlist after path/shell hard gates (`src/permissions.rs` ~366–406); test `allowlist_still_enforces_dangerous_shell`. |
| P0 Dual blocklist / rm -rf false positive | **REGRESSED / SPLIT** | Policy `is_rm_rf_root` fixed; userspace `is_blocked_command` still substring `"rm -rf /"` (`src/sandbox/userspace.rs` ~419) → blocks `rm -rf /tmp/...` at sandbox layer. |
| P1 unknown tool effect | **FIXED** | `effect_of` defaults `Process` (`src/agent/tool_types.rs`). |
| P1 stale Authorizer | **FIXED (aggressive)** | `set_policy`/`set_scope` clear custom authorizer. |
| P1 symlink canonicalize | **STILL OPEN** | `SandboxManager::canonicalize` joins only; no `fs::canonicalize` / symlink resolve. |
| P1 subagent isolation honesty | **STILL OPEN** | Worktree optional; policy derived from config; not OS-isolated by default. |
| P1 CLI approval default | **STILL OPEN** | `--approval` default `always_allow` (`src/bin/rotary.rs`). |
| P1 seatbelt global read* | **STILL OPEN** | `(allow file-read*)` in profile (`src/sandbox/os.rs` ~69). |
| P1 secrets only tool results | **STILL OPEN** | Redact in `run_tool_call` on tool content only. |

## New / Confirmed Findings

### T-001 Severity: HIGH
Bash stdout/stderr pipe deadlock under large output
Location: `src/tools/fs.rs` `exec_bash` (~179–226)
Attacker: model (any bash that prints >pipe buffer ~64KB)
Path:
1. Model calls bash with large stdout
2. Child fills pipe, blocks on write
3. Parent only `try_wait` until exit — never drains until after wait
4. Wait never completes → timeout kill (DoS) or hang until timeout
Impact: reliable tool DoS; false timeouts; incomplete output on edge cases
Remediation: concurrent drain (`tokio::io::copy` / `read_to_end` on stdout+stderr while waiting) or `wait_with_output` under timeout
PoC sketch: `bash -c 'python3 -c "print(\"x\"*200000)"'` under short timeout

### T-002 Severity: HIGH
Tool result `id` not rewritten to provider `tool_call_id`
Location: `src/tools/fs.rs` etc. stamp `"bash"`/`"read"`; `run_tool_call` returns result without `result.id = call.id` (`src/agent/mod.rs` ~765–784); messages use `result.id` (~491)
Attacker: n/a (correctness → provider protocol break)
Path:
1. Provider emits tool_call id `call_abc`
2. Tool returns `ToolResult.id = "bash"`
3. Agent posts tool message with id `bash`
4. Next turn API rejects / mis-associates tool results
Impact: multi-tool turns break; possible provider 400s; wrong pairing under parallel tools
Remediation: after execute (and cache hit already OK), set `result.id = call.id.clone()`
PoC sketch: parallel two reads — both results id `"read"`

### T-003 Severity: HIGH
IPC mutating surface unauthenticated by default
Location: `src/ipc.rs` ~120–227
Attacker: local IPC client (same user / socket access)
Path:
1. Host runs `rotary serve` without `RX4_IPC_TOKEN`
2. Client calls `set_approver` always_allow + `set_policy` full_access
3. Client triggers `prompt` → model tools run unrestricted
Impact: local privilege to full agent tool power
Remediation: require token always (or refuse serve without token); auth all non-ping methods; optional allowlist methods
PoC sketch: JSON-RPC `set_approver` + `set_policy` without token on open socket

### T-004 Severity: MEDIUM
Userspace command blocklist false-positive + dual deny lists
Location: `src/sandbox/userspace.rs` `is_blocked_command`; vs `permissions::is_dangerous_shell_command` / `is_rm_rf_root`
Attacker: model (availability); also policy confusion for host
Path:
1. Policy allows `rm -rf /tmp/build` (not root wipe)
2. Sandbox `validate_command` substring-matches `"rm -rf /"`
3. Command denied at sandbox after policy Allow
Impact: false deny; hosts maintain two incompatible deny engines
Remediation: call shared `is_dangerous_shell_command` / `is_rm_rf_root`; drop naive substring list or gate only under FullAccess bypass paths

### T-005 Severity: MEDIUM
Seatbelt allows global `file-read*`
Location: `src/sandbox/os.rs` ~69 + selective deny `.ssh`/`.aws`
Attacker: model via bash under seatbelt
Path:
1. OS sandbox enabled, seatbelt wraps bash
2. Profile allows all file reads
3. `cat ~/.netrc` / project sibling secrets succeed (except few deny subpaths)
Impact: read exfil outside workspace despite "sandbox"
Remediation: default deny read; allow workspace + explicit system libs paths needed for dyld; expand sensitive deny list

### T-006 Severity: MEDIUM
`SandboxManager::canonicalize` ignores symlinks
Location: `src/sandbox/userspace.rs` ~306–312
Attacker: model if can create symlink in workspace
Path:
1. Write symlink `ws/link -> /etc/passwd` (or outside)
2. `read` path `link` joins workspace, `starts_with(workspace)` may still true for non-resolved path depending on OS representation — **if path string stays under ws without resolve, open follows link**
3. `std::fs::read` follows symlink → outside content
Impact: workspace path check bypass for reads/writes via symlinks
Remediation: `std::fs::canonicalize` when exists; for create, resolve parent; reject if target outside after resolve

### T-007 Severity: MEDIUM
`set_workspace_root` does not refresh sandbox roots
Location: `src/agent/mod.rs` ~223–225
Attacker: host misconfig / IPC if exposed later
Path:
1. Agent built with root A + SandboxManager(A)
2. `set_workspace_root(B)` only flips field
3. FS tools validate against sandbox A while tools resolve under B
Impact: wrong confinement / open holes
Remediation: rebuild userspace + clear/rebuild OS sandbox on root change

### T-008 Severity: MEDIUM
`web_fetch` ignores `SandboxManager::validate_network`
Location: `src/tools/extended.rs` `exec_web_fetch` — `_ctx` unused for network
Attacker: model
Path:
1. Policy/sandbox network denied
2. `web_fetch` still HTTP GETs arbitrary URL (providers feature)
Impact: SSRF / data exfil channel despite network flag
Remediation: `ctx.sandbox.validate_network()`; optional host allowlist

### T-009 Severity: MEDIUM
CLI / IPC defaults favor always_allow
Location: `src/bin/rotary.rs` `--approval` default; IPC `set_approver` default string `always_allow`
Attacker: host misconfig + model
Impact: "secure defaults" in library Policy undermined by product defaults
Remediation: interactive default `ask`; CI flag for always_allow; IPC refuse always_allow without token

### T-010 Severity: LOW→drop for final table but noted
Secrets redaction only on tool results; not model stream / session JSONL
Location: `src/agent/mod.rs` redact after tool execute
Impact: keys in assistant text or persisted session may leak to logs/UI
Remediation: host-side; optional redact on session write

### T-011 Severity: LOW (correctness/DoS)
`spawn_agent` / multiagent `wait_sync` on async runtime
Location: `src/tools/mod.rs` ~196; `src/multiagent.rs`
Impact: blocks worker thread under nested agents
Remediation: async wait or spawn_blocking

## Residual risk (ship stance)

**OK as capability engine for trusted host** that owns policy, Approver, and socket ACL — core Policy hard-denies + userspace sandbox attach are real improvements vs early 0.3.x.

**Not OK as multi-tenant / untrusted-model isolation boundary** until: IPC auth mandatory, symlink-safe paths, seatbelt read confinement, bash pipe drain, tool-id fix, network gate on web_fetch. OS sandbox is best-effort host isolation, not a jail.

## Top 5 fix order
1. Bash concurrent pipe drain (T-001)
2. `result.id = call.id` (T-002)
3. IPC require token + gate mutators (T-003)
4. Symlink-aware canonicalize + workspace refresh (T-006/T-007)
5. Seatbelt read lockdown + web_fetch network + unify blocklist (T-005/T-008/T-004)

## Supply chain
- `cargo audit`: **0 vulnerabilities** (272 deps, advisory-db 2026-07-17)
- `cargo deny` advisories/bans/sources: **ok**
- `cargo deny` licenses: **failed** — no root `deny.toml`; default config rejects common MIT/Apache OR expressions (tooling noise, not runtime CVE)
