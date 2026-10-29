#!/usr/bin/env bash
# SWE-bench Verified accuracy runner (resolve rate, not toy wall-clock).
# Bash 3.2 compatible (macOS /bin/bash).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")" && pwd)
MODELS_JSON="$ROOT/models.json"
CACHE_DIR="$ROOT/.cache"
ADAPTERS=${BENCH_ADAPTERS:-"$ROOT/adapters"}
export PATH="${HOME}/.local/bin:${PATH}"

usage() {
  cat <<'H'
run.sh — SWE-bench Verified accuracy bench

Dataset: princeton-nlp/SWE-bench Verified (Harbor swebench-verified).
Metric: resolve rate (fail-to-pass tests). Not wall-clock on toy tasks.

Usage:
  scripts/bench/run.sh [options]

Options:
  --n N              sample size (default: 10). Full 500 is opt-in: --n 500 or --full
  --sample-seed S    deterministic sample (default: 0)
  --full             alias for --n 500
  --model ID         only this model id from models.json (repeatable)
  --harness NAME     only this harness: codex | pi | tk | omp | fx | opencode (repeatable)
  --driver MODE      auto | harbor | pier | thin  (default: auto)
  --skip-eval        produce patches only; do not run official eval
  --dry-check        print --help for runner + adapters and exit 0
  --out DIR          output directory (default: scripts/bench/out/<date>)
  --jobs-dir DIR     Harbor/Pier jobs dir (default: under --out)
  --resume           keep existing cells/patches in --out and skip completed
  --no-resume        truncate cells.jsonl even if --out already has work
  -h, --help         this help

Matrix: every selected model x every selected harness (same model+effort).
H
}

N=""
SEED=""
DRIVER=auto
SKIP_EVAL=0
DRY=0
OUT=""
JOBS_DIR=""
MODELS=""
HARNESSES=""
RESUME=auto

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --n) N=$2; shift 2 ;;
    --sample-seed) SEED=$2; shift 2 ;;
    --full) N=500; shift ;;
    --model) MODELS="${MODELS} $2"; shift 2 ;;
    --harness) HARNESSES="${HARNESSES} $2"; shift 2 ;;
    --driver) DRIVER=$2; shift 2 ;;
    --skip-eval) SKIP_EVAL=1; shift ;;
    --dry-check) DRY=1; shift ;;
    --out) OUT=$2; shift 2 ;;
    --jobs-dir) JOBS_DIR=$2; shift 2 ;;
    --resume) RESUME=1; shift ;;
    --no-resume) RESUME=0; shift ;;
    *) echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [ "$DRY" -eq 1 ]; then
  usage
  echo
  "$ADAPTERS/codex.sh" --help
  echo
  "$ADAPTERS/pi.sh" --help
  echo
  "$ADAPTERS/tk.sh" --help
  echo
  "$ADAPTERS/omp.sh" --help
  echo
  "$ADAPTERS/fx.sh" --help
  echo
  "$ADAPTERS/opencode.sh" --help
  exit 0
fi

luajit "$ROOT/lib/config.lua" json-valid "$MODELS_JSON"

DEFAULT_N=$(luajit "$ROOT/lib/config.lua" default "$MODELS_JSON" default_n)
DEFAULT_SEED=$(luajit "$ROOT/lib/config.lua" default "$MODELS_JSON" default_sample_seed)
N=${N:-$DEFAULT_N}
SEED=${SEED:-$DEFAULT_SEED}

DATE=$(date +%Y-%m-%d)
OUT=${OUT:-"$ROOT/out/$DATE"}
mkdir -p "$OUT" "$CACHE_DIR"
# Match Z.ai Flash protocol (6h). Inherited 1800s from older jobs starves SWE.
export BENCH_AGENT_TIMEOUT=${BENCH_AGENT_TIMEOUT:-21600}
export TK_MAX_TURNS=${TK_MAX_TURNS:-400}
JOBS_DIR=${JOBS_DIR:-"$OUT/jobs"}
CELLS="$OUT/cells.jsonl"
SAMPLE_JSONL="$OUT/sample.jsonl"
IDS_FILE="$OUT/instance_ids.txt"

