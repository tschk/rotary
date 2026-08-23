#!/usr/bin/env bash
# SWE-bench Verified accuracy runner (resolve rate, not toy wall-clock).
# Bash 3.2 compatible (macOS /bin/bash).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")" && pwd)
MODELS_JSON="$ROOT/models.json"
CACHE_DIR="$ROOT/.cache"
ADAPTERS="$ROOT/adapters"
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
  --harness NAME     only this harness: codex | pi | tk | omp | fx (repeatable)
  --driver MODE      auto | harbor | pier | thin  (default: auto)
  --skip-eval        produce patches only; do not run official eval
  --dry-check        print --help for runner + adapters and exit 0
  --out DIR          output directory (default: scripts/bench/out/<date>)
  --jobs-dir DIR     Harbor/Pier jobs dir (default: under --out)
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
  exit 0
fi

python3 -c "import json; json.load(open(r'''$MODELS_JSON'''))"

DEFAULT_N=$(python3 -c "import json; print(json.load(open(r'''$MODELS_JSON'''))['default_n'])")
DEFAULT_SEED=$(python3 -c "import json; print(json.load(open(r'''$MODELS_JSON'''))['default_sample_seed'])")
N=${N:-$DEFAULT_N}
SEED=${SEED:-$DEFAULT_SEED}

DATE=$(date +%Y-%m-%d)
OUT=${OUT:-"$ROOT/out/$DATE"}
mkdir -p "$OUT" "$CACHE_DIR"
JOBS_DIR=${JOBS_DIR:-"$OUT/jobs"}
CELLS="$OUT/cells.jsonl"
: > "$CELLS"
SAMPLE_JSONL="$OUT/sample.jsonl"
IDS_FILE="$OUT/instance_ids.txt"

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

python3 -c "
import json
from pathlib import Path
ids=[]
for line in Path(r'''$SAMPLE_JSONL''').read_text().splitlines():
    ids.append(json.loads(line)['instance_id'])
Path(r'''$IDS_FILE''').write_text('\n'.join(ids)+'\n')
print('sampled', len(ids), 'instances', file=__import__('sys').stderr)
"

if [ -z "$HARNESSES" ]; then
  HARNESSES=$(python3 -c "import json; print(' '.join(json.load(open(r'''$MODELS_JSON'''))['harnesses']))")
fi
for h in $HARNESSES; do
  case "$h" in
    codex|pi|tk|omp|fx) ;;
    *) echo "unknown harness: $h (only codex, pi, tk, omp, fx)" >&2; exit 2 ;;
  esac
done

MODEL_IDS=$(python3 - "$MODELS_JSON" $MODELS <<'PY'
import json, sys
cfg = json.load(open(sys.argv[1]))
want = [a for a in sys.argv[2:] if a]
ids = [m["id"] for m in cfg["models"]]
if want:
    missing = [w for w in want if w not in ids]
    if missing:
        raise SystemExit("unknown model(s): " + ", ".join(missing))
    ids = want
print(" ".join(ids))
PY
)

json_get() {
  python3 -c "import json; cfg=json.load(open(r'''$MODELS_JSON'''));
mid='$1'
print(next(m$2 for m in cfg['models'] if m['id']==mid))"
}

instance_field() {
  python3 -c "
import json,sys
iid, key = sys.argv[1], sys.argv[2]
for line in open(r'''$SAMPLE_JSONL'''):
    r=json.loads(line)
    if r['instance_id']==iid:
        print(r[key]); break
" "$1" "$2"
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
  python3 "$ROOT/lib/prompt.py" --sample "$SAMPLE_JSONL" --instance-id "$iid" --out "$dest/.bench_prompt.md"
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
  git -C "$dest" add -A >/dev/null 2>&1 || true
  git -C "$dest" diff --binary "$base" -- . ":(exclude).bench_prompt.md" > "$patch_out" || true
  if [ ! -s "$patch_out" ]; then
    git -C "$dest" diff --binary --cached "$base" -- . ":(exclude).bench_prompt.md" > "$patch_out" || true
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
  model=$(json_get "$mid" "['codex']['model']")
  mkdir -p "$JOBS_DIR"
  harbor run --dataset swebench@verified --agent codex --model "$model" \
    --n-tasks "$N" --jobs-dir "$JOBS_DIR/harbor-$mid" \
    || harbor run -d swe-bench/swe-bench-verified -a codex -m "$model" \
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
  model=$(json_get "$mid" "['codex']['model']")
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
    python3 "$ROOT/lib/cell.py" --cells "$CELLS" --harness "$harness" --model "$mid" --instance "$iid" --seconds "$seconds" --exit 2 --error checkout_failed
    return 0
  fi
  write_prompt "$iid" "$dest"
  base=$(git -C "$dest" rev-parse HEAD)
  effort=$(json_get "$mid" "['effort']")
  rc=0
  set +e
  case "$harness" in
    codex)
      model=$(json_get "$mid" "['codex']['model']")
      cfg=$(json_get "$mid" "['codex']['config']")
      BENCH_MODEL="$model" BENCH_EFFORT="$effort" BENCH_CODEX_CONFIG="$cfg" \
        "$ADAPTERS/codex.sh" "$dest" "$dest/.bench_prompt.md" >"$log" 2>&1
      rc=$?
      ;;
    pi)
      model=$(json_get "$mid" "['pi']['model']")
      prov=$(json_get "$mid" "['pi']['provider']")
      BENCH_MODEL="$model" BENCH_EFFORT="$effort" BENCH_PI_PROVIDER="$prov" \
        "$ADAPTERS/pi.sh" "$dest" "$dest/.bench_prompt.md" >"$log" 2>&1
      rc=$?
      ;;
    tk)
      model=$(json_get "$mid" "['tk']['model']")
      BENCH_MODEL="$model" BENCH_EFFORT="$effort" \
        "$ADAPTERS/tk.sh" "$dest" "$dest/.bench_prompt.md" >"$log" 2>&1
      rc=$?
      ;;
    omp)
      model=$(json_get "$mid" "['omp']['model']")
      prov=$(json_get "$mid" "['omp']['provider']")
      BENCH_MODEL="$model" BENCH_EFFORT="$effort" BENCH_OMP_PROVIDER="$prov" \
        "$ADAPTERS/omp.sh" "$dest" "$dest/.bench_prompt.md" >"$log" 2>&1
      rc=$?
      ;;
    fx)
      model=$(json_get "$mid" "['fx']['model']")
      BENCH_MODEL="$model" BENCH_EFFORT="$effort" \
        "$ADAPTERS/fx.sh" "$dest" "$dest/.bench_prompt.md" >"$log" 2>&1
      rc=$?
      ;;
  esac
  set -e
  collect_patch "$dest" "$base" "$patch"
  t1=$(date +%s)
  seconds=$((t1 - t0))
  python3 -c "
