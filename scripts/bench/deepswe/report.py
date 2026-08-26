#!/usr/bin/env python3
"""Write DeepSWE REPORT.md + results.json. Never invent a resolve rate."""
from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path


def rate(rows: list[dict]) -> str:
    scored = [r for r in rows if r.get("resolved") is True or r.get("resolved") is False]
    if not scored:
        return "n/a (no Pier verifier reward.json)"
    ok = sum(1 for r in scored if r["resolved"] is True)
    return f"{ok}/{len(scored)} ({ok / len(scored):.0%}) pass@1"


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--cells", required=True)
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
        "# DeepSWE accuracy",
        "",
        f"- Dataset: `{meta.get('dataset', 'datacurve/deep-swe')}`",
        "- Metric: Pier verifier `reward.json` (reward==1). Not patch-byte size.",
        f"- n={meta.get('n')}  sample-seed={meta.get('sample_seed')}",
        f"- Model: {meta.get('model')} effort={meta.get('effort')}",
        f"- Driver: {meta.get('driver', 'thin+pier-verifier')}",
        f"- Docker: {meta.get('docker', 'unknown')}",
        "",
        "## Summary",
        "",
        "| harness | model | pass@1 | instances |",
        "|---|---|---|---|",
    ]
    for (h, m) in sorted(by):
        rows = by[(h, m)]
        lines.append(f"| {h} | {m} | {rate(rows)} | {len(rows)} |")
    lines += [
        "",
        "## Cells",
        "",
        "| harness | model | instance_id | resolved | seconds |",
        "|---|---|---|---|---|",
    ]
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
    (out_dir / "results.json").write_text(json.dumps({"meta": meta, "cells": cells}, indent=2) + "\n")
    sys.stdout.write("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