if [ "$RESUME" = auto ]; then
  if [ -s "$CELLS" ] || [ -n "$(ls -A "$OUT/patches" 2>/dev/null)" ]; then
    RESUME=1
  else
    RESUME=0
  fi
fi
if [ "$RESUME" = 1 ]; then
  echo "resume: keeping $CELLS and existing patches" >&2
  touch "$CELLS"
else
  : > "$CELLS"
fi

if docker info >/dev/null 2>&1; then
  DOCKER=up
  echo "docker: up" >&2
else
  DOCKER=down
  echo "docker: down (Harbor/Pier/official eval need a daemon; will still produce patches)" >&2
fi

ensure_harbor() {
  if command -v harbor >/dev/null 2>&1; then
    return 0
  fi
  if [ -x "${HOME}/.local/bin/harbor" ]; then
    return 0
  fi
  if command -v uv >/dev/null 2>&1; then
    uv tool install harbor || return 1
  else
    return 1
  fi
  command -v harbor >/dev/null 2>&1 || [ -x "${HOME}/.local/bin/harbor" ]
}

harbor_download() {
  ensure_harbor || return 1
  harbor datasets download swe-bench@verified \
    || harbor datasets download swebench-verified \
    || harbor download swebench-verified \
    || return 1
}

if harbor_download; then
  echo "Harbor dataset download: ok" >&2
else
  echo "note: Harbor dataset download skipped or unavailable" >&2
fi

if [ "$RESUME" = 1 ] && [ -s "$SAMPLE_JSONL" ] && [ -s "$IDS_FILE" ]; then
  echo "resume: reusing existing sample $SAMPLE_JSONL" >&2
else
set +e
python3 "$ROOT/lib/dataset.py" --cache "$CACHE_DIR/swebench-verified.jsonl" --n "$N" --sample-seed "$SEED" --out "$SAMPLE_JSONL"
ds_rc=$?
set -e
if [ "$ds_rc" -ne 0 ]; then
  echo "trying uv run --with datasets ..." >&2
  if command -v uv >/dev/null 2>&1; then
    uv run --with datasets python3 "$ROOT/lib/dataset.py" \
      --cache "$CACHE_DIR/swebench-verified.jsonl" --n "$N" --sample-seed "$SEED" --out "$SAMPLE_JSONL"
  else
    echo "cannot download SWE-bench Verified (need python datasets or uv)" >&2
    exit 1
  fi
fi

luajit "$ROOT/lib/config.lua" sample-ids "$SAMPLE_JSONL" "$IDS_FILE"
fi

if [ ! -s "$IDS_FILE" ]; then
  echo "empty sample: $IDS_FILE has no ids; refusing to write a 0-cell report" >&2
  exit 1
fi

if [ -z "$HARNESSES" ]; then
  HARNESSES=$(luajit "$ROOT/lib/config.lua" harnesses "$MODELS_JSON")
fi
for h in $HARNESSES; do
  case "$h" in
    codex|pi|tk|omp|fx|opencode) ;;
    *) echo "unknown harness: $h (only codex, pi, tk, omp, fx, opencode)" >&2; exit 2 ;;
  esac
done

MODEL_IDS=$(luajit "$ROOT/lib/config.lua" model-ids "$MODELS_JSON" $MODELS)
PLAN="$OUT/plan.json"
# Only model x harness pairs with their own model entry run; the rest are
# reported as exclusions instead of running an adapter's default model.
MATRIX=$(luajit "$ROOT/lib/config.lua" matrix "$MODELS_JSON" "$MODEL_IDS" "$HARNESSES" "$IDS_FILE" "$PLAN")
export BENCH_PROVENANCE
BENCH_PROVENANCE=$(luajit "$ROOT/lib/config.lua" provenance "$ROOT" "$MODELS_JSON" "$IDS_FILE")

