#!/usr/bin/env python3
"""Write REPORT.md + results.json and print a resolve-rate table."""
from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path


def rate(rows: list[dict]) -> str:
    scored = [r for r in rows if r.get("resolved") is True or r.get("resolved") is False]
    if not scored:
        return "n/a (no official eval)"
    ok = sum(1 for r in scored if r["resolved"] is True)
    return f"{ok}/{len(scored)} ({ok / len(scored):.0%})"


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--cells", required=True, help="JSONL of per-instance cells")
    p.add_argument("--out-dir", required=True)
    p.add_argument("--meta", default="{}")
    args = p.parse_args()
    cells = [json.loads(l) for l in Path(args.cells).read_text().splitlines() if l.strip()]
    meta = json.loads(args.meta)
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    by = defaultdict(list)
    for c in cells:
        by[(c["harness"], c["model"])].append(c)

    lines = [
        "# SWE-bench Verified accuracy",
        "",
        f"- Dataset: `{meta.get('dataset', 'princeton-nlp/SWE-bench_Verified')}`",
        f"- Metric: resolve rate (fail-to-pass tests), not wall-clock on toy tasks",
        f"- n={meta.get('n')}  sample-seed={meta.get('sample_seed')}",
        f"- Driver: {meta.get('driver', 'auto')}",
        f"- Docker: {meta.get('docker', 'unknown')}",
        "",
        "## Summary",
        "",
        "| harness | model | resolve | instances |",
        "|---|---|---|---|",
    ]
    for (h, m) in sorted(by):
        rows = by[(h, m)]
        lines.append(f"| {h} | {m} | {rate(rows)} | {len(rows)} |")
    lines += ["", "## Cells", "", "| harness | model | instance_id | resolved | seconds |", "|---|---|---|---|---|"]
    for c in cells:
        r = c.get("resolved")
        if r is True:
            rs = "true"
        elif r is False:
            rs = "false"
        else:
            rs = "null"
        lines.append(
            f"| {c['harness']} | {c['model']} | {c['instance_id']} | {rs} | {c.get('seconds', '')} |"
        )
    lines.append("")
    (out_dir / "REPORT.md").write_text("\n".join(lines))
    payload = {"meta": meta, "cells": cells}
    (out_dir / "results.json").write_text(json.dumps(payload, indent=2) + "\n")
    sys.stdout.write("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
