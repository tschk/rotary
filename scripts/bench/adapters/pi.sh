#!/usr/bin/env bash
# Drive Pi coding agent against a checked-out SWE-bench task directory.
# Usage: pi.sh TASK_DIR [PROMPT_FILE]
# Env: BENCH_MODEL, BENCH_EFFORT, BENCH_PI_PROVIDER, PI_BIN
set -euo pipefail

usage() {
  cat <<'H'
pi.sh — SWE-bench Verified adapter for Pi

Usage:
  adapters/pi.sh TASK_DIR [PROMPT_FILE]

Reads the issue prompt from PROMPT_FILE or $TASK_DIR/.bench_prompt.md.
Writes a git patch or leaves the repo fixed. stdin is /dev/null.

Env:
  BENCH_MODEL           model id (default: gpt-5.6-sol)
  BENCH_EFFORT          thinking level (default: low)
  BENCH_PI_PROVIDER     Pi provider (default: openai-codex)
  PI_BIN                Pi binary (default: $HOME/.bun/bin/pi or PATH)
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
PROVIDER=${BENCH_PI_PROVIDER:-openai-codex}

if [[ -n "${PI_BIN:-}" ]]; then
  PI="$PI_BIN"
elif [[ -x "${HOME}/.bun/bin/pi" ]]; then
  PI="${HOME}/.bun/bin/pi"
else
  PI=$(command -v pi)
fi
if [[ ! -x "$PI" && ! -f "$PI" ]]; then
  echo "pi.sh: Pi CLI not found" >&2
  exit 127
fi
if [[ ! -f "$PROMPT_FILE" ]]; then
  echo "pi.sh: missing prompt file: $PROMPT_FILE" >&2
  exit 2
fi

PROMPT=$(cat "$PROMPT_FILE")
cd "$TASK_DIR"
cmd=("$PI" --provider "$PROVIDER" --model "$MODEL" --thinking "$EFFORT" --print --approve --no-session "$PROMPT")

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
