#!/usr/bin/env bash
# Integrity regression tests for the bench glue: denominators, model
# attribution, and per-task DeepSWE reward isolation. No network, no Docker.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/rotary-bench-test.XXXXXX")
trap 'rm -rf "$T"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
row() { grep -F "| $1 | $2 |" "$3" | head -1; }

# --- 1. Unscored and missing cells stay in the denominator. ---------------
cells="$T/cells.jsonl"
cell() { luajit "$ROOT/lib/cell.lua" --cells "$cells" --harness tk --model m --seconds 1 "$@"; }
cell --instance a --resolved true
cell --instance b --resolved false
cell --instance c --resolved null
luajit "$ROOT/lib/report.lua" --cells "$cells" --out-dir "$T/r1" >/dev/null
r=$(row tk m "$T/r1/REPORT.md")
[[ "$r" == *"| 1/3 (33%) |"* ]] || fail "unscored cell dropped from denominator: $r"
[[ "$r" == *"INCOMPLETE"* ]] || fail "partial coverage not flagged: $r"

printf '%s\n' a b c d > "$T/ids.txt"
printf '{"pairs":[{"harness":"tk","model":"m"}],"excluded":[{"harness":"codex","model":"m","reason":"not_configured"}],"instance_ids_file":"%s"}\n' "$T/ids.txt" > "$T/plan.json"
luajit "$ROOT/lib/report.lua" --cells "$cells" --plan "$T/plan.json" --out-dir "$T/r2" >/dev/null
r=$(row tk m "$T/r2/REPORT.md")
[[ "$r" == *"| 1/4 (25%) |"* ]] || fail "missing planned cell dropped from denominator: $r"
grep -Fq '| tk | m | d | missing |' "$T/r2/REPORT.md" || fail "missing cell not listed"
grep -Fq '| codex | m | not_configured |' "$T/r2/REPORT.md" || fail "excluded pair not listed"
echo "ok report denominators"

# --- 2. A model only runs on harnesses it is configured for. --------------
luajit "$ROOT/lib/config.lua" matrix "$ROOT/models.json" "gpt-5.6-sol deepseek-v4-flash" "codex opencode" > "$T/matrix.txt"
grep -qx 'run codex gpt-5.6-sol' "$T/matrix.txt" || fail "configured pair not run"
grep -qx 'run opencode deepseek-v4-flash' "$T/matrix.txt" || fail "configured pair not run"
grep -qx 'skip codex deepseek-v4-flash not_configured' "$T/matrix.txt" || fail "unconfigured pair not skipped"
grep -qx 'skip opencode gpt-5.6-sol not_configured' "$T/matrix.txt" || fail "unconfigured pair not skipped"

repo="$T/repo"
git init -q "$repo"
echo hi > "$repo/f.txt"
git -C "$repo" add f.txt
git -C "$repo" -c user.name=t -c user.email=t@t commit -qm base
task="$T/task"
mkdir -p "$task" "$T/adapters"
printf '[metadata]\nrepository_url = "file://%s"\nbase_commit_hash = "%s"\ntask_id = "t1"\n' \
  "$repo" "$(git -C "$repo" rev-parse HEAD)" > "$task/task.toml"
echo "fix it" > "$task/instruction.md"
printf '#!/usr/bin/env bash\necho "$BENCH_MODEL" > "%s/ran.$(basename "$0" .sh)"\n' "$T" > "$T/adapters/codex.sh"
chmod +x "$T/adapters/codex.sh"
if BENCH_ADAPTERS="$T/adapters" BENCH_MODEL_ID=deepseek-v4-flash \
  "$ROOT/deepswe/thin_task.sh" codex "$task" "$T/out/p.patch" >/dev/null 2>&1; then
  fail "thin_task ran codex for a model with no codex config"
fi
[[ ! -e "$T/ran.codex" ]] || fail "adapter ran under the wrong model label (BENCH_MODEL=$(cat "$T/ran.codex"))"
BENCH_ADAPTERS="$T/adapters" BENCH_MODEL_ID=gpt-5.6-sol BENCH_MODEL=leaked-env-model \
  "$ROOT/deepswe/thin_task.sh" codex "$task" "$T/out/p.patch" >/dev/null 2>&1
[[ "$(cat "$T/ran.codex")" == gpt-5.6-sol ]] || fail "adapter got model $(cat "$T/ran.codex"), want gpt-5.6-sol"
echo "ok model attribution"

# --- 3. A DeepSWE cell only reads its own job's reward.json. --------------
reward() { mkdir -p "$(dirname "$1")"; printf '{"reward": %s}\n' "$2" > "$1"; }
jobs="$T/pier/tk"
long_a="averyveryverylongtaskprefix-a"
long_b="averyveryverylongtaskprefix-b"
reward "$jobs/tk__m__$long_b/trial1/verifier/reward.json" 1
mkdir -p "$jobs/tk__m__$long_a"
[[ "$(luajit "$ROOT/deepswe/reward.lua" "$jobs/tk__m__$long_a")" == null ]] \
  || fail "task scored against another task's reward.json"
reward "$jobs/tk__m__$long_a/old/verifier/reward.json" 1
reward "$jobs/tk__m__$long_a/new/verifier/reward.json" 0
[[ "$(luajit "$ROOT/deepswe/reward.lua" "$jobs/tk__m__$long_a" 2>/dev/null)" == null ]] \
  || fail "ambiguous rewards in one job accepted"
rm -rf "$jobs/tk__m__$long_a/old"
out=$(luajit "$ROOT/deepswe/reward.lua" "$jobs/tk__m__$long_a")
[[ "$out" == *'"reward":0'* && "$out" == *"/tk__m__$long_a/new/"* ]] || fail "own reward not read: $out"
echo "ok DeepSWE reward isolation"

echo "bench integrity tests passed"
