# SWE-bench Verified accuracy bench

Resolve rate on [SWE-bench Verified](https://www.swebench.com/)
(`princeton-nlp/SWE-bench_Verified`, Harbor `swebench-verified`). The score is
**fail-to-pass tests**, not wall-clock on toy tasks.

## How to run

```bash
scripts/bench/check.sh
scripts/bench/run.sh                  # n=10, --sample-seed 0, all models × harnesses
scripts/bench/run.sh --n 3 --model gpt-5.6-sol   # smoke
scripts/bench/run.sh --n 500          # full 500 (opt-in)
scripts/bench/run.sh --full           # same
```

The benchmark glue requires LuaJIT 2.1 (`brew install luajit` on macOS or
`sudo apt-get install luajit` on Ubuntu). Its small pure-Lua JSON module is
vendored; no LuaRocks setup is needed. Dataset download, Pier task handling,
and the official SWE-bench evaluator remain Python integrations.

Output: `scripts/bench/out/<date>/REPORT.md`, `results.json`, and `plan.json`.
Each cell is `{harness, model, instance_id, resolved, seconds, adapter_model,
effort, patch_bytes}`. `resolved` is `null` when the official eval did not
produce a verdict (no Docker, `--skip-eval`, checkout or adapter failure,
empty patch, eval error).

Scoring rules:

- **Denominator = every planned cell** (`plan.json`: configured pairs × sampled
  ids). Unscored (`null`) and never-run cells count as unresolved. `upper` is
  the rate if every unscored cell had resolved; the 95% Wilson interval is
  shown only when every cell is scored. Status is `complete`, `INCOMPLETE`, or
  `NOT SCORED`; only `complete` rows are comparable.
- **Attribution**: a model runs on a harness only when its `models.json` entry
  has that harness's own `model`. Other pairs are listed under Exclusions and
  never fall back to an adapter's default model. Cells record the model string
  the adapter actually received (`adapter_model`).
- The report lists unscored cells by reason, pairs not run, duplicate or
  unplanned cells, and adapter failures, plus provenance (rotary commit and
  dirty flag, `models.json` and instance-set sha256, start/finish UTC).
- `scripts/bench/test.sh` (run by `check.sh`) pins these rules.

Needs Docker (OrbStack is fine) for Harbor, Pier, and
`python -m swebench.harness.run_evaluation`.

## Harnesses

These harnesses, same model + effort on each:

| harness | binary | flags |
|---|---|---|
| Codex CLI | `$HOME/.local/bin/codex` | `-m <model> -c model_reasoning_effort=<effort>` |
| Pi | `$HOME/.bun/bin/pi` | `--provider openai-codex --model <model> --thinking <effort>` |
| tk | prefer `~/projects/worktrees/telekinesis-rx4-consume/ui/tui/target/release/tk`, else PATH `tk` | `exec --model <model> --effort <effort>` when that flag exists; otherwise `--effort` is passed through only if `tk exec --help` lists it |
| OMP | `/opt/homebrew/bin/omp` (v18) or PATH `omp` | `-p --print --cwd TASK --no-session --model <model> --thinking <effort>`; stdin `/dev/null` |
| fx | prefer `$HOME/.local/bin/fx` (vercel-labs/fx; **not** antonmedv/fx), else PATH `fx` | `fx ask --yolo --no-save`; `--cwd`/`--model`/`--effort` when advertised, else `FX_MODEL`; stdin `/dev/null`. Unauthenticated fail closed. |

Adapters: `scripts/bench/adapters/{codex,pi,tk,omp,fx}.sh`. They take `TASK_DIR` plus
the issue prompt and write a git patch or leave the repo fixed. stdin is
`/dev/null`.

`run.sh` collect_patch drops runner-owned helpers (`.bench_prompt.md`) so they
never enter `model_patch` for official eval.

Models live in `scripts/bench/models.json` (default: `gpt-5.6-sol` low from
`~/.codex/config.toml`, plus `gpt-5.6-terra` low from the Codex model cache).

## Drivers

`run.sh` tries Harbor (`uv tool install harbor`, `harbor download swebench-verified`
/ `harbor datasets download swe-bench@verified`) for the dataset, then Pier
(`~/.local/bin/pier`) if you pass `--driver pier`.

Harbor and Pier ship a built-in `codex` agent. They do **not** drive Pi, tk, or
OMP, so those always use the thin loop:

1. Sample n instances with `--sample-seed` (default 0).
2. Check out the instance repo at `base_commit`.
3. Feed the issue text to the adapter.
4. Keep the working-tree fix / collect a git patch (excluding `.bench_prompt.md`).
5. If Docker is up, run the official eval
   (`python -m swebench.harness.run_evaluation`).

`--driver auto` (default) = Harbor dataset download + thin loop for every
harness (seeded, comparable). `--driver harbor` / `--driver pier` try those
frontends for Codex only.

## Cheap check

```bash
scripts/bench/check.sh
```

Confirms adapters exist, `models.json` parses, and `run.sh --help` works.
It also checks LuaJIT is installed and that an unscored cell stays `null` in
the generated report.

## Past runs

The August 2026 runs under `scripts/bench/out/` (never committed) were scored
with the old denominator, which dropped unscored cells. Corrected:

- n10 pi: reported 5/9 (56%), actually 5/10 (50%). n10 omp 0/10 was an
  adapter crash (`adapter_exit=1` on all 10 cells), not a model result.
- "full500" codex 8/9 (89%) and tk/pi/omp 6/10 (60%): official eval errored on
  478–488 of 500 instances, so only 9–10 were scored (8/500 and 6/500
  resolved). No rate can be defended from those runs.
- DeepSWE runs produced no verifier verdicts.

## Resume

`run.sh` keeps `--out` cells and patches when that directory already has work
(`--resume`, also the default in that case). Use `--no-resume` to truncate
`cells.jsonl`. Existing `patches/<harness>__<model>__<instance>.patch` files are
recorded as cells and skipped.

## Harness inner loop (no model, seconds)

Fix-and-iterate path. Does not measure GLM. Catches stream/tool/recovery
regressions that SWE-bench only shows after hours.

```bash
scripts/bench/harness.sh
```

Runs `cargo test` for `provider`, `cassette`, `guardrails`, `hashline`,
`avo`, and `compaction` with `providers` + `builtin-tools`. Includes the
Z.ai GLM fixture: `usage` bundled with `finish_reason: tool_calls` must
still emit the tool call.

Live coding-plan smoke (optional, uses `ZAI_API_KEY`, model **glm-5.3-flash**
only — do not spend the Coding Plan key on glm-5.3 for this loop):

```bash
TK_BIN=/path/to/tk scripts/bench/harness-live.sh
```

## DeepSWE (faster live score)

DeepSWE gives a faster pass@1 than a full Verified 500 run.

    scripts/bench/deepswe/run.sh --n 20 --sample-seed 0 --model gpt-5.6-sol \
      --harness tk --harness codex --harness pi --harness omp --harness fx \
      --out scripts/bench/out/2026-08-23-deepswe20

Agents run host-side (same adapters as Verified). Pier in-container Codex
loops on DeepSWE no-network tasks. After each host patch, Pier applies it
(`apply_host_patch.py`) and runs the official task verifier. `resolved` is
true only when `verifier/reward.json` has `reward == 1`. No score is invented
if Docker/Pier is down. Each cell gets a fresh Pier job dir
(`pier/<harness>/<harness>__<model>__<task>`), and `deepswe/reward.lua` only
accepts exactly one `reward.json` inside that dir, so a task is never scored
from another task's or a stale trial's reward.

Sample matches Pier: filesystem iterdir order, then Random(seed).shuffle,
then first n. Seed 0 / n=20 starts with meriyah-explicit-resource-declarations.

## Reproducible historical scoring and usage coverage

[The full historical evaluation and isolation correction](evals/2026-10-29/REPORT.md)
retain durable grading evidence, per-image digests and all exclusions.
**Its isolation claims are withdrawn:** the old Docker collection patch
did not apply memory/PID limits or disable networking. Counts remain
historical observations, not isolated evaluation results; all 500 tasks
could behave differently under enforced isolation. The reproduced counts
are retained as bridge-networked results; no mixed-condition subset rerun
has replaced them. Four reference patches failed in that run, and OMP/pi
also retain unscored cells.

`scripts/bench/score.py bundle` exports only patches and allowlisted cell
metadata. Inspect the bundle and scan it for credentials before uploading;
never upload agent logs or environment dumps. `score.py run` uses pinned
SWE-bench Verified data and SWE-bench 5.0.2, records the actual Epoch image
digest, scores the reference patch first, then scores each harness patch in
a container whose limits are inspected before it starts. Requested and
observed configuration are separate; missing inspection evidence prevents
an isolation claim. Images are pulled and deleted one instance at a time.
The manual-only `.github/workflows/score.yml` parallelizes instances across
20 shards; it has no push or scheduled trigger.

Calibration must reproduce every historical official verdict (codex 7/10,
tk 6/10) and pass every reference patch before a full run is permitted.
Disagreement is a finding, not permission to change the expected results.
Reports retain every planned cell, distinguish missing scores from failures,
and show reference-patch failures and missing reference verdicts alongside
both planned-sample and reference-eligible denominators. An eligible-only
rate is conditional on those exclusions, not a rate for the original sample.

New generation runs record the harness executable version and SHA-256,
arguments with the prompt replaced by its digest, timeout, requested model,
effort, dataset revision and sample checksum. Historical provenance remains
explicitly unrecorded; it cannot be reconstructed from a filename.

Terminal JSON usage events are captured for codex, pi, omp and opencode.
tk/fx or unsupported event formats retain unknown usage, not zero tokens.
Interrupted runs can have incomplete subtotals; the report states coverage.
Costs are either harness estimates or calculated from an explicit `pricing`
object in a model entry: `input`, `output`, `cache_read`, `cache_write` in
USD per million tokens, plus `source` and `as_of`. Without a usable estimate
or price snapshot, cost stays unknown. These are estimates, not invoices;
subscription charges and missing historical token usage are not inferred.

### Historical OMP workspace-collection defect

The full500 OMP predictions for `sphinx-doc__sphinx-8593` and
`sphinx-doc__sphinx-9229` contain entire `.venv311` and `.venv39` trees.
This is a harness defect: prediction collection captured unrelated workspace
artifacts and could capture other files, not just virtual environments.
Both predictions are quarantined whole before upload; their cells stay in
the planned denominator as unscored, with original patch checksums recorded.

The initial secret scan flagged four Sourcegraph-token matches in the first
patch. All four are false positives on Git binary-patch index lines for Babel
`locale-data/{ce_RU,ceb,haw_US,he}.dat`. Decoding each binary payload and
recomputing its Git blob hash reproduced the flagged value exactly. These
are content hashes, not live credentials; no rotation or authentication
probe is needed. Neither environment-bearing patch was uploaded. The final
allowlisted bundle passed a complete secret scan with no findings.