import json
cell = {
  'harness': '''$harness''',
  'model': '''$mid''',
  'instance_id': '''$iid''',
  'resolved': None,
  'seconds': $seconds,
  'adapter_exit': $rc,
  'patch': r'''$patch''',
  'driver': 'thin',
}
open(r'''$CELLS''','a').write(json.dumps(cell)+'\n')
"
}

eval_pending_patches() {
  harness=$1
  mid=$2
  preds="$OUT/preds_${harness}_${mid}.jsonl"
  python3 -c "
import json
h, m = '''$harness''', '''$mid'''
slug = '%s__%s' % (h, m)
out = r'''$preds'''
with open(out,'w') as fh:
    for line in open(r'''$CELLS'''):
        c=json.loads(line)
        if c['harness']!=h or c['model']!=m:
            continue
        patch=''
        p=c.get('patch')
        if p:
            try: patch=open(p).read()
            except OSError: patch=''
        fh.write(json.dumps({
            'instance_id': c['instance_id'],
            'model_name_or_path': slug,
            'model_patch': patch,
        })+'\n')
"
  run_id="rotary-bench-${harness}-${mid}-$(date +%Y%m%d%H%M%S)"
  set +e
  official_eval "$preds" "$run_id"
  ev=$?
  set -e
  if [ "$ev" -eq 0 ]; then
    python3 -c "
import json, glob
h, m, run_id = '''$harness''', '''$mid''', '''$run_id'''
resolved_map = {}
for path in glob.glob('logs/run_evaluation/%s/**/*' % run_id, recursive=True):
    if not path.endswith('.json'):
        continue
    try:
        data = json.load(open(path))
    except Exception:
        continue
    if isinstance(data, dict):
        for k, v in data.items():
            if isinstance(v, dict) and 'resolved' in v:
                resolved_map[k] = bool(v['resolved'])
        for key in ('resolved_ids', 'resolved_instances'):
            if key in data and isinstance(data[key], list):
                for iid in data[key]:
                    resolved_map[iid] = True
rows = [json.loads(l) for l in open(r'''$CELLS''') if l.strip()]
for r in rows:
    if r['harness']==h and r['model']==m and r['instance_id'] in resolved_map:
        r['resolved'] = resolved_map[r['instance_id']]
        r['eval_run_id'] = run_id
open(r'''$CELLS''','w').write(''.join(json.dumps(r)+'\n' for r in rows))
"
  fi
}

echo "sample: n=$N seed=$SEED" >&2
echo "models: $MODEL_IDS" >&2
echo "harnesses: $HARNESSES" >&2

for mid in $MODEL_IDS; do
  for harness in $HARNESSES; do
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
    while IFS= read -r iid; do
      [ -n "$iid" ] || continue
      echo "-- $harness $mid $iid --" >&2
      run_thin_cell "$harness" "$mid" "$iid"
    done < "$IDS_FILE"
    eval_pending_patches "$harness" "$mid"
  done
done

META=$(python3 -c "
import json
print(json.dumps({
  'dataset': 'princeton-nlp/SWE-bench_Verified',
  'n': int('''$N'''),
  'sample_seed': int('''$SEED'''),
  'driver': '''$DRIVER''',
  'docker': '''$DOCKER''',
  'models': '''$MODEL_IDS'''.split(),
  'harnesses': '''$HARNESSES'''.split(),
}))
")
python3 "$ROOT/lib/report.py" --cells "$CELLS" --out-dir "$OUT" --meta "$META"
echo "wrote $OUT/REPORT.md and $OUT/results.json" >&2
