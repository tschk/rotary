# Final audit report — rotary (rx4) 0.3.19
Date: 2026-07-19

## Executive summary
Supply chain clean (`cargo audit` 0 vulns). Core permission hard-denies improved. **Ship OK as trusted-host capability library.** **Do not claim multi-tenant isolation** until HIGH items fixed (bash pipe deadlock, tool-call id, IPC auth).

| Severity | Count (open) |
|---|---|
| CRITICAL | 0 |
| HIGH | 3 (T-001, T-002, T-003) |
| MEDIUM | 6 |
| LOW | noted, dropped from promotion |

## Methodology
- cargo audit + cargo deny (advisories/bans/sources)
- Manual thermo re-audit of prior P0/P1 + agent/tools/sandbox/ipc
- Code-reviewer pass on async tool path
- Ponytail complexity audit
- Semgrep/gitleaks/trivy/codeql: **not installed** in environment (noted gap)

## Findings summary

| ID | Sev | Title | Status |
|---|---|---|---|
| T-001 | HIGH | Bash pipe deadlock | OPEN |
| T-002 | HIGH | Tool result id ≠ call id | OPEN |
| T-003 | HIGH | IPC unauthenticated mutators | OPEN |
| T-004 | MED | Dual shell blocklist / rm false positive | OPEN |
| T-005 | MED | Seatbelt global file-read* | OPEN |
| T-006 | MED | No symlink canonicalize | OPEN |
| T-007 | MED | set_workspace_root sandbox drift | OPEN |
| T-008 | MED | web_fetch skips network validate | OPEN |
| T-009 | MED | CLI/IPC always_allow defaults | OPEN |
| prior | — | OS fail-open / default userspace / allowlist hard-deny / effect Process / authorizer clear | FIXED |

## Supply chain
- **cargo audit**: clean (0 vulnerabilities, 272 crates)
- **cargo deny** advisories/bans/sources: ok
- **cargo deny licenses**: fail without root deny.toml (false process failure; add permissive MIT/Apache allowlist if CI wants green)

## Ship stance
Trusted host embed: **yes** with eyes open.  
Untrusted multi-tenant sandbox: **no**.

## Artifacts
- `piolium/thermo-reaudit.md`
- `piolium/code-review.md`
- `piolium/ponytail-audit.md`
- `piolium/cargo-audit.json`
- `piolium/final-audit-report.md` (this file)


## Fix pass 2026-07-19
Shipped in 0.3.20: T-001..T-009 addressed (see CHANGELOG).
