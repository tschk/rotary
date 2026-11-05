#!/usr/bin/env bash
# Cheap structural check for the SWE-bench Verified + DeepSWE benches.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")" && pwd)
fail=0

if ! command -v luajit >/dev/null 2>&1; then
  echo "missing luajit (LuaJIT 2.1 is required for the benchmark glue)" >&2
  exit 1
fi

for a in codex pi tk omp fx opencode; do
  p="$ROOT/adapters/$a.sh"
  if [[ ! -f "$p" ]]; then
    echo "missing $p"; fail=1
  elif [[ ! -x "$p" ]]; then
    echo "not executable $p"; fail=1
  else
    "$p" --help >/dev/null
    echo "ok adapter $a --help"
  fi
done

luajit "$ROOT/lib/config.lua" models-check "$ROOT/models.json"

luajit - "$ROOT/lib/common.lua" <<'LUA'
local common = dofile(arg[1])
local function ensure_dir_result(first, second, third, expected)
  os.execute = function() return first, second, third end
  assert(pcall(common.ensure_dir, "mock") == expected)
end
ensure_dir_result(0, nil, nil, true)
ensure_dir_result(true, "exit", 0, true)
ensure_dir_result(1, nil, nil, false)
ensure_dir_result(false, "exit", 1, false)
print("ok ensure_dir LuaJIT return conventions")
LUA

smoke=$(mktemp -d "${TMPDIR:-/tmp}/rotary-bench-lua.XXXXXX")
trap 'rm -rf "$smoke"' EXIT
luajit "$ROOT/lib/cell.lua" --cells "$smoke/cells.jsonl" --harness tk --model smoke --instance null-cell --seconds 3
luajit "$ROOT/lib/report.lua" --cells "$smoke/cells.jsonl" --out-dir "$smoke/report" --meta '{}' >/dev/null
grep -Fq '| tk | smoke | n/a (no official eval) | — | 100% | 0/1 (0%) | NOT SCORED |' "$smoke/report/REPORT.md"
grep -Fq '| tk | smoke | null-cell | null | 3 |' "$smoke/report/REPORT.md"
echo "ok Lua cell/report null handling"
"$ROOT/test.sh"

"$ROOT/run.sh" --help >/dev/null
echo "ok run.sh --help"
"$ROOT/run.sh" --dry-check >/dev/null
echo "ok run.sh --dry-check"

if [[ -x "$ROOT/harness.sh" ]]; then
  echo "ok harness.sh present"
else
  echo "missing $ROOT/harness.sh"; fail=1
fi
if [[ -x "$ROOT/harness-live.sh" ]]; then
  echo "ok harness-live.sh present"
else
  echo "missing $ROOT/harness-live.sh"; fail=1
fi

"$ROOT/deepswe/run.sh" --help >/dev/null
echo "ok deepswe/run.sh --help"
if [[ -x "$ROOT/deepswe/thin_task.sh" && -f "$ROOT/deepswe/apply_host_patch.py" && -f "$ROOT/deepswe/sample.py" && -f "$ROOT/deepswe/report.lua" && -f "$ROOT/deepswe/reward.lua" ]]; then
  echo "ok deepswe runner files"
else
  echo "missing deepswe runner files"; fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi
echo "bench check passed"
