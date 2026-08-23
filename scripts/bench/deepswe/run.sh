#!/usr/bin/env bash
# DeepSWE runner: host-side adapters (tk/codex/pi/omp/fx) + Pier task verifier.
# Bash 3.2 compatible (macOS /bin/bash).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
HERE=$(cd "$(dirname "$0")" && pwd)
MODELS_JSON="$ROOT/models.json"
export PATH="${HOME}/.local/bin:${PATH}"

usage() {
  cat <<'H'
deepswe/run.sh — DeepSWE pass@1 via host adapters + Pier verifier

Dataset: /tmp/deep-swe (or --tasks). Metric: Pier verifier reward.json
(reward==1). Does not invent a resolve rate. Host-side agents (thin) because
Pier in-container Codex loops on DeepSWE no-network tasks.

Usage:
  scripts/bench/deepswe/run.sh [options]

Options:
  --n N              sample size (default: 20)
  --sample-seed S    Pier-compatible sample (default: 0)
  --model ID         model id from models.json (default: gpt-5.6-sol)
  --harness NAME     repeatable; default tk codex pi omp fx
  --tasks DIR        DeepSWE tasks root (default: /tmp/deep-swe/tasks)
  --out DIR          output directory
  --skip-eval        host patches only; do not run Pier verifier
  --resume           keep existing cells (default if --out has work)
  --no-resume        start cells.jsonl fresh
  -h, --help
H
}

N=20
SEED=0
MID=gpt-5.6-sol
HARNESSES=""
TASKS=/tmp/deep-swe/tasks
OUT=""
SKIP_EVAL=0
RESUME=auto

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --n) N=$2; shift 2 ;;
    --sample-seed) SEED=$2; shift 2 ;;
    --model) MID=$2; shift 2 ;;
    --harness) HARNESSES="${HARNESSES} $2"; shift 2 ;;
    --tasks) TASKS=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    --skip-eval) SKIP_EVAL=1; shift ;;
    --resume) RESUME=1; shift ;;
    --no-resume) RESUME=0; shift ;;
    *) echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [ -z "$HARNESSES" ]; then
  HARNESSES="tk codex pi omp fx"
fi
for h in $HARNESSES; do
  case "$h" in
    codex|pi|tk|omp|fx) ;;
    *) echo "unknown harness: $h" >&2; exit 2 ;;
  esac
done

python3 -c "import json; json.load(open(r'''$MODELS_JSON'''))"

DATE=$(date +%Y-%m-%d)
OUT=${OUT:-"$ROOT/out/${DATE}-deepswe${N}"}
mkdir -p "$OUT" "$OUT/patches" "$OUT/logs" "$OUT/workspaces" "$OUT/pier"
CELLS="$OUT/cells.jsonl"
IDS_FILE="$OUT/task_ids.txt"

if [ "$RESUME" = auto ]; then
  if [ -s "$CELLS" ] || [ -n "$(ls -A "$OUT/patches" 2>/dev/null)" ]; then
    RESUME=1
  else
    RESUME=0
  fi
fi
if [ "$RESUME" = 1 ]; then
  echo "resume: keeping $CELLS" >&2
  touch "$CELLS"
else
  : > "$CELLS"
fi

if docker info >/dev/null 2>&1; then
  DOCKER=up
  echo "docker: up" >&2
else
  DOCKER=down
  echo "docker: down (Pier verifier needs a daemon)" >&2
fi

PIER="${HOME}/.local/bin/pier"
if [ ! -x "$PIER" ]; then
  PIER=$(command -v pier 2>/dev/null || true)
fi

SAMPLE_PY="$HERE/sample.py"
if [ "$RESUME" = 1 ] && [ -s "$IDS_FILE" ]; then
  echo "resume: reusing $IDS_FILE" >&2
else
  PIER_PY="${HOME}/.local/share/uv/tools/datacurve-pier/bin/python"
  if [ -x "$PIER_PY" ]; then
    "$PIER_PY" "$SAMPLE_PY" --tasks "$TASKS" --n "$N" --sample-seed "$SEED" --out "$IDS_FILE"
  else
    python3 "$SAMPLE_PY" --tasks "$TASKS" --n "$N" --sample-seed "$SEED" --out "$IDS_FILE"
  fi
fi

json_get() {
  python3 -c "import json; cfg=json.load(open(r'''$MODELS_JSON'''));
mid='$MID'
print(next(m$1 for m in cfg['models'] if m['id']==mid))"
}

EFFORT=$(json_get "['effort']")
export BENCH_MODEL_ID="$MID"
export BENCH_EFFORT="$EFFORT"
export BENCH_AGENT_TIMEOUT=${BENCH_AGENT_TIMEOUT:-1800}

cell_exists() {
  python3 -c "
import json,sys
from pathlib import Path
h,m,iid,cells=sys.argv[1:5]
if Path(cells).exists():
    for line in Path(cells).read_text().splitlines():
        if not line.strip():
            continue
        c=json.loads(line)
        if c.get('harness')==h and c.get('model')==m and c.get('instance_id')==iid:
            raise SystemExit(0)
raise SystemExit(1)
" "$1" "$2" "$3" "$CELLS"
}

