#!/usr/bin/env bash
# Cheap structural check for the SWE-bench Verified bench.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")" && pwd)
fail=0

for a in codex pi tk omp fx; do
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

python3 - "$ROOT/models.json" <<'PY'
import json, sys
cfg = json.load(open(sys.argv[1]))
assert "models" in cfg and len(cfg["models"]) >= 2
ids = {m["id"] for m in cfg["models"]}
assert "gpt-5.6-sol" in ids
for m in cfg["models"]:
    assert m.get("effort")
    assert "codex" in m and "pi" in m and "tk" in m and "omp" in m and "fx" in m
assert "omp" in cfg.get("harnesses", []) and "fx" in cfg.get("harnesses", [])
print("ok models.json", sorted(ids))
PY

"$ROOT/run.sh" --help >/dev/null
echo "ok run.sh --help"
"$ROOT/run.sh" --dry-check >/dev/null
echo "ok run.sh --dry-check"

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi
echo "bench check passed"
