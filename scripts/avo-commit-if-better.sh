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
finite_num() {
  awk -v x="$1" 'BEGIN {
    if (x !~ /^[+-]?([0-9]+(\.[0-9]*)?|\.[0-9]+)([eE][+-]?[0-9]+)?$/) exit 1
    if (x ~ /[nN][aA][nN]/ || x ~ /[iI][nN][fF]/) exit 1
    n = x + 0
    if (n != n) exit 1
    exit 0
  }'
}
if ! finite_num "$best" || ! finite_num "$cand"; then
  echo "reject: non-numeric or non-finite score ($cand vs $best)" >&2
  exit 3
fi
awk -v b="$best" -v c="$cand" 'BEGIN { if (!(c + 0 > b + 0)) { print "reject: " c " <= " b > "/dev/stderr"; exit 3 } }'
if [[ $# -eq 0 ]]; then
  echo "accept: $cand > $best (no commit args; dry run)"
  exit 0
fi
git commit "$@"
echo "accept: committed on $branch ($cand > $best)"
# never git push
