#!/usr/bin/env python3
import argparse, json
from pathlib import Path

def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--cells", required=True)
    p.add_argument("--harness", required=True)
    p.add_argument("--model", required=True)
    p.add_argument("--instance", required=True)
    p.add_argument("--seconds", type=int, required=True)
    p.add_argument("--exit", dest="adapter_exit", type=int, default=0)
    p.add_argument("--error", default="")
    p.add_argument("--patch", default="")
    p.add_argument("--driver", default="thin")
    args = p.parse_args()
    cell = {
        "harness": args.harness,
        "model": args.model,
        "instance_id": args.instance,
        "resolved": None,
        "seconds": args.seconds,
        "adapter_exit": args.adapter_exit,
        "driver": args.driver,
    }
    if args.patch:
        cell["patch"] = args.patch
    if args.error:
        cell["error"] = args.error
    Path(args.cells).parent.mkdir(parents=True, exist_ok=True)
    with Path(args.cells).open("a") as fh:
        fh.write(json.dumps(cell) + "\n")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
