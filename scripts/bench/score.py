#!/usr/bin/env python3
"""Isolated historical-patch scoring with pinned inputs and gold controls.

The upload boundary is `bundle`: only allowlisted prediction fields and scalar
metadata leave the machine, never raw logs, environment variables or paths.
Run `gitleaks dir` against the resulting directory before publishing it.
"""
import argparse
import hashlib
import importlib.metadata
import json
import platform
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

DATASET = "SWE-bench/SWE-bench_Verified"
REVISION = "78f471bf655a3137b2e8a75af1501690ec009ec3"
SCORER = "5.0.2"
CONTAINER_LIMITS = dict(Memory=6 * 1024**3, PidsLimit=1024, NetworkMode="none")


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write(path, value):
    Path(path).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def rows(path):
    return [json.loads(line) for line in Path(path).read_text().splitlines() if line.strip()]


def bundle(sources, out):
    """Export complete declared samples, not only cells with available patches."""
    out = Path(out)
    out.mkdir(exist_ok=False, parents=True)
    metadata, predictions, seen = [], [], set()
    for harness, source in sources:
        root = Path(source)
        ids = root.joinpath("instance_ids.txt").read_text().split()
        if len(ids) != len(set(ids)):
            raise ValueError("duplicate sample ids")
        cells = {}
        for cell in rows(root / "cells.jsonl"):
            if cell["harness"] == harness and cell["model"] == "gpt-5.6-sol":
                if cell["instance_id"] in cells:
                    raise ValueError("duplicate source cell")
                cells[cell["instance_id"]] = cell
        for iid in ids:
            if not re.fullmatch(r"[A-Za-z0-9_-]+", iid):
                raise ValueError("unsafe instance id")
            name = harness + "__gpt-5.6-sol"
            if (name, iid) in seen:
                raise ValueError("duplicate planned cell")
            seen.add((name, iid))
            cell = cells.get(iid, {})
            # Construct the patch path; never trust arbitrary paths in cells.jsonl.
            patch = root / "patches" / f"{name}__{iid}.patch"
            text = patch.read_text() if patch.exists() else ""
            original_sha256 = hashlib.sha256(text.encode()).hexdigest()
            # Never publish a bundled environment, even if a secret scanner is
            # quiet. Quarantine the whole prediction rather than changing it.
            exclusion = None
            if re.search(r'^diff --git [ab]/(?:\.venv[^/ ]*|venv|node_modules|\.ssh|\.aws|\.env)(?:/| )', text, re.M):
                exclusion = "bundled_environment_or_credentials_path"
                text = ""
            predictions.append(dict(instance_id=iid, model_name_or_path=name, model_patch=text))
            verdict = cell.get("resolved")
            metadata.append(dict(instance_id=iid, model_name_or_path=name,
                                 historical_resolved=verdict if type(verdict) is bool else None,
                                 historical_adapter_exit=cell.get("adapter_exit"),
                                 patch_available=patch.exists(),
                                 original_patch_sha256=original_sha256,
                                 upload_exclusion=exclusion,
                                 generation_provenance="unrecorded"))
    pred = out / "predictions.jsonl"
    pred.write_text("".join(json.dumps(p) + "\n" for p in predictions))
    write(out / "manifest.json", dict(schema=1, dataset=DATASET, revision=REVISION,
                                      scorer=SCORER, predictions_sha256=digest(pred), cells=metadata))
    print(f"Exported {len(predictions)} planned cells; only predictions.jsonl and manifest.json")


def load_bundle(directory):
    directory = Path(directory)
    meta = json.loads((directory / "manifest.json").read_text())
    if meta["dataset"] != DATASET or meta["revision"] != REVISION or meta["scorer"] != SCORER:
        raise ValueError("unexpected evaluation specification")
    if digest(directory / "predictions.jsonl") != meta["predictions_sha256"]:
        raise ValueError("prediction checksum mismatch")
    predictions = rows(directory / "predictions.jsonl")
    keys = [(p["model_name_or_path"], p["instance_id"]) for p in predictions]
    if len(keys) != len(set(keys)):
        raise ValueError("duplicate predictions")
    if set(keys) != {(c["model_name_or_path"], c["instance_id"]) for c in meta["cells"]}:
        raise ValueError("manifest/prediction coverage mismatch")
    return meta, predictions


def enforce_container_limits(client):
    """Apply limits at the persistent API boundary; verify before any start."""
    create = client.api.create_container
    observations = []

    def limited_create(*args, **kwargs):
        host = dict(kwargs.get("host_config") or {})
        host.update(CONTAINER_LIMITS)
        kwargs["host_config"] = host
        result = create(*args, **kwargs)
        try:
            actual = client.api.inspect_container(result["Id"])["HostConfig"]
            observed = {key: actual.get(key) for key in ("Memory", "PidsLimit", "NetworkMode")}
            if observed != {key: host[key] for key in observed}:
                raise RuntimeError("daemon did not apply evaluation container limits")
            observations.append(dict(container_id=result["Id"], host_config=observed))
        except Exception:
            client.api.remove_container(result["Id"], force=True)
            raise
        return result

    client.api.create_container = limited_create
    return observations