write_live_report() {
  python3 "$HERE/report.py" --cells "$CELLS" --out-dir "$OUT" --meta "$(python3 -c "
import json
print(json.dumps({
  'dataset': 'datacurve/deep-swe',
  'n': int('''$N'''),
  'sample_seed': int('''$SEED'''),
  'docker': '''$DOCKER''',
  'model': '''$MID''',
  'effort': '''$EFFORT''',
  'harnesses': '''$HARNESSES'''.split(),
  'driver': 'thin+pier-verifier',
  'tasks': '''$TASKS''',
}))
")"
}

pier_verify() {
  harness=$1
  iid=$2
  patch=$3
  job_parent="$OUT/pier/${harness}"
  mkdir -p "$job_parent"
  if [ -z "$PIER" ] || [ ! -x "$PIER" ]; then
    echo "pier missing; skip verifier" >&2
    return 1
  fi
  if [ "$DOCKER" != up ]; then
    echo "docker down; skip verifier" >&2
    return 1
  fi
  export PYTHONPATH="$HERE${PYTHONPATH:+:$PYTHONPATH}"
  export DOCKER_DEFAULT_PLATFORM="${DOCKER_DEFAULT_PLATFORM:-linux/amd64}"
  job_name="${harness}__${MID}__${iid}"
  set +e
  "$PIER" run \
    -p "$TASKS/$iid" \
    --agent-import-path apply_host_patch:ApplyHostPatchAgent \
    --ak "patch_file=${patch}" \
    -m "$MID" \
    -n 1 -y \
    -o "$job_parent" \
    --job-name "$job_name"
  prc=$?
  set -e
  echo "$prc" > "$job_parent/${job_name}.pier_rc"
  return 0
}

parse_reward() {
  job_parent=$1
  iid=$2
  python3 - "$job_parent" "$iid" <<'PY'
import json, sys
from pathlib import Path
root, iid = Path(sys.argv[1]), sys.argv[2]
hits = sorted(root.rglob("reward.json"), key=lambda p: p.stat().st_mtime, reverse=True)
# Prefer a reward whose trial path mentions the task id.
chosen = None
for p in hits:
    if iid.replace("_", "")[:20] in str(p).replace("_", "") or iid in str(p):
        chosen = p
        break
if chosen is None and hits:
    chosen = hits[0]
if chosen is None:
    print("null")
    raise SystemExit(0)
data = json.loads(chosen.read_text())
reward = data.get("reward")
print(json.dumps({"reward": reward, "path": str(chosen), "f2p": data.get("f2p"), "p2p": data.get("p2p")}))
PY
}

echo "DeepSWE sample: n=$N seed=$SEED model=$MID effort=$EFFORT" >&2
echo "harnesses:$HARNESSES" >&2
echo "tasks: $TASKS" >&2
echo "out: $OUT" >&2

for harness in $HARNESSES; do
  echo "== $harness x $MID ==" >&2
  while IFS= read -r iid; do
    [ -n "$iid" ] || continue
    if cell_exists "$harness" "$MID" "$iid"; then
      echo "resume skip $harness $MID $iid" >&2
      continue
    fi
    echo "-- $harness $MID $iid --" >&2
    patch="$OUT/patches/${harness}__${MID}__${iid}.patch"
    log="$OUT/logs/${harness}__${MID}__${iid}.log"
    t0=$(date +%s)
    set +e
    "$HERE/thin_task.sh" "$harness" "$TASKS/$iid" "$patch" >"$log" 2>&1
    thin_rc=$?
    set -e
    adapter_exit=0
    if [ -f "${patch}.rc" ]; then
      adapter_exit=$(cat "${patch}.rc")
    elif [ "$thin_rc" -ne 0 ]; then
      adapter_exit=$thin_rc
    fi
    resolved_py="None"
    reward_path=""
    verifier=""
    if [ "$SKIP_EVAL" -eq 0 ]; then
      pier_verify "$harness" "$iid" "$patch" >>"$log" 2>&1 || true
      reward_json=$(parse_reward "$OUT/pier/${harness}" "$iid" || echo null)
      if [ "$reward_json" != "null" ]; then
        verifier=pier
        reward_path=$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('path',''))" "$reward_json")
        rew=$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('reward'))" "$reward_json")
        if [ "$rew" = "1" ] || [ "$rew" = "1.0" ]; then
          resolved_py="True"
        else
          resolved_py="False"
        fi
      fi
    fi
    t1=$(date +%s)
    seconds=$((t1 - t0))
    python3 -c "
import json
cell = {
  'harness': '''$harness''',
  'model': '''$MID''',
  'instance_id': '''$iid''',
  'resolved': $resolved_py,
  'seconds': $seconds,
  'adapter_exit': $adapter_exit,
  'patch': r'''$patch''',
  'driver': 'thin+pier-verifier',
  'dataset': 'deep-swe',
}
if '''$reward_path''':
    cell['reward_json'] = r'''$reward_path'''
if '''$verifier''':
    cell['verifier'] = '''$verifier'''
open(r'''$CELLS''','a').write(json.dumps(cell)+'\n')
"
    write_live_report >/dev/null || true
  done < "$IDS_FILE"
done

write_live_report
echo "wrote $OUT/REPORT.md and $OUT/results.json" >&2