json_get() {
  if [ "$#" -gt 2 ]; then
    luajit "$ROOT/lib/config.lua" model-get "$MODELS_JSON" "$1" "$2" "$3"
  else
    luajit "$ROOT/lib/config.lua" model-get "$MODELS_JSON" "$1" "$2"
  fi
}

instance_field() {
  luajit "$ROOT/lib/config.lua" instance-field "$SAMPLE_JSONL" "$1" "$2"
}

checkout_instance() {
  iid=$1
  dest=$2
  repo=$(instance_field "$iid" repo)
  commit=$(instance_field "$iid" base_commit)
  rm -rf "$dest"
  mkdir -p "$dest"
  git -C "$dest" init >/dev/null
  git -C "$dest" remote add origin "https://github.com/${repo}.git"
  git -C "$dest" fetch --depth 1 origin "$commit"
  git -C "$dest" -c advice.detachedHead=false checkout --force FETCH_HEAD
}

write_prompt() {
  iid=$1
  dest=$2
  luajit "$ROOT/lib/prompt.lua" --sample "$SAMPLE_JSONL" --instance-id "$iid" --out "$dest/.bench_prompt.md"
}

# Runner-owned helpers that must never enter model_patch / official eval.
BENCH_HELPER_FILES=".bench_prompt.md"

collect_patch() {
  dest=$1
  base=$2
  patch_out=$3
  # Strip files the runner planted (prompt, etc.) before collecting the agent patch.
  for helper in $BENCH_HELPER_FILES; do
    rm -f "$dest/$helper"
    git -C "$dest" rm -f --ignore-unmatch --quiet "$helper" >/dev/null 2>&1 || true
  done
  rm -rf "$dest/.rx4" >/dev/null 2>&1 || true
  git -C "$dest" add -A >/dev/null 2>&1 || true
  local excludes=(
    ":(exclude).bench_prompt.md"
    ":(exclude).rx4" ":(exclude).rx4/**"
    ":(exclude).venv" ":(exclude).venv/**"
    ":(exclude)venv" ":(exclude)venv/**"
    ":(exclude)node_modules" ":(exclude)node_modules/**"
    ":(exclude)__pycache__" ":(exclude)__pycache__/**"
    ":(exclude).pytest_cache" ":(exclude).pytest_cache/**"
    ":(exclude)dist" ":(exclude)dist/**"
    ":(exclude)build" ":(exclude)build/**"
  )
  git -C "$dest" diff --binary "$base" -- . "${excludes[@]}" > "$patch_out" || true
  if [ ! -s "$patch_out" ]; then
    git -C "$dest" diff --binary --cached "$base" -- . "${excludes[@]}" > "$patch_out" || true
  fi
}

official_eval() {
  preds=$1
  run_id=$2
  if [ "$SKIP_EVAL" -eq 1 ]; then
    return 1
  fi
  if [ "$DOCKER" != up ]; then
    echo "official eval skipped: docker down" >&2
    return 1
  fi
  ids=$(tr '\n' ' ' < "$IDS_FILE")
  if python3 -c "import swebench.harness.run_evaluation" 2>/dev/null; then
    python3 -m swebench.harness.run_evaluation \
      --dataset_name SWE-bench/SWE-bench_Verified \
      --split test \
      --predictions_path "$preds" \
      --instance_ids $ids \
      --max_workers 1 \
      --run_id "$run_id" || return 1
  elif command -v uv >/dev/null 2>&1; then
    uv run --with swebench python -m swebench.harness.run_evaluation \
      --dataset_name SWE-bench/SWE-bench_Verified \
      --split test \
      --predictions_path "$preds" \
      --instance_ids $ids \
      --max_workers 1 \
      --run_id "$run_id" || return 1
  else
    echo "swebench harness not installed" >&2
    return 1
  fi
}

