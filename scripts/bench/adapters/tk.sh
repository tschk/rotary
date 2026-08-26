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

Prefers ~/projects/worktrees/telekinesis-rx4-consume/ui/tui/target/release/tk,
then PATH tk.

Env:
  BENCH_MODEL           model id (default: gpt-5.6-sol)
  BENCH_EFFORT          effort (default: low). Passed as --effort when tk exec
                        advertises that flag; otherwise documented pass-through
                        only (current tk exec has --model, not --effort).
  TK_BIN                tk binary override
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

PREFERRED="${HOME}/projects/worktrees/telekinesis-rx4-consume/ui/tui/target/release/tk"
if [[ -n "${TK_BIN:-}" ]]; then
  TK="$TK_BIN"
elif [[ -x "$PREFERRED" ]]; then
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
if grep -q -- '--effort' <<<"$help_text"; then
  cmd+=(--effort "$EFFORT")
else
  echo "tk.sh: note: tk exec has no --effort yet; model=$MODEL effort=$EFFORT (pass-through skipped)" >&2
fi
cmd+=("$PROMPT")

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
