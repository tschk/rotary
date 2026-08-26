#!/usr/bin/env bash
# Checkout a DeepSWE task repo and run a Verified-style adapter.
# Usage: thin_task.sh HARNESS TASK_DIR OUT_PATCH
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
HARNESS=$1
TASK=$2
OUT_PATCH=$3
ADAPTERS="$ROOT/adapters"
MODELS_JSON="$ROOT/models.json"
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
MID=${BENCH_MODEL_ID:-gpt-5.6-sol}
export BENCH_MODEL=${BENCH_MODEL:-$(python3 -c "import json; cfg=json.load(open(r'''$MODELS_JSON'''));
print(next(m['$HARNESS']['model'] for m in cfg['models'] if m['id']=='''$MID'''))")}
export BENCH_EFFORT=${BENCH_EFFORT:-$(python3 -c "import json; cfg=json.load(open(r'''$MODELS_JSON'''));
print(next(m['effort'] for m in cfg['models'] if m['id']=='''$MID'''))")}
export BENCH_AGENT_TIMEOUT=${BENCH_AGENT_TIMEOUT:-1800}
case "$HARNESS" in
  pi)
    export BENCH_PI_PROVIDER=${BENCH_PI_PROVIDER:-$(python3 -c "import json; cfg=json.load(open(r'''$MODELS_JSON'''));
print(next(m['pi'].get('provider','openai-codex') for m in cfg['models'] if m['id']=='''$MID'''))")}
    ;;
  omp)
    export BENCH_OMP_PROVIDER=${BENCH_OMP_PROVIDER:-$(python3 -c "import json; cfg=json.load(open(r'''$MODELS_JSON'''));
print(next(m['omp'].get('provider','openai-codex') for m in cfg['models'] if m['id']=='''$MID'''))")}
    ;;
  codex)
    export BENCH_CODEX_CONFIG=${BENCH_CODEX_CONFIG:-$(python3 -c "import json; cfg=json.load(open(r'''$MODELS_JSON'''));
print(next(m['codex'].get('config','model_reasoning_effort=low') for m in cfg['models'] if m['id']=='''$MID'''))")}
    ;;
esac
set +e
"$ADAPTERS/${HARNESS}.sh" "$WS" "$WS/.bench_prompt.md"
rc=$?
set -e
rm -f "$WS/.bench_prompt.md"
git -C "$WS" rm -f --ignore-unmatch --quiet .bench_prompt.md >/dev/null 2>&1 || true
base=$(git -C "$WS" rev-parse HEAD)
git -C "$WS" add -A >/dev/null 2>&1 || true
git -C "$WS" diff --binary "$base" -- . ":(exclude).bench_prompt.md" > "$OUT_PATCH" || true
if [ ! -s "$OUT_PATCH" ]; then
  git -C "$WS" diff --binary --cached "$base" -- . ":(exclude).bench_prompt.md" > "$OUT_PATCH" || true
fi
echo "thin $HARNESS $(basename "$TASK") rc=$rc patch_bytes=$(wc -c < "$OUT_PATCH" | tr -d ' ')"
echo "$rc" > "${OUT_PATCH}.rc"
exit 0