def run(bundle_dir, output, shard, shards):
    """One image at a time, no live model calls, no filesystem-wide pruning."""
    from datasets import load_dataset
    import docker
    from swebench.harness.run_evaluation import run_instance
    from swebench.harness.utils import make_test_spec

    if importlib.metadata.version("swebench") != SCORER:
        raise ValueError("incorrect scorer version")
    meta, predictions = load_bundle(bundle_dir)
    exclusions = {(c["model_name_or_path"], c["instance_id"]): c.get("upload_exclusion")
                  for c in meta["cells"]}
    ids = sorted({p["instance_id"] for p in predictions})[shard::shards]
    output = Path(output)
    output.mkdir(parents=True, exist_ok=False)
    dataset = {r["instance_id"]: r for r in load_dataset(DATASET, revision=REVISION, split="test")}
    client = docker.from_env()
    observations = enforce_container_limits(client)
    provenance = dict(dataset=DATASET, revision=REVISION, scorer=SCORER,
                      predictions_sha256=meta["predictions_sha256"],
                      python=platform.python_version(), machine=platform.machine(),
                      timeout_seconds=1800,
                      requested_host_config=CONTAINER_LIMITS,
                      isolation_evidence="per-cell inspected_host_configs; requests alone are not proof",
                      workflow_run_id=__import__("os").environ.get("GITHUB_RUN_ID"))
    write(output / "provenance.json", provenance)
    results = output / "cells.jsonl"
    for iid in ids:
        if shutil.disk_usage(".").free < 3 * 1024**3:
            raise RuntimeError("less than 3 GiB free; stopping")
        row = dict(dataset[iid])
        image_name = f"ghcr.io/epoch-research/swe-bench.eval.x86_64.{iid.lower()}:latest"
        gold = dict(instance_id=iid, model_name_or_path="gold", model_patch=row["patch"])
        candidates = [gold] + [p for p in predictions if p["instance_id"] == iid]
        image = None
        try:
            image = client.images.pull(image_name)
            image_digest = image.attrs["RepoDigests"][0]
            row["image"] = image_digest
            spec = make_test_spec(row)
            for prediction in candidates:
                started = time.time()
                name = prediction["model_name_or_path"]
                cell = dict(instance_id=iid, model_name_or_path=name, image_digest=image_digest,
                            resolved=None, error=None)
                if exclusions.get((name, iid)):
                    cell["error"] = exclusions[name, iid]
                elif not prediction["model_patch"]:
                    cell.update(resolved=False, error="empty_or_missing_patch")
                else:
                    try:
                        # Unique per model: upstream names containers by run+instance.
                        run_id = f"rotary-{shard}-{name}"
                        observation_start = len(observations)
                        run_instance(spec, prediction, client, run_id, timeout=1800)
                        cell["inspected_host_configs"] = observations[observation_start:]
                        if not cell["inspected_host_configs"]:
                            raise RuntimeError("no inspected evaluation container")
                        report = Path("logs/run_evaluation") / run_id / name / iid / "report.json"
                        evidence = json.loads(report.read_text())[iid]
                        if type(evidence.get("resolved")) is not bool:
                            raise ValueError("no boolean verifier verdict")
                        cell["resolved"] = evidence["resolved"]
                        # Persist the grading evidence, not the unbounded process logs.
                        evidence_path = output / f"{name}__{iid}.json"
                        write(evidence_path, evidence)
                        cell["evidence_sha256"] = digest(evidence_path)
                    except Exception as exc:
                        cell["error"] = type(exc).__name__
                cell["seconds"] = round(time.time() - started, 3)
                with results.open("a") as file:
                    file.write(json.dumps(cell) + "\n")
                print(f"{name} {iid}: {cell['resolved']} ({cell['error']})", flush=True)
        except Exception as exc:
            # A pull/setup failure remains visible in gold coverage.
            with results.open("a") as file:
                file.write(json.dumps(dict(instance_id=iid, model_name_or_path="gold", resolved=None,
                                           error=type(exc).__name__)) + "\n")
        finally:
            if image is not None:
                client.images.remove(image.id, force=True)


