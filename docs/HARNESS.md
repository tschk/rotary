# Engine-owned harness pieces

Hosts (telekinesis, apollo) embed these; they do not reimplement them.

## Generic harness builder

`HarnessBuilder` is an opt-in composition helper, not another agent type. Its
`build` method returns the ordinary `Agent` after applying a generic operating
contract, a scope and policy, selected builtin tools, and optional
confidence-gated todo state.

```rust
use rx4::{HarnessBuilder, Policy, Scope};

let mut agent = HarnessBuilder::new()
    .scope(Scope::Coding)
    .policy(Policy::workspace_write())
    .builtin_tools(["read", "write", "edit", "bash", "grep", "find", "ls", "todo"])
    .instructions("Keep the public API backward compatible.")
    .build()?;

// The host still selects and attaches these capabilities.
// agent.set_provider(provider);
// agent.set_async_approver(approver);
// agent.set_hooks(hooks);
// agent.load_project_context();
```

The default contract requires exploration before action, confirmation before
irreversible actions, and practical verification. `instructions` are appended
to it. `system_prompt` instead supplies a complete host-owned prompt and omits
both the contract and caller instructions; the normal scope addendum still
applies. Use `todo_config(None)` to leave todo state disabled.

`HarnessConfig` and `BuiltinToolSelection` are serializable because hosts may
store setup defaults. Provider instances, approvers, hooks, session paths, and
lifecycle or scheduling decisions are intentionally absent. They stay under
host control.

`builtin_tools` selects exactly from `read`, `write`, `edit`, `hashline_edit`,
`bash`, `grep`, `find`, `ls`, `web_fetch`, `todo`, `spawn_agent`, plan-mode,
LSP, `exec`, and `script` tools already in rotary. Unknown or duplicate names fail during
`build`; explicitly selecting `bash`/`grep`/`find`, `web_fetch`, or an LSP tool
without its `builtin-tools`, `providers`, or `ipc` feature respectively also
fails during construction. The default loadout contains the portable coding
tools available under the enabled features. Selecting `script` requires its
feature and the helper described below.

## Supervised scripts

The optional `script` feature adds a LuaJIT tool. Build and package the
matching `rx4-script-worker` binary with the host. The agent searches for the
helper beside the current executable, or the host can set `Agent::script_worker`
to its absolute path. A missing or incompatible helper fails closed.

Scripts run in a supervised process. The parent authorizes each file bridge
through its normal tool gates and enforces a wall-clock deadline. On Linux,
the helper limits its address space to 96 MiB, or a lower inherited limit.
The bridge runs in the parent because it enforces the parent's authorization.
Per script, frames are capped at 2 MiB, reads at 1 MiB, directory listings and
the grep traversal frontier at 4096 entries, grep matches at 500, and regex
compilation and cache sizes at 2 MiB each. A script has at most one bridge job
in flight, with a maximum of 4096 requests. These are per-script bounds, not
an aggregate parent quota: hosts must bound concurrent scripts and limit the
parent process separately when running untrusted workloads.
The helper fails closed on other platforms and when it cannot apply the limit.

## Hashline

`rx4::hashline` — tagged file reads (`[path#TAG]` + `N:line`) and fail-closed
`PUT` / `CUT` / `MV` / `REM`. Stale tags, elided or unseen lines, and no-ops
error. Some model families get a sloppy parse fallback after strict parse fails.

`read` accepts `"hashline": true`. When set, `limit` is the max visible line
count; `head`/`tail` are derived from `limit` only so they cannot exceed it.
`offset` is ignored on hashline reads (it remains the start line for plain reads).

`hashline_edit` applies a script against the visibility recorded by the last
hashline `read` of that path whose tag still matches. Without a prior hashline
read — or if the tag does not match that read — every line is treated as unseen
and the edit fails closed. Elided lines from that read also fail closed.
A successful edit invalidates the stored visibility; read again before the next
script. Sequential ops are bounded against the *current* buffer (CUT then PUT
returns `OutOfRange` instead of panicking).

## Prewalk / plan-yolo

`rx4::prewalk::Prewalk` — investigate on the big model; the first real write
switches one-way to the smol/apply model.

- `RX4_PREWALK=1`
- `RX4_SMOL_MODEL`
- `RX4_INVESTIGATE_MODEL`

## AVO

`rx4::avo` — `P_t`, two-part `f` (incorrect ⇒ 0), commit-if-better, stall
detect. `scripts/avo-commit-if-better.sh` refuses `main`/`master` and never
pushes. Subtask completion is host-adjudicated (`rx4::subtask`); claims only
go down. See [PROJECTION.md](PROJECTION.md).
