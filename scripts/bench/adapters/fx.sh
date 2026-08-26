#!/usr/bin/env bash
# Drive fx (vercel-labs/fx, fx.sh) against a checked-out SWE-bench task directory.
# Usage: fx.sh TASK_DIR [PROMPT_FILE]
# Env: BENCH_MODEL, BENCH_EFFORT, FX_BIN
set -euo pipefail

usage() {
  cat <<'H'
fx.sh — SWE-bench Verified adapter for fx (vercel-labs/fx)

Usage:
  adapters/fx.sh TASK_DIR [PROMPT_FILE]

Reads the issue prompt from PROMPT_FILE or $TASK_DIR/.bench_prompt.md.
Writes a git patch or leaves the repo fixed. stdin is /dev/null.

Non-interactive: prefer `fx ask` (official). Probe --help for print/exec/cwd/model/yolo
and pass those flags when present. Default model+effort: gpt-5.6-sol / low via
--model/--effort when advertised, else FX_MODEL.

Auth: uses existing `fx login` / `fx setup` / AI_GATEWAY_API_KEY. Does not
invent keys. Unauthenticated runs fail closed (clear log).

Env:
  BENCH_MODEL           model id (default: gpt-5.6-sol)
  BENCH_EFFORT          effort (default: low)
  FX_BIN                fx binary override
  BENCH_AGENT_TIMEOUT   seconds (optional)
H
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi
if [[ $# -lt 1 ]]; then
  usage >&2
  exit 2
fi

TASK_DIR=$(cd "$1" && pwd)
PROMPT_FILE=${2:-"$TASK_DIR/.bench_prompt.md"}
MODEL=${BENCH_MODEL:-gpt-5.6-sol}
EFFORT=${BENCH_EFFORT:-low}

PREFERRED="${HOME}/.local/bin/fx"
if [[ -n "${FX_BIN:-}" ]]; then
  FX="$FX_BIN"
elif [[ -x "$PREFERRED" ]]; then
  FX="$PREFERRED"
else
  FX=$(command -v fx)
fi
if [[ ! -x "$FX" ]]; then
  echo "fx.sh: fx binary not found (expected vercel-labs/fx at ~/.local/bin/fx)" >&2
  exit 127
fi
if [[ ! -f "$PROMPT_FILE" ]]; then
  echo "fx.sh: missing prompt file: $PROMPT_FILE" >&2
  exit 2
fi

# Fail closed if fx has no credential. Do not invent keys or run login.
status_json=$("$FX" status --json 2>/dev/null || true)
auth_state=$(python3 -c "
import json, sys
raw = sys.stdin.read().strip()
if not raw:
    print('missing')
    raise SystemExit
try:
    data = json.loads(raw)
except Exception:
    print('missing')
    raise SystemExit
print(data.get('auth') or 'missing')
" <<<"$status_json")
if [[ "$auth_state" == "missing" || "$auth_state" == "none" || "$auth_state" == "unauthenticated" ]]; then
  help_msg=$(python3 -c "
import json, sys
raw = sys.stdin.read().strip()
try:
    data = json.loads(raw)
except Exception:
    data = {}
print(data.get('auth_help') or 'Run fx login to sign in, fx setup to use an API key, or set AI_GATEWAY_API_KEY.')
" <<<"$status_json")
  echo "fx.sh: not authenticated (auth=${auth_state}). Fail closed. ${help_msg}" >&2
  echo "fx.sh: binary=${FX} model=${MODEL} effort=${EFFORT}" >&2
  exit 2
fi

PROMPT=$(cat "$PROMPT_FILE")
top_help=$("$FX" --help 2>&1 || true)
ask_help=$("$FX" ask --help 2>&1 || true)
help_text="${top_help}"$'\n'"${ask_help}"

has_flag() {
  grep -q -- "$1" <<<"$help_text"
}

# Official non-interactive is `fx ask`. Fall back to print/exec only if ask is absent.
if grep -Eq '(^|[[:space:]])ask([[:space:]]|$)' <<<"$top_help"; then
  SUB=ask
elif grep -Eq '(^|[[:space:]])print([[:space:]]|$)' <<<"$top_help"; then
  SUB=print
elif grep -Eq '(^|[[:space:]])exec([[:space:]]|$)' <<<"$top_help"; then
  SUB=exec
else
  SUB=ask
fi

cd "$TASK_DIR"
export FX_AUTO_UPGRADE=0
export FX_MODEL="$MODEL"

cmd=("$FX")
if has_flag '--cwd'; then
  cmd+=(--cwd "$TASK_DIR")
fi
cmd+=("$SUB")
if has_flag '--yolo'; then
  cmd+=(--yolo)
fi
if has_flag '--no-save'; then
  cmd+=(--no-save)
fi
if has_flag '--no-color'; then
  cmd+=(--no-color)
fi
if has_flag '--model'; then
  cmd+=(--model "$MODEL")
fi
if has_flag '--effort'; then
  cmd+=(--effort "$EFFORT")
elif has_flag '--thinking'; then
  cmd+=(--thinking "$EFFORT")
else
  echo "fx.sh: note: fx has no --effort/--thinking; model=$MODEL effort=$EFFORT (FX_MODEL set, effort pass-through skipped)" >&2
fi
cmd+=(-- "$PROMPT")

echo "fx.sh: binary=${FX} auth=${auth_state} sub=${SUB} model=${MODEL} effort=${EFFORT}" >&2

run() {
  "${cmd[@]}" </dev/null
}

if [[ -n "${BENCH_AGENT_TIMEOUT:-}" ]]; then
  if command -v gtimeout >/dev/null 2>&1; then
    gtimeout --signal=TERM "$BENCH_AGENT_TIMEOUT" "${cmd[@]}" </dev/null
  elif command -v timeout >/dev/null 2>&1; then
    timeout --signal=TERM "$BENCH_AGENT_TIMEOUT" "${cmd[@]}" </dev/null
  else
    run
  fi
else
  run
fi
