#!/usr/bin/env bash
# Drive tk (telekinesis) against a checked-out SWE-bench task directory.
# Usage: tk.sh TASK_DIR [PROMPT_FILE]
# Env: BENCH_MODEL, BENCH_EFFORT, TK_BIN
set -euo pipefail

usage() {
  cat <<'H'
tk.sh — SWE-bench Verified adapter for tk

Usage:
  adapters/tk.sh TASK_DIR [PROMPT_FILE]

Reads the issue prompt from PROMPT_FILE or $TASK_DIR/.bench_prompt.md.
Writes a git patch or leaves the repo fixed. stdin is /dev/null.

Prefers $TK_BIN, then $TELEKINESIS_ROOT/ui/tui/target/release/tk,
then PATH tk.

Env:
  BENCH_MODEL           model id (default: glm-5.3-flash)
  BENCH_PROVIDER        tk --provider (optional; e.g. zai)
  BENCH_EFFORT          effort (default: low). Passed as --effort when tk exec
                        advertises that flag.
  TK_BIN                tk binary override
  TELEKINESIS_ROOT      telekinesis checkout (optional)
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
MODEL=${BENCH_MODEL:-glm-5.3-flash}
EFFORT=${BENCH_EFFORT:-low}
PROVIDER=${BENCH_PROVIDER:-}

PREFERRED=""
if [[ -n "${TELEKINESIS_ROOT:-}" ]]; then
  PREFERRED="${TELEKINESIS_ROOT}/ui/tui/target/release/tk"
fi
if [[ -n "${TK_BIN:-}" ]]; then
  TK="$TK_BIN"
elif [[ -n "$PREFERRED" && -x "$PREFERRED" ]]; then
  TK="$PREFERRED"
else
  TK=$(command -v tk)
fi
if [[ ! -x "$TK" ]]; then
  echo "tk.sh: tk binary not found" >&2
  exit 127
fi
if [[ ! -f "$PROMPT_FILE" ]]; then
  echo "tk.sh: missing prompt file: $PROMPT_FILE" >&2
  exit 2
fi

PROMPT=$(cat "$PROMPT_FILE")
help_text=$("$TK" exec --help 2>&1 || true)

cd "$TASK_DIR"
cmd=("$TK" exec --cwd "$TASK_DIR" --model "$MODEL")
if [[ -n "$PROVIDER" ]] && grep -q -- '--provider' <<<"$help_text"; then
  cmd+=(--provider "$PROVIDER")
fi
if grep -q -- '--effort' <<<"$help_text"; then
  cmd+=(--effort "$EFFORT")
else
  echo "tk.sh: note: tk exec has no --effort yet; model=$MODEL effort=$EFFORT (pass-through skipped)" >&2
fi
cmd+=("$PROMPT")

run() {
  "${cmd[@]}" </dev/null
}

TIMEOUT_SECS=${BENCH_AGENT_TIMEOUT:-1800}
if command -v gtimeout >/dev/null 2>&1; then
  gtimeout --signal=TERM "$TIMEOUT_SECS" "${cmd[@]}" </dev/null
elif command -v timeout >/dev/null 2>&1; then
  timeout --signal=TERM "$TIMEOUT_SECS" "${cmd[@]}" </dev/null
else
  run
fi
