#!/usr/bin/env python3
"""Write the per-instance agent prompt into a task checkout."""
from __future__ import annotations

import argparse
import json
from pathlib import Path


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--sample", required=True)
    p.add_argument("--instance-id", required=True)
    p.add_argument("--out", required=True)
    args = p.parse_args()
    rec = None
    for line in Path(args.sample).read_text().splitlines():
        r = json.loads(line)
        if r["instance_id"] == args.instance_id:
            rec = r
            break
    if rec is None:
        raise SystemExit(f"unknown instance: {args.instance_id}")
    body = rec["problem_statement"]
    text = (
        f"You are solving SWE-bench Verified instance {args.instance_id}.\n\n"
        "The repository is already checked out at the issue base commit.\n"
        "Implement a correct fix for the GitHub issue below.\n"
        "Do not expand scope. Leave the repo fixed (dirty tree or committed patch is fine).\n"
        "Do not rewrite git history. Do not push.\n"
        "Do not ask clarifying questions; pick the conservative correct fix and implement it.\n\n"
        f"ISSUE:\n{body}\n"
    )
    Path(args.out).write_text(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
