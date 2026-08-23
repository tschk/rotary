#!/usr/bin/env python3
"""Sample DeepSWE tasks the same way Pier does (--n-tasks + --sample-seed).

Pier lists valid local tasks via Path.iterdir() (filesystem order, not sorted),
then random.Random(seed).shuffle, then [:n].
"""
from __future__ import annotations

import argparse
import json
import random
import sys
from pathlib import Path


def _valid_task_dirs(tasks_root: Path) -> list[Path]:
    try:
        from pier.models.task.paths import TaskPaths  # type: ignore

        return [
            path
            for path in tasks_root.iterdir()
            if TaskPaths(path).is_valid(disable_verification=False)
        ]
    except Exception:
        out = []
        for path in tasks_root.iterdir():
            if (path / "task.toml").is_file() and (path / "instruction.md").is_file():
                out.append(path)
        return out


def sample(tasks_root: Path, n: int, seed: int) -> list[Path]:
    ids = _valid_task_dirs(tasks_root)
    filtered = list(ids)
    random.Random(seed).shuffle(filtered)
    if n < len(filtered):
        filtered = filtered[:n]
    return filtered


def main() -> int:
    p = argparse.ArgumentParser(description="DeepSWE Pier-compatible sample")
    p.add_argument("--tasks", required=True)
    p.add_argument("--n", type=int, default=20)
    p.add_argument("--sample-seed", type=int, default=0)
    p.add_argument("--out", required=True, help="Write task ids, one per line")
    args = p.parse_args()
    root = Path(args.tasks)
    chosen = sample(root, args.n, args.sample_seed)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("".join(path.name + "\n" for path in chosen))
    meta = {
        "n": len(chosen),
        "sample_seed": args.sample_seed,
        "tasks_root": str(root),
        "task_ids": [path.name for path in chosen],
    }
    (out.parent / "sample.json").write_text(json.dumps(meta, indent=2) + "\n")
    print(f"sampled {len(chosen)} tasks seed={args.sample_seed}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
