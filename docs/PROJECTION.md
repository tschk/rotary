# Projection, recovery, and stable surfaces

Rotary keeps the session log append-only. Compaction is a projection of that
log, not a rewrite of it.

## PrefixShape and prune-then-fold

`PrefixShape` is the canonical byte digest of the live system prefix. Prune
drops old turns and must leave those bytes unchanged. Fold (a summary inserted
after the prefix) runs only when prune still cannot fit the trigger threshold.

Dropped turns are archived as verbatim JSONL (`RavenArchive`), not as a second
summary. Hosts may persist the archive beside `session.jsonl`.

## Request reconstruction

Provider requests rebuild from the session log (`Session::serialize_provider_request`
/ `replay_provider_request`). Mutating a live `Vec<Message>` does not change a
request reconstructed from `session.jsonl`.

## Empty-turn recovery

`recover_empty_turn` / `recover_stuck_tool` classify `Prefill`, `Nudge`,
`Retry`, or `Halt`. The engine records; the host decides whether to continue.

## Sandbox escalate

On OS-sandbox deny the engine retries once at the next layer:
userspace → nested FS (seatbelt/bwrap) → `.git` remounted read-only.
`Event::RetryReason` is emitted. There is no silent pass after the last layer.

## Tool spill

Oversized tool bodies are written to `.rx4/spill/`. The model sees a preview
plus a locator.

## complete_subtask

Claims only go down the task tree. The host adjudicates; the engine records an
evidence ledger. Complements AVO scoring. A child cannot mark a parent complete.

## Stable MCP surface

When the `mcp` feature is on, the prefix exposes `tool_search` and
`use_capability` so child schemas do not churn the cached prefix.

## Trajectory cassette

`ReplayProvider` replays recorded turns without executing real tools.
`detect_divergence` reports the first mismatched message.

## Other host capabilities

- Unified exec sessions emit `ProcessStdin` (`process_id` + bytes).
- `GuardianAuthorizer` is fail-closed; hosts install the review callback.
- `apply_patch` is an optional bulk tool. Hashline remains the editor.
- Plan accept wipes `<planning>` / `[planning]` / `PLAN:` tokens.
- `WritePathSchedule`: omit paths = whole workspace = serialize writes.
- `ContextCapsule` gives subagents zero ambient inheritance.
