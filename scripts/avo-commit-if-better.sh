#!/usr/bin/env bash
# AVO commit-if-better helper. Never commits on main/master. Never pushes.
set -euo pipefail
if [[ $# -lt 2 ]]; then
  echo "usage: $0 <best_f> <candidate_f> [git commit args...]" >&2
  exit 1
fi
best="$1"
cand="$2"
shift 2
branch="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$branch" == "main" || "$branch" == "master" ]]; then
  echo "refuse: commit-if-better will not commit on $branch" >&2
  exit 2
fi
awk -v b="$best" -v c="$cand" 'BEGIN { if (!(c > b)) { print "reject: " c " <= " b > "/dev/stderr"; exit 3 } }'
if [[ $# -eq 0 ]]; then
  echo "accept: $cand > $best (no commit args; dry run)"
  exit 0
fi
git commit "$@"
echo "accept: committed on $branch ($cand > $best)"
# never git push
