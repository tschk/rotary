#!/usr/bin/env bash
# DeepSWE runner: host-side adapters (tk/codex/pi/omp/fx/opencode) + Pier task verifier.
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
  --harness NAME     repeatable; default tk codex pi omp fx (opencode opt-in)
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
    codex|pi|tk|omp|fx|opencode) ;;
    *) echo "unknown harness: $h" >&2; exit 2 ;;
  esac
done

luajit "$ROOT/lib/config.lua" json-valid "$MODELS_JSON"

DATE=$(date +%Y-%m-%d)
OUT=${OUT:-"$ROOT/out/${DATE}-deepswe${N}"}
mkdir -p "$OUT" "$OUT/patches" "$OUT/logs" "$OUT/workspaces" "$OUT/pier"
OUT=$(cd "$OUT" && pwd)
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

if [ ! -s "$IDS_FILE" ]; then
  echo "empty sample: $IDS_FILE has no ids; refusing to write a 0-cell report" >&2
  exit 1
fi

json_get() {
  if [ "$#" -gt 1 ]; then
    luajit "$ROOT/lib/config.lua" model-get "$MODELS_JSON" "$MID" "$1" "$2"
  else
    luajit "$ROOT/lib/config.lua" model-get "$MODELS_JSON" "$MID" "$1"
  fi
}

EFFORT=$(json_get effort)
export BENCH_MODEL_ID="$MID"
PLAN="$OUT/plan.json"
# Only harnesses with their own model entry for $MID run; others are listed
# as exclusions in the report instead of running an adapter default model.
RUN_HARNESSES=""
while read -r verb h _m reason; do
  if [ "$verb" = run ]; then
    RUN_HARNESSES="$RUN_HARNESSES $h"
  else
    echo "skip $h: $MID $reason" >&2
  fi
done <<EOF
$(luajit "$ROOT/lib/config.lua" matrix "$MODELS_JSON" "$MID" "$HARNESSES" "$IDS_FILE" "$PLAN")
EOF
export BENCH_AGENT_TIMEOUT=${BENCH_AGENT_TIMEOUT:-21600}
export TK_MAX_TURNS=${TK_MAX_TURNS:-400}
export BENCH_PROVENANCE
BENCH_PROVENANCE=$(luajit "$ROOT/lib/config.lua" provenance "$ROOT" "$MODELS_JSON" "$IDS_FILE")

cell_exists() {
  luajit "$ROOT/lib/config.lua" cell-exists "$CELLS" "$1" "$2" "$3"
}

write_live_report() {
  local meta
  meta=$(luajit "$ROOT/lib/config.lua" deep-meta-json "$N" "$SEED" "$DOCKER" "$MID" "$EFFORT" "$HARNESSES" "$TASKS")
  luajit "$HERE/report.lua" --cells "$CELLS" --out-dir "$OUT" --meta "$meta" --plan "$PLAN"
}

pier_verify() {
  harness=$1
  iid=$2
  patch=$3
  job_parent="$OUT/pier/${harness}"
  job_name="${harness}__${MID}__${iid}"
  mkdir -p "$job_parent"
  # Fresh job dir per cell, cleared even when the verifier is skipped, so a
  # stale trial can never be read as this one.
  # Called in an `if`, so set -e does not apply. Check the outcome, not rm's
  # exit status: if anything of an old job survives, refuse to verify.
  rm -rf "${job_parent:?}/$job_name"
  if [ -e "$job_parent/$job_name" ]; then
    echo "cannot clear $job_parent/$job_name; refusing to verify" >&2
    return 1
  fi
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
  set +e
  # Cap the verifier env's memory: task test suites can balloon
  # (pystan fits hit 5.5GB) and thrash the host for every other trial.
  "$PIER" run \
    -p "$TASKS/$iid" \
    --agent-import-path apply_host_patch:ApplyHostPatchAgent \
    --ak "patch_file=${patch}" \
    -m "$MID" \
    -n 1 -y \
    --override-memory-mb "${BENCH_ENV_MEM_MB:-3072}" \
    -o "$job_parent" \
    --job-name "$job_name"
  prc=$?
  set -e
  echo "$prc" > "$job_parent/${job_name}.pier_rc"
  return 0
}

echo "DeepSWE sample: n=$N seed=$SEED model=$MID effort=$EFFORT" >&2
echo "harnesses:$HARNESSES" >&2
echo "tasks: $TASKS" >&2
echo "out: $OUT" >&2

for harness in $RUN_HARNESSES; do
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
    export BENCH_CELL_METADATA="$OUT/logs/${harness}__${MID}__${iid}.metadata.json"
    export BENCH_MODEL_ID="$MID"
    rm -f "$BENCH_CELL_METADATA"
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
    resolved="null"
    reward_path=""
    verifier=""
    unscored=not_verified
    adapter_model=$(cat "${patch}.model" 2>/dev/null || true)
    if [ "$SKIP_EVAL" -eq 0 ]; then
      # Read a reward only when this attempt actually ran the verifier.
      if pier_verify "$harness" "$iid" "$patch" >>"$log" 2>&1; then
        reward_json=$(luajit "$HERE/reward.lua" "$OUT/pier/${harness}/${harness}__${MID}__${iid}" 2>>"$log" || echo null)
        [ "$reward_json" = "null" ] && unscored=verifier_no_reward
      else
        reward_json=null
      fi
      if [ "$reward_json" != "null" ]; then
        unscored=""
        verifier=pier
        reward_path=$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('path',''))" "$reward_json")
        rew=$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('reward'))" "$reward_json")
        if [ "$rew" = "1" ] || [ "$rew" = "1.0" ]; then
          resolved="true"
        else
          resolved="false"
        fi
      fi
    fi
    t1=$(date +%s)
    seconds=$((t1 - t0))
    luajit "$ROOT/lib/cell.lua" --cells "$CELLS" --harness "$harness" --model "$MID" --instance "$iid" --seconds "$seconds" --exit "$adapter_exit" --resolved "$resolved" --patch "$patch" --driver thin+pier-verifier --dataset deep-swe --reward-json "$reward_path" --verifier "$verifier" --adapter-model "$adapter_model" --effort "$EFFORT" --unscored-reason "$unscored" --execution-json "$BENCH_CELL_METADATA"
    write_live_report >/dev/null || true
  done < "$IDS_FILE"
done

write_live_report
echo "wrote $OUT/REPORT.md and $OUT/results.json" >&2
