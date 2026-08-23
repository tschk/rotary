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

Only these three, same model + effort on each:

| harness | binary | flags |
|---|---|---|
| Codex CLI | `$HOME/.local/bin/codex` | `-m <model> -c model_reasoning_effort=<effort>` |
| Pi | `$HOME/.bun/bin/pi` | `--provider openai-codex --model <model> --thinking <effort>` |
| tk | prefer `~/projects/worktrees/telekinesis-rx4-consume/ui/tui/target/release/tk`, else PATH `tk` | `exec --model <model> --effort <effort>` when that flag exists; otherwise `--effort` is passed through only if `tk exec --help` lists it |

Adapters: `scripts/bench/adapters/{codex,pi,tk}.sh`. They take `TASK_DIR` plus
the issue prompt and write a git patch or leave the repo fixed. stdin is
`/dev/null`.

Models live in `scripts/bench/models.json` (default: `gpt-5.6-sol` low from
`~/.codex/config.toml`, plus `gpt-5.6-terra` low from the Codex model cache).

## Drivers

`run.sh` tries Harbor (`uv tool install harbor`, `harbor download swebench-verified`
/ `harbor datasets download swe-bench@verified`) for the dataset, then Pier
(`~/.local/bin/pier`) if you pass `--driver pier`.

Harbor and Pier ship a built-in `codex` agent. They do **not** drive Pi or tk,
so those always use the thin loop:

1. Sample n instances with `--sample-seed` (default 0).
2. Check out the instance repo at `base_commit`.
3. Feed the issue text to the adapter.
4. Keep the working-tree fix / collect a git patch.
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