harbor_try_codex() {
  mid=$1
  ensure_harbor || return 1
  [ "$DOCKER" = up ] || return 1
  model=$(json_get "$mid" codex/model)
  mkdir -p "$JOBS_DIR"
  # Cap each task env's memory: one runaway task process must not
  # thrash the host for every concurrent trial (8GB boxes).
  harbor run --dataset swebench@verified --agent codex --model "$model" \
    --override-memory-mb "${BENCH_ENV_MEM_MB:-3072}" \
    --n-tasks "$N" --jobs-dir "$JOBS_DIR/harbor-$mid" \
    || harbor run -d swe-bench/swe-bench-verified -a codex -m "$model" \
         --override-memory-mb "${BENCH_ENV_MEM_MB:-3072}" \
         --n-tasks "$N" -o "$JOBS_DIR/harbor-$mid"
}

pier_try_codex() {
  mid=$1
  pier="${HOME}/.local/bin/pier"
  if [ ! -x "$pier" ]; then
    pier=$(command -v pier 2>/dev/null || true)
  fi
  [ -n "$pier" ] && [ -x "$pier" ] || return 1
  [ "$DOCKER" = up ] || return 1
  model=$(json_get "$mid" codex/model)
  "$pier" run -a codex -m "$model" -n 1 -o "$JOBS_DIR/pier-$mid"
}

run_thin_cell() {
  harness=$1
  mid=$2
  iid=$3
  dest="$OUT/workspaces/${harness}__${mid}__${iid}"
  mkdir -p "$OUT/workspaces" "$OUT/patches" "$OUT/logs"
  log="$OUT/logs/${harness}__${mid}__${iid}.log"
  patch="$OUT/patches/${harness}__${mid}__${iid}.patch"
  t0=$(date +%s)
  if ! checkout_instance "$iid" "$dest"; then
    echo "checkout failed: $iid" >&2
    t1=$(date +%s)
    seconds=$((t1 - t0))
    luajit "$ROOT/lib/cell.lua" --cells "$CELLS" --harness "$harness" --model "$mid" --instance "$iid" --seconds "$seconds" --exit 2 --error checkout_failed
    return 0
  fi
  write_prompt "$iid" "$dest"
  base=$(git -C "$dest" rev-parse HEAD)
  effort=$(json_get "$mid" effort)
  # Strict lookup: the matrix only schedules configured pairs, and an empty
  # model would let the adapter run its own default under this label.
  model=$(json_get "$mid" "$harness/model")
  rc=0
  set +e
  case "$harness" in
    codex)
      cfg=$(json_get "$mid" codex/config "model_reasoning_effort=$effort")
      BENCH_MODEL="$model" BENCH_EFFORT="$effort" BENCH_CODEX_CONFIG="$cfg" \
        "$ADAPTERS/codex.sh" "$dest" "$dest/.bench_prompt.md" >"$log" 2>&1
      rc=$?
      ;;
    pi)
      prov=$(json_get "$mid" pi/provider openai-codex)
      BENCH_MODEL="$model" BENCH_EFFORT="$effort" BENCH_PI_PROVIDER="$prov" \
        "$ADAPTERS/pi.sh" "$dest" "$dest/.bench_prompt.md" >"$log" 2>&1
      rc=$?
      ;;
    tk)
      prov=$(json_get "$mid" tk/provider "")
      BENCH_MODEL="$model" BENCH_EFFORT="$effort" BENCH_PROVIDER="$prov" \
        "$ADAPTERS/tk.sh" "$dest" "$dest/.bench_prompt.md" >"$log" 2>&1
      rc=$?
      ;;
    omp)
      prov=$(json_get "$mid" omp/provider openai-codex)
      BENCH_MODEL="$model" BENCH_EFFORT="$effort" BENCH_OMP_PROVIDER="$prov" \
        "$ADAPTERS/omp.sh" "$dest" "$dest/.bench_prompt.md" >"$log" 2>&1
      rc=$?
      ;;
    fx)
      BENCH_MODEL="$model" BENCH_EFFORT="$effort" \
        "$ADAPTERS/fx.sh" "$dest" "$dest/.bench_prompt.md" >"$log" 2>&1
      rc=$?
      ;;
    opencode)
      BENCH_MODEL="$model" BENCH_EFFORT="$effort" \
        "$ADAPTERS/opencode.sh" "$dest" "$dest/.bench_prompt.md" >"$log" 2>&1
      rc=$?
      ;;
  esac
  set -e
  collect_patch "$dest" "$base" "$patch"
  t1=$(date +%s)
  seconds=$((t1 - t0))
  luajit "$ROOT/lib/cell.lua" --cells "$CELLS" --harness "$harness" --model "$mid" --instance "$iid" --seconds "$seconds" --exit "$rc" --patch "$patch" --adapter-model "$model" --effort "$effort"
}

