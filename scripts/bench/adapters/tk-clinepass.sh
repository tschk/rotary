#!/usr/bin/env bash
# Thin wrapper around tk.sh that selects Cline-pass.
# Usage: adapters/tk-clinepass.sh TASK_DIR [PROMPT_FILE]
# Env: BENCH_MODEL (default cline-pass/deepseek-v4-flash), TK_BIN, BENCH_AGENT_TIMEOUT
set -euo pipefail
export TK_PROVIDER="${TK_PROVIDER:-clinepass}"
export BENCH_MODEL="${BENCH_MODEL:-cline-pass/deepseek-v4-flash}"
DIR=$(cd "$(dirname "$0")" && pwd)
exec "$DIR/tk.sh" "$@"
