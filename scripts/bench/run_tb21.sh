#!/usr/bin/env bash
# Terminal-Bench 2.1 accuracy runner (committed version of the bench
# launcher so full-set runs are reproducible from the repo).
#
# Staggered k=2, n=3 with tk 429 backoff + agent-level retries; resumes
# the same output dir, so re-running continues where the last run died.
#
# Each task env gets a hard memory cap (harbor --override-memory-mb): a
# runaway task process (a 5.5GB pystan fit was observed) must degrade
# only its own trial, not every concurrent trial on the host.
#
# Usage: scripts/bench/run_tb21.sh [out-dir-suffix] [extra harbor args...]
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
REPO=${TELEKINESIS_ROOT:-/home/user/workspace/repo}
TK_BIN_DEFAULT="$REPO/ui/tui/target/x86_64-unknown-linux-musl/release/tk"
TK_BIN_DEFAULT_FALLBACK="$REPO/ui/tui/target/release/tk"

SUFFIX=${1:-full-k1}
shift || true
OUT=${BENCH_OUT_DIR:-$ROOT/scripts/bench/out/glm53flash-tb21-$SUFFIX}
LOG=${BENCH_LOG:-/tmp/bench-logs/tb21-$SUFFIX.log}

# Hard cap per task env, in MB. Raise for tasks that legitimately need
# more, never remove: an uncapped env can pin the whole host.
export BENCH_ENV_MEM_MB=${BENCH_ENV_MEM_MB:-3072}

export PATH="${HOME}/.local/bin:${HOME}/.cargo/bin:${PATH}"
export PYTHONPATH="$ROOT/scripts/bench${PYTHONPATH:+:$PYTHONPATH}"
export TK_BIN=${TK_BIN:-$([ -x "$TK_BIN_DEFAULT" ] && echo "$TK_BIN_DEFAULT" || echo "$TK_BIN_DEFAULT_FALLBACK")}
export TK_MAX_TURNS=${TK_MAX_TURNS:-400}
export TK_AGENT_RETRIES=${TK_AGENT_RETRIES:-2}
export BENCH_AGENT_TIMEOUT=${BENCH_AGENT_TIMEOUT:-21600}

mkdir -p "$(dirname "$LOG")"
exec harbor run \
  -d terminal-bench/terminal-bench-2-1 \
  -a harbor_tk_agent:TkHarborAgent \
  -k "${TK_BENCH_K:-2}" -n "${TK_BENCH_CONCURRENCY:-3}" \
  --override-memory-mb "$BENCH_ENV_MEM_MB" \
  --timeout-multiplier 24 \
  -o "$OUT" \
  "$@" >> "$LOG" 2>&1