eval_pending_patches() {
  harness=$1
  mid=$2
  preds="$OUT/preds_${harness}_${mid}.jsonl"
  luajit "$ROOT/lib/config.lua" predictions "$CELLS" "$preds" "$harness" "$mid"
  run_id="rotary-bench-${harness}-${mid}-$(date +%Y%m%d%H%M%S)"
  set +e
  official_eval "$preds" "$run_id"
  ev=$?
  set -e
  if [ "$ev" -eq 0 ]; then
    luajit "$ROOT/lib/config.lua" apply-eval "$CELLS" "$harness" "$mid" "$run_id"
  fi
}

echo "sample: n=$N seed=$SEED" >&2
echo "models: $MODEL_IDS" >&2
echo "harnesses: $HARNESSES" >&2

echo "$MATRIX" | grep '^skip ' >&2 || true
while read -r verb harness mid _reason <&3; do
    [ "$verb" = run ] || continue
    echo "== $harness x $mid ==" >&2
    used=thin
    if [ "$harness" = codex ] && [ "$DRIVER" != thin ] && [ "$DRIVER" != auto ]; then
      if [ "$DRIVER" = harbor ]; then
        if harbor_try_codex "$mid"; then
          used=harbor
        fi
      fi
      if [ "$used" = thin ] && [ "$DRIVER" = pier ]; then
        if pier_try_codex "$mid"; then
          used=pier
        fi
      fi
    fi
    if [ "$DRIVER" = harbor ] || [ "$DRIVER" = pier ]; then
      if [ "$used" != thin ]; then
        echo "note: $used drove Codex; Harbor/Pier cannot drive pi/tk/omp/fx" >&2
        continue
      fi
      echo "$DRIVER could not drive $harness; falling back to thin loop" >&2
    fi
    luajit "$ROOT/lib/config.lua" resume-patches "$CELLS" "$OUT" "$harness" "$mid"
    while IFS= read -r iid; do
      [ -n "$iid" ] || continue
      if luajit "$ROOT/lib/config.lua" cell-exists "$CELLS" "$harness" "$mid" "$iid"; then
        echo "resume skip $harness $mid $iid" >&2
        continue
      fi
      echo "-- $harness $mid $iid --" >&2
      run_thin_cell "$harness" "$mid" "$iid"
    done < "$IDS_FILE"
    eval_pending_patches "$harness" "$mid"
done 3<<EOF
$MATRIX
EOF

META=$(luajit "$ROOT/lib/config.lua" meta-json "$N" "$SEED" "$DRIVER" "$DOCKER" "$MODEL_IDS" "$HARNESSES")
luajit "$ROOT/lib/report.lua" --cells "$CELLS" --out-dir "$OUT" --meta "$META" --plan "$PLAN"
echo "wrote $OUT/REPORT.md and $OUT/results.json" >&2
