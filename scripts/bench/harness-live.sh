#!/usr/bin/env bash
# Optional live smoke against Z.ai Coding Plan. Model is glm-5.3-flash only.
# Does not use OpenCode Go. Requires ZAI_API_KEY and a tk binary.
set -euo pipefail

if [[ -z "${ZAI_API_KEY:-}" ]]; then
  echo "harness-live: ZAI_API_KEY is not set" >&2
  exit 2
fi

if [[ -n "${TK_BIN:-}" && -x "${TK_BIN}" ]]; then
  TK="${TK_BIN}"
elif command -v tk >/dev/null 2>&1; then
  TK=$(command -v tk)
else
  echo "harness-live: tk binary not found (set TK_BIN)" >&2
  exit 127
fi

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT
printf 'hello\n' >"$workdir/note.txt"

echo "==> glm-5.3-flash pong"
out=$("$TK" exec --provider zai --model glm-5.3-flash --cwd "$workdir" \
  "reply with the single word pong")
printf '%s\n' "$out"
printf '%s\n' "$out" | grep -qi '^pong$' || {
  echo "harness-live: expected pong" >&2
  exit 1
}

echo "==> glm-5.3-flash tool loop"
out=$("$TK" exec --provider zai --model glm-5.3-flash --cwd "$workdir" \
  "Use a file-listing or bash tool to confirm note.txt exists. Then reply with the single word done. Do not write files.")
printf '%s\n' "$out"
printf '%s\n' "$out" | grep -qi '^done$' || {
  echo "harness-live: expected done after tools" >&2
  exit 1
}

echo "harness-live passed (glm-5.3-flash)"
