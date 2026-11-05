#!/usr/bin/env bash
# Checkout a DeepSWE task repo and run a Verified-style adapter.
# Usage: thin_task.sh HARNESS TASK_DIR OUT_PATCH
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
HARNESS=$1
TASK=$2
OUT_PATCH=$3
ADAPTERS=${BENCH_ADAPTERS:-"$ROOT/adapters"}
MODELS_JSON="$ROOT/models.json"
MID=${BENCH_MODEL_ID:?BENCH_MODEL_ID is required}
cfg() { luajit "$ROOT/lib/config.lua" model-get "$MODELS_JSON" "$MID" "$@"; }
# Resolve the model from models.json only. A missing harness entry must not
# fall through to the adapter's default model under this model's label.
if ! BENCH_MODEL=$(cfg "$HARNESS/model" 2>/dev/null) || [ -z "$BENCH_MODEL" ]; then
  echo "thin_task: model $MID has no $HARNESS entry in models.json; refusing to run" >&2
  exit 3
fi
BENCH_EFFORT=$(cfg effort)
export BENCH_MODEL BENCH_EFFORT
python3 - "$TASK" "$OUT_PATCH" "$HARNESS" <<'PY'
import os, subprocess, sys
from pathlib import Path
task, out_patch, harness = sys.argv[1], sys.argv[2], sys.argv[3]
td = Path(task)
try:
    import tomllib
except ImportError:
    import tomli as tomllib  # type: ignore
cfg = tomllib.loads((td / "task.toml").read_text())
repo = cfg["metadata"]["repository_url"]
commit = cfg["metadata"]["base_commit_hash"]
name = cfg["metadata"]["task_id"]
ws = Path(out_patch).parent / ("ws_%s_%s" % (harness, name))
if ws.exists():
    import shutil
    shutil.rmtree(ws)
ws.mkdir(parents=True)
subprocess.check_call(["git", "init"], cwd=ws, stdout=subprocess.DEVNULL)
subprocess.check_call(["git", "remote", "add", "origin", repo], cwd=ws)
subprocess.check_call(["git", "fetch", "--depth", "1", "origin", commit], cwd=ws)
subprocess.check_call(
    ["git", "-c", "advice.detachedHead=false", "checkout", "--force", "FETCH_HEAD"],
    cwd=ws,
)
prompt = (td / "instruction.md").read_text()
(ws / ".bench_prompt.md").write_text(prompt)
Path(str(out_patch) + ".ws").write_text(str(ws))
print(str(ws))
PY
WS=$(cat "${OUT_PATCH}.ws")
# Capture the base BEFORE the agent runs. The prompt tells the agent to commit
# its work; capturing after the run would make committed work invisible to the
# `git diff` below and score solved tasks as empty patches.
base=$(git -C "$WS" rev-parse HEAD)
export BENCH_AGENT_TIMEOUT=${BENCH_AGENT_TIMEOUT:-21600}
case "$HARNESS" in
  pi) export BENCH_PI_PROVIDER; BENCH_PI_PROVIDER=$(cfg pi/provider openai-codex) ;;
  omp) export BENCH_OMP_PROVIDER; BENCH_OMP_PROVIDER=$(cfg omp/provider openai-codex) ;;
  tk) export BENCH_PROVIDER; BENCH_PROVIDER=$(cfg tk/provider "") ;;
  codex) export BENCH_CODEX_CONFIG; BENCH_CODEX_CONFIG=$(cfg codex/config "model_reasoning_effort=$BENCH_EFFORT") ;;
esac
echo "$BENCH_MODEL" > "${OUT_PATCH}.model"
set +e
"$ADAPTERS/${HARNESS}.sh" "$WS" "$WS/.bench_prompt.md"
rc=$?
set -e
rm -f "$WS/.bench_prompt.md"
git -C "$WS" rm -f --ignore-unmatch --quiet .bench_prompt.md >/dev/null 2>&1 || true
rm -rf "$WS/.rx4" >/dev/null 2>&1 || true
git -C "$WS" diff --binary "$base" -- . \
  ":(exclude).bench_prompt.md" \
  ":(exclude).rx4" \
  ":(exclude).rx4/**" \
  ":(exclude).venv" \
  ":(exclude).venv/**" \
  ":(exclude)venv" \
  ":(exclude)venv/**" \
  ":(exclude)node_modules" \
  ":(exclude)node_modules/**" \
  ":(exclude)__pycache__" \
  ":(exclude)__pycache__/**" \
  ":(exclude).pytest_cache" \
  ":(exclude).pytest_cache/**" \
  ":(exclude)dist" \
  ":(exclude)dist/**" \
  ":(exclude)build" \
  ":(exclude)build/**" \
  > "$OUT_PATCH" || true
if [ ! -s "$OUT_PATCH" ]; then
  git -C "$WS" diff --binary --cached "$base" -- . \
    ":(exclude).bench_prompt.md" \
    ":(exclude).rx4" \
    ":(exclude).rx4/**" \
    ":(exclude).venv" \
    ":(exclude).venv/**" \
    ":(exclude)venv" \
    ":(exclude)venv/**" \
    ":(exclude)node_modules" \
    ":(exclude)node_modules/**" \
    ":(exclude)__pycache__" \
    ":(exclude)__pycache__/**" \
    ":(exclude).pytest_cache" \
    ":(exclude).pytest_cache/**" \
    ":(exclude)dist" \
    ":(exclude)dist/**" \
    ":(exclude)build" \
    ":(exclude)build/**" \
    > "$OUT_PATCH" || true
fi
echo "thin $HARNESS $(basename "$TASK") rc=$rc patch_bytes=$(wc -c < "$OUT_PATCH" | tr -d ' ')"
echo "$rc" > "${OUT_PATCH}.rc"
exit 0
