#!/usr/bin/env bash
# Inner-loop harness quality suite. No Docker, no model quota.
# Run after every rx4 change that touches streaming, tools, or recovery.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
export PATH="${HOME}/.cargo/bin:${HOME}/.local/bin:${PATH}"

cd "$ROOT"

echo "==> rx4 harness inner loop (providers + builtin-tools)"
cargo test --locked --lib --features providers,builtin-tools -- \
  provider:: \
  cassette:: \
  guardrails:: \
  hashline:: \
  avo:: \
  compaction::

echo "harness inner loop passed"
