# Autoresearch controller

`rx4::AutoresearchController` is the engine capability for a host-driven
optimization loop. It is deliberately separate from the older opt-in
`.auto/` session tools, which remain available for compatibility but do not
provide isolation or rollback.

The controller follows the useful part of the Pi autoresearch pattern: keep
the harness small, make one focused hypothesis, measure it, and retain only a
measurable improvement. The host still decides when to form the next
hypothesis. See [Pi's minimalism and autoresearch case study](https://earendil.com/posts/pi-autoresearch-and-databricks/).

## Guarantees

- `new()` requires a clean repository root and creates a detached Git
  worktree in a controller-owned temporary directory.
- The hypothesis callback receives only the detached worktree path. Accepted
  changes become private checkpoint commits there; rejected and failed changes
  are restored with `git reset --hard` and `git clean -fdx`.
- A required correctness command must exit successfully. A metric command must
  emit a finite `METRIC name=value` for every measured run.
- Warmups are discarded. Measured runs preserve their samples and use the
  median for the acceptance decision. The configured threshold is absolute
  and strict: an improvement must be greater than `min_improvement`.
- Events are typed, delivered to subscribers, and appended as JSONL under the
  returned temporary `session_dir()`. The log is never rewritten or used as a
  mutable checkpoint database.
- Iteration, wall-clock, host-reported cost, and disk budgets stop the loop.
  Cancellation kills bounded metric/check commands and rolls back the active
  candidate. The callback receives the same cancellation handle so it can
  stop an in-flight agent turn.
- `final_patch()` is read-only. The user's checkout changes only after an
  explicit `accept_final_patch()` call, and that call refuses if the user's
  HEAD or worktree changed since `new()`.

The callback/cancellation contract follows the useful lifecycle discipline in
[Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent): abort is an
explicit signal that the host and long-running callback are expected to honor,
not a hidden scheduler side effect.

The controller does not schedule iterations, generate hypotheses, alter an
`Agent` policy, register tools, commit to the user's checkout, or choose an OS
sandbox. Those are host decisions. This follows rotary's capability/policy
boundary and the capability-based access lesson in [Cloudflare OS](https://github.com/cloudflare/cloudflare-os): an agent should receive a
resource explicitly, not through ambient access. Hosts running untrusted
measurement commands should attach their normal OS sandbox and network policy
to that command execution boundary. For the existing userspace validation
layer, hosts can call `set_command_sandbox(Arc<SandboxManager>)`; an OS sandbox
or outer container remains the real isolation boundary.

## Minimal SDK flow

```rust,no_run
use rx4::{
    Agent, AutoresearchController, AutoresearchControllerConfig,
    ExperimentHypothesis, HypothesisOutcome,
};

# async fn example(mut agent: Agent, root: std::path::PathBuf) -> Result<(), Box<dyn std::error::Error>> {
let config = AutoresearchControllerConfig::new(
    "compile-time",
    "milliseconds",
    "./.auto/measure.sh",
    "./.auto/checks.sh",
);
let mut experiment = AutoresearchController::new(root, config).await?;
experiment.subscribe(|event| {
    // Forward the typed event to the host UI.
    let _ = serde_json::to_value(event);
});
experiment.establish_baseline().await?;

let hypothesis = ExperimentHypothesis::new("h-1", "use the smaller allocation path");
experiment
    .run_iteration(hypothesis, |workspace| async move {
        // The host may point an Agent at workspace.path() before prompting it.
        agent.set_workspace_root(workspace.path());
        // The host owns the agent prompt and its cost accounting.
        // agent.prompt("implement the hypothesis").await?;
        Ok(HypothesisOutcome { cost_usd: 0.0 })
    })
    .await?;

let preview = experiment.final_patch().await?;
// Render `preview`; do not call accept_final_patch without user consent.
let _ = preview;
experiment.close().await?;
# Ok(())
# }
```

In a real integration, use a dedicated agent/session for the worktree or
explicitly reset its conversation between hypotheses. Keep the tool loadout
stable across turns where prompt-cache reuse matters, as described by [Pi's
prompt-cache notes](https://earendil.com/posts/prompt-caching/). The
controller's `Agent` attachment is only an SDK reference:

```rust,no_run
let handle = rx4::new_controller_handle(experiment);
agent.set_autoresearch_controller(handle.clone());
```

Attaching the handle does not start the loop.

## Telekinesis integration contract

Telekinesis can implement `/autoresearch` as a thin host surface:

1. `start(root, config)` creates the controller, subscribes before baseline,
   and renders the `Baseline` event or setup failure.
2. For each row, Telekinesis asks its agent/session for exactly one
   `ExperimentHypothesis`, then calls `run_iteration`. The callback points the
   agent at `ExperimentWorkspace::path()` and returns the provider-cost delta
   in `HypothesisOutcome`.
3. Map `Iteration` to a running row, then `Accepted`, `Rejected`, or `Failed`
   to the terminal row state. Persist/replay the append-only JSONL path or
   consume the subscriber stream; do not invent a second acceptance rule.
4. `/autoresearch cancel` calls `AutoresearchCancellation::cancel()`, waits
   for the failed/rolled-back iteration and `Completed { reason: Cancelled }`,
   then calls `close()`.
5. `/autoresearch patch` calls `final_patch()` and displays the changed files
   and diff. A separate user action calls `accept_final_patch()`; there is no
   implicit merge, commit, reset, or checkout mutation.
6. `/autoresearch stop` calls `complete()` when the host wants an explicit
   terminal event without applying the patch, then calls `close()`.

The minimum iteration-table fields are `iteration`, hypothesis id and
description, status, metric, samples, improvement, cost, duration, checkpoint,
candidate commit, and reason. The host may add model/provider/token fields,
but those remain host-owned metadata.

The engine's event types are:

| Event | Meaning |
| --- | --- |
| `baseline` | Initial checkpoint passed measurement and guards. |
| `iteration` | One hypothesis was admitted to the isolated worktree. |
| `accepted` | Guards passed, threshold was exceeded, and a private commit was made. |
| `rejected` | Guards or metric acceptance failed; checkpoint was restored. |
| `failed` | Applying, measuring, Git, cancellation, or a budget operation failed; checkpoint was restored when applicable. |
| `completed` | The controller reached an explicit, cancellation, budget, failure, or final-patch terminal state. |

The controller's command execution is an explicitly configured capability. It
does not expand the normal `Agent` tool registry or bypass host `Policy`,
`Authorizer`, approvers, or scopes. Hosts that want hard process isolation
should run the experiment under the same OS sandbox policy they use for other
process tools; the detached worktree is the Git isolation and rollback layer,
not a kernel sandbox.
