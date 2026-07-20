# Knowledge base — rotary re-audit 2026-07-19

## Component inventory
- Crate: rx4 (Rust 2021), MPL-2.0
- Runtime: tokio (rt,time,macros,sync,process)
- Features: ipc, builtin-tools, providers, mcp, skills, graph-memory, computer-use, memory, sqlite-sessions
- Hosts: telekinesis, omi, CLI `rotary`

## Trust boundaries
1. Untrusted model → tool name/args JSON
2. Optional Unix socket JSON-RPC (local)
3. Subprocess bash / seatbelt / bwrap
4. Network via web_fetch / providers HTTP
5. Host-supplied Approver / Authorizer / Policy

## High-risk flows
- tool_call → PolicyAuthorizer → Approver → sandbox validate → execute
- IPC set_policy/set_approver → agent power change
- bash under OS sandbox profile

## Advisory intelligence
cargo audit 0 vulns at audit time.

## Static analysis summary
Manual + cargo audit. Semgrep/CodeQL not run (tools absent).
