#!/usr/bin/env bash
# Drive OpenCode against a checked-out SWE-bench / DeepSWE task directory.
# Usage: opencode.sh TASK_DIR [PROMPT_FILE]
# Env: BENCH_MODEL, OPENCODE_BIN, BENCH_AGENT_TIMEOUT
# OpenCode reads ~/.local/share/opencode/auth.json itself. Do not echo keys.
set -euo pipefail

usage() {
  cat <<'H'
opencode.sh — SWE-bench / DeepSWE adapter for OpenCode

Usage:
  adapters/opencode.sh TASK_DIR [PROMPT_FILE]

Reads the issue prompt from PROMPT_FILE or $TASK_DIR/.bench_prompt.md.
Writes a git patch or leaves the repo fixed. stdin is /dev/null.

Non-interactive:
  opencode run --auto --dir TASK_DIR -m MODEL "$(cat prompt)" </dev/null

Auth: OpenCode reads ~/.local/share/opencode/auth.json. This adapter never
prints or exports provider keys.

Env:
  BENCH_MODEL           model id (default: cline-pass/cline-pass/deepseek-v4-flash)
  OPENCODE_BIN          OpenCode binary (default: /opt/homebrew/bin/opencode or PATH)
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
MODEL=${BENCH_MODEL:-cline-pass/cline-pass/deepseek-v4-flash}

PREFERRED="/opt/homebrew/bin/opencode"
if [[ -n "${OPENCODE_BIN:-}" ]]; then
  OPENCODE="$OPENCODE_BIN"
elif [[ -x "$PREFERRED" ]]; then
  OPENCODE="$PREFERRED"
else
  OPENCODE=$(command -v opencode)
fi
if [[ ! -x "$OPENCODE" ]]; then
  echo "opencode.sh: OpenCode binary not found (expected /opt/homebrew/bin/opencode)" >&2
  exit 127
fi
if [[ ! -f "$PROMPT_FILE" ]]; then
  echo "opencode.sh: missing prompt file: $PROMPT_FILE" >&2
  exit 2
fi

PROMPT=$(cat "$PROMPT_FILE")
cd "$TASK_DIR"
cmd=("$OPENCODE" run --auto --dir "$TASK_DIR" -m "$MODEL" "$PROMPT")

echo "opencode.sh: binary=${OPENCODE} model=${MODEL} dir=${TASK_DIR}" >&2

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
