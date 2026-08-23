#!/usr/bin/env bash
# Drive Codex CLI against a checked-out SWE-bench task directory.
# Usage: codex.sh TASK_DIR [PROMPT_FILE]
# Env: BENCH_MODEL, BENCH_EFFORT, BENCH_CODEX_CONFIG, CODEX_BIN
set -euo pipefail

usage() {
  cat <<'H'
codex.sh — SWE-bench Verified adapter for Codex CLI

Usage:
  adapters/codex.sh TASK_DIR [PROMPT_FILE]

Reads the issue prompt from PROMPT_FILE or $TASK_DIR/.bench_prompt.md.
Writes a git patch or leaves the repo fixed. stdin is /dev/null.

Env:
  BENCH_MODEL           model id (default: gpt-5.6-sol)
  BENCH_EFFORT          reasoning effort (default: low)
  BENCH_CODEX_CONFIG    -c value (default: model_reasoning_effort=$BENCH_EFFORT)
  CODEX_BIN             Codex binary (default: $HOME/.local/bin/codex or PATH)
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
CONFIG=${BENCH_CODEX_CONFIG:-"model_reasoning_effort=${EFFORT}"}

if [[ -n "${CODEX_BIN:-}" ]]; then
  CODEX="$CODEX_BIN"
elif [[ -x "${HOME}/.local/bin/codex" ]]; then
  CODEX="${HOME}/.local/bin/codex"
else
  CODEX=$(command -v codex)
fi
if [[ ! -x "$CODEX" ]]; then
  echo "codex.sh: Codex CLI not found" >&2
  exit 127
fi
if [[ ! -f "$PROMPT_FILE" ]]; then
  echo "codex.sh: missing prompt file: $PROMPT_FILE" >&2
  exit 2
fi

PROMPT=$(cat "$PROMPT_FILE")
help_text=$("$CODEX" exec --help 2>&1 || true)

cd "$TASK_DIR"
cmd=("$CODEX" exec -m "$MODEL" -c "$CONFIG")
if grep -q -- '--skip-git-repo-check' <<<"$help_text"; then
  cmd+=(--skip-git-repo-check)
fi
if grep -q -- '--full-auto' <<<"$help_text"; then
  cmd+=(--full-auto)
fi
# Prefer an explicit workspace flag; otherwise we already cd'd.
if grep -qE -- '--cd |--cd=' <<<"$help_text"; then
  cmd+=(--cd "$TASK_DIR")
elif grep -qE -- '-C |--working-dir' <<<"$help_text"; then
  cmd+=(-C "$TASK_DIR")
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
