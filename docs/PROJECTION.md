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

Semantic and automatic compaction persist an append-only `projection` event
(`summary` + `archived_ids`). Replay applies that ledger so the next provider
request still includes the summary. Repeated `compact()` does not re-archive
the same dropped turns.

## Empty-turn recovery

`recover_empty_turn` / `recover_stuck_tool` classify `Prefill`, `Nudge`,
`Retry`, or `Halt`. The engine emits `Event::Recovery` (`action` + `reason`);
the host decides whether to continue.

## Sandbox escalate

On OS-sandbox deny the engine retries once at the next layer:
userspace → nested FS (seatbelt/bwrap) → `.git` remounted read-only.
`Event::RetryReason` is emitted. There is no silent pass after the last layer.

## Tool spill

Oversized tool bodies are written to `.rx4/spill/`. The model sees a preview
plus a locator. Previews truncate on a UTF-8 character boundary. If the spill
write fails, the model still receives a bounded preview and hosts get a typed
`SpillStatus::SpillFailed` notice (`Event::ToolSpill` / `ToolResult.spill`).

## complete_subtask

Claims only go down the task tree. The host adjudicates; the engine records an
evidence ledger. Complements AVO scoring. A child cannot mark a parent complete.

## Stable MCP surface

When the `mcp` feature is on, the prefix exposes `tool_search` and
`use_capability` so child schemas do not churn the cached prefix.

## Trajectory cassette

`ReplayProvider` replays recorded turns, including `CassetteTurn.tool_calls`.
`detect_divergence` / `detect_tool_divergence` report the first mismatch.
`Agent::enable_cassette_replay` simulates recorded tools instead of executing
them.

## Other host capabilities

- Unified exec sessions drain stdout/stderr, expose the `exec` tool
  (`spawn` / `stdin` / `wait` / `kill`), and emit `ProcessStart` /
  `ProcessStdin` / `ProcessEnd`.
- `GuardianAuthorizer` is fail-closed; hosts install the review callback.
- `apply_patch` is an optional bulk tool. Hashline remains the editor.
- Plan accept wipes `<planning>` / `[planning]` / `PLAN:` tokens.
- `WritePathSchedule`: omit paths = whole workspace = serialize writes.
- `ContextCapsule` gives subagents zero ambient inheritance.
- File snapshots and observed-version guards are host opt-in (`enable_file_guards`).
- `TwoSessionCoordinator` moves messages only on explicit routes.
- Skills with `required_tools` stay silent when those tools are not loaded.
- `ModelBinding` is per request; it is not a process-global default.
- `ShadowGit` checkpoints live in `.rx4/shadow.git` when the host enables it.
- `HunkLog` records hashline edits for rewind without replacing `hashline::apply`.
- Explore subagents default to depth 1 and do not inherit the parent transcript.
