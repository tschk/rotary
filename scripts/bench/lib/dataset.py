#!/usr/bin/env python3
"""Download and sample SWE-bench Verified (princeton-nlp/SWE-bench_Verified)."""
from __future__ import annotations

import argparse
import json
import random
import sys
from pathlib import Path

HF_ID = "princeton-nlp/SWE-bench_Verified"
KEEP = (
    "instance_id",
    "repo",
    "base_commit",
    "problem_statement",
    "hints_text",
    "FAIL_TO_PASS",
    "PASS_TO_PASS",
    "version",
    "environment_setup_commit",
)


def _row(ex: dict) -> dict:
    out = {}
    for k in KEEP:
        if k in ex:
            v = ex[k]
            if hasattr(v, "tolist"):
                v = v.tolist()
            out[k] = v
    return out


def load_or_download(cache_jsonl: Path) -> list[dict]:
    if cache_jsonl.exists():
        rows = [json.loads(line) for line in cache_jsonl.read_text().splitlines() if line.strip()]
        if rows:
            return rows
    cache_jsonl.parent.mkdir(parents=True, exist_ok=True)
    try:
        from datasets import load_dataset  # type: ignore
    except ImportError:
        print(
            "dataset.py: install HuggingFace datasets (e.g. uv run --with datasets) to download",
            file=sys.stderr,
        )
        raise
    ds = load_dataset(HF_ID, split="test")
    rows = [_row(dict(ex)) for ex in ds]
    with cache_jsonl.open("w") as fh:
        for r in rows:
            fh.write(json.dumps(r, ensure_ascii=False) + "\n")
    return rows


def sample(rows: list[dict], n: int, seed: int) -> list[dict]:
    ordered = sorted(rows, key=lambda r: r["instance_id"])
    if n >= len(ordered):
        return ordered
    rng = random.Random(seed)
    idx = list(range(len(ordered)))
    rng.shuffle(idx)
    pick = sorted(idx[:n])
    return [ordered[i] for i in pick]


def main() -> int:
    p = argparse.ArgumentParser(description="SWE-bench Verified sample helper")
    p.add_argument("--cache", required=True, help="JSONL cache path")
    p.add_argument("--n", type=int, default=10)
    p.add_argument("--sample-seed", type=int, default=0)
    p.add_argument("--out", required=True, help="Write sampled JSONL here")
    args = p.parse_args()
    rows = load_or_download(Path(args.cache))
    chosen = sample(rows, args.n, args.sample_seed)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as fh:
        for r in chosen:
            fh.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"sampled {len(chosen)} / {len(rows)} seed={args.sample_seed}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
