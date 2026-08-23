#!/usr/bin/env bash
# Drive OMP against a checked-out SWE-bench task directory.
# Usage: omp.sh TASK_DIR [PROMPT_FILE]
# Env: BENCH_MODEL, BENCH_OMP_PROVIDER, BENCH_EFFORT, OMP_BIN
set -euo pipefail

usage() {
  cat <<'H'
omp.sh — SWE-bench Verified adapter for OMP

Usage:
  adapters/omp.sh TASK_DIR [PROMPT_FILE]

Reads the issue prompt from PROMPT_FILE or $TASK_DIR/.bench_prompt.md.
Writes a git patch or leaves the repo fixed. stdin is /dev/null.

Non-interactive: omp -p --print --cwd TASK --no-session --model openai-codex/<id>
Thinking/effort: --thinking (off, minimal, low, medium, high, xhigh, max, auto).

Env:
  BENCH_MODEL           model id (default: gpt-5.6-sol). Bare ids are
                        prefixed with BENCH_OMP_PROVIDER so fuzzy match
                        does not land on cursor.
  BENCH_OMP_PROVIDER    OMP provider prefix (default: openai-codex)
  BENCH_EFFORT          thinking level (default: low). Passed as --thinking.
  OMP_BIN               OMP binary (default: /opt/homebrew/bin/omp or PATH)
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
PROVIDER=${BENCH_OMP_PROVIDER:-openai-codex}
# Prefer provider/model so "gpt-5.6-sol" does not fuzzy-match cursor first.
if [[ "$MODEL" == */* ]]; then
  MODEL_SPEC="$MODEL"
else
  MODEL_SPEC="${PROVIDER}/${MODEL}"
fi

if [[ -n "${OMP_BIN:-}" ]]; then
  OMP="$OMP_BIN"
elif [[ -x /opt/homebrew/bin/omp ]]; then
  OMP=/opt/homebrew/bin/omp
else
  OMP=$(command -v omp)
fi
if [[ ! -x "$OMP" ]]; then
  echo "omp.sh: OMP binary not found" >&2
  exit 127
fi
if [[ ! -f "$PROMPT_FILE" ]]; then
  echo "omp.sh: missing prompt file: $PROMPT_FILE" >&2
  exit 2
fi

PROMPT=$(cat "$PROMPT_FILE")
help_text=$("$OMP" --help 2>&1 || true)

cd "$TASK_DIR"
# -p is --print; keep both as documented for non-interactive runs.
cmd=("$OMP" -p --print --cwd "$TASK_DIR" --no-session --model "$MODEL_SPEC")
if grep -q -- '--thinking' <<<"$help_text"; then
  cmd+=(--thinking "$EFFORT")
fi
if grep -q -- '--auto-approve' <<<"$help_text"; then
  cmd+=(--auto-approve)
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