def summarize(bundle_dir, files, out, calibration):
    """All planned cells stay visible; gold failures are separate from model failures."""
    meta, predictions = load_bundle(bundle_dir)
    results = {}
    for path in files:
        for cell in rows(path):
            key = (cell["model_name_or_path"], cell["instance_id"])
            if key in results:
                raise ValueError(f"duplicate score {key}")
            results[key] = cell
    ids = sorted({p["instance_id"] for p in predictions})
    gold_pass = {i for i in ids if results.get(("gold", i), {}).get("resolved") is True}
    gold_fail = {i for i in ids if results.get(("gold", i), {}).get("resolved") is False}
    gold_unscored = set(ids) - gold_pass - gold_fail
    evaluated = [c for c in results.values() if c.get("evidence_sha256")]
    isolation_verified = bool(evaluated) and all(
        c.get("inspected_host_configs") and all(o.get("host_config") == CONTAINER_LIMITS
                                               for o in c["inspected_host_configs"])
        for c in evaluated)
    lines = ["# Historical patch evaluation", "",
             "**Isolation: daemon configurations verified for graded cells.**" if isolation_verified else
             "**Isolation NOT verified: these data do not establish isolated evaluation results.**", "",
             f"**Gold controls: {len(gold_pass)}/{len(ids)} passed; {len(gold_fail)} failed; "
             f"{len(gold_unscored)} missing/error.**", "",
             "Gold-failed and gold-unscored instances are excluded from eligible rates, listed below.",
             "All-planned rates retain every instance; eligible rates use only gold-passing instances.",
             "Historical generation versions, token splits and costs are unrecorded, not zero.", "",
             f"Dataset: {DATASET}@{REVISION}; swebench {SCORER}.", "",
             "| harness/model | resolved/all planned | resolved/gold-eligible | scored/eligible | gold excluded | status |",
             "|---|---|---|---|---|---|"]
    summary = {}
    mismatches = []
    old = {(c["model_name_or_path"], c["instance_id"]): c for c in meta["cells"]}
    for name in sorted({p["model_name_or_path"] for p in predictions}):
        planned = {p["instance_id"] for p in predictions if p["model_name_or_path"] == name}
        eligible = planned & gold_pass
        passed = {i for i in planned if results.get((name, i), {}).get("resolved") is True}
        scored = {i for i in eligible if type(results.get((name, i), {}).get("resolved")) is bool}
        status = "complete" if scored == eligible and not gold_unscored else "INCOMPLETE"
        s = dict(planned=len(planned), resolved=len(passed), eligible=len(eligible),
                 eligible_resolved=len(passed & eligible), scored=len(scored),
                 gold_excluded=len(planned - eligible), status=status)
        summary[name] = s
        lines.append(f"| {name} | {s['resolved']}/{s['planned']} | "
                     f"{s['eligible_resolved']}/{s['eligible']} | {s['scored']}/{s['eligible']} | "
                     f"{s['gold_excluded']} | {status} |")
        if calibration:
            for iid in planned:
                actual = results.get((name, iid), {}).get("resolved")
                expected = old[name, iid]["historical_resolved"]
                if expected is None or actual is not expected or iid not in gold_pass:
                    mismatches.append(dict(model=name, instance_id=iid, expected=expected, actual=actual,
                                           gold=results.get(("gold", iid), {}).get("resolved")))
    lines += ["", "## Gold exclusions", ""]
    for iid in sorted(set(ids) - gold_pass):
        lines.append(f"- {iid}: " + ("gold failed" if iid in gold_fail else "gold missing/error"))
    lines += ["", "## Upload exclusions (retained in planned denominators)", ""]
    if any(c.get("upload_exclusion") for c in meta["cells"]):
        lines += ["**Harness defect: workspace artifacts were swept into generated predictions.**",
                  "The historical OMP runs included entire virtual environments in their patches.",
                  "This is a patch-collection failure, not merely an upload-packaging issue: other workspace files could also be captured.",
                  "Affected predictions are quarantined whole, not rewritten; their cells remain unscored in planned denominators.", ""]
    for cell in meta["cells"]:
        if cell.get("upload_exclusion"):
            lines.append(f"- {cell['model_name_or_path']}/{cell['instance_id']}: {cell['upload_exclusion']}; original patch not uploaded or scored")
    if calibration:
        verdict = "STOP: disagrees with historical official evidence" if mismatches else (
            "PASS" if isolation_verified else "STOP: missing verified container isolation")
        lines += ["", "## Calibration", "", verdict]
        lines += [f"- {m['model']}/{m['instance_id']}: expected {m['expected']}, actual {m['actual']}, gold {m['gold']}" for m in mismatches]
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    write(out / "summary.json", dict(gold_pass=len(gold_pass), gold_fail=sorted(gold_fail),
                                      gold_unscored=sorted(gold_unscored), models=summary,
                                      isolation_verified=isolation_verified,
                                      calibration_mismatches=mismatches))
    (out / "REPORT.md").write_text("\n".join(lines) + "\n")
    return not mismatches and (not calibration or isolation_verified)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("bundle")
    p.add_argument("--source", nargs=2, action="append", required=True, metavar=("HARNESS", "RUN_DIR"))
    p.add_argument("--out", required=True)
    p = sub.add_parser("run")
    p.add_argument("--bundle", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--shard", type=int, required=True)
    p.add_argument("--shards", type=int, required=True)
    p = sub.add_parser("report")
    p.add_argument("--bundle", required=True)
    p.add_argument("--scores", nargs="+", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--calibration", action="store_true")
    args = parser.parse_args()
    if args.command == "bundle":
        bundle(args.source, args.out)
    elif args.command == "run":
        run(args.bundle, args.out, args.shard, args.shards)
    else:
        sys.exit(0 if summarize(args.bundle, args.scores, args.out, args.calibration) else 1)


if __name__ == "__main__":
    main()
