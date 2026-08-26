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

Output: `scripts/bench/out/<date>/REPORT.md` and `results.json`. Each cell is
`{harness, model, instance_id, resolved, seconds}`. `resolved` is `null` when
the official eval did not run (no Docker, `--skip-eval`, or harness missing).
This bench never invents a resolve rate.

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

## fx n=10 / full 500

A mid-run adapter add does not attach to an already-started serial job
(`com.rotary.swebench.full500` tk×500). Smoke fx with
`scripts/bench/run.sh --harness fx --n 10 --model gpt-5.6-sol --out scripts/bench/out/2026-08-23-fx`.
Skip official eval if Docker is busy with that full500 eval. A full 500 fx
pass waits until the current serial run finishes.

## Resume

`run.sh` keeps `--out` cells and patches when that directory already has work
(`--resume`, also the default in that case). Use `--no-resume` to truncate
`cells.jsonl`. Existing `patches/<harness>__<model>__<instance>.patch` files are
recorded as cells and skipped.

## DeepSWE (faster live score)

DeepSWE is the live pass@1 while Verified 500 is still running.

    scripts/bench/deepswe/run.sh --n 20 --sample-seed 0 --model gpt-5.6-sol \
      --harness tk --harness codex --harness pi --harness omp --harness fx \
      --out scripts/bench/out/2026-08-23-deepswe20

Agents run host-side (same adapters as Verified). Pier in-container Codex
loops on DeepSWE no-network tasks. After each host patch, Pier applies it
(`apply_host_patch.py`) and runs the official task verifier. `resolved` is
true only when `verifier/reward.json` has `reward == 1`. No score is invented
if Docker/Pier is down.

Sample matches Pier: filesystem iterdir order, then Random(seed).shuffle,
then first n. Seed 0 / n=20 starts with meriyah-explicit-resource-declarations.
