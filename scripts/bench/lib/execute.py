#!/usr/bin/env python3
"""Capture a benchmark invocation and final usage events, never infer missing costs.

No log or environment is copied into metadata. Prompt arguments are replaced by
their digest. Usage schemas are explicit per harness; unknown formats stay null.
Price snapshots, when supplied in models.json, must include source and as_of.
"""
import hashlib
import json
import os
import signal
import subprocess
import sys
import threading
from pathlib import Path


def usage_event(harness, event):
    """Only terminal usage events count; streaming deltas would double-count."""
    if harness == "codex" and event.get("type") == "turn.completed":
        u = event["usage"]
        cached = u.get("cached_input_tokens", 0)
        return dict(input=u["input_tokens"] - cached, output=u["output_tokens"],
                    cache_read=cached, cache_write=0, reported_usd=None)
    if harness in ("pi", "omp") and event.get("type") == "message_end":
        message = event.get("message", {})
        if message.get("role") != "assistant" or "usage" not in message:
            return None
        u = message["usage"]
        return dict(input=u["input"], output=u["output"], cache_read=u["cacheRead"],
                    cache_write=u["cacheWrite"], reported_usd=u.get("cost", {}).get("total"))
    if harness == "opencode" and event.get("type") == "step_finish":
        p = event["part"]
        u = p["tokens"]
        return dict(input=u["input"], output=u["output"], cache_read=u["cache"]["read"],
                    cache_write=u["cache"]["write"], reported_usd=p.get("cost"))
    return None


def totals(events, pricing):
    if not events:
        return dict(tokens=None, cost_usd=None, cost_source=None, usage_status="unavailable")
    tokens = {key: sum(e[key] for e in events) for key in ("input", "output", "cache_read", "cache_write")}
    if any(type(v) is not int or v < 0 for e in events for k, v in e.items() if k != "reported_usd"):
        raise ValueError("invalid token usage")
    result = dict(tokens=tokens, cost_usd=None, cost_source=None, usage_status="reported",
                  usage_events=len(events))
    # A rate snapshot is explicit user-owned evaluation configuration, not a
    # guessed tariff for an alias or subscription model.
    if pricing is not None:
        if not pricing.get("source") or not pricing.get("as_of"):
            raise ValueError("pricing needs source and as_of")
        rates = {k: pricing[k] for k in tokens}
        if any(type(v) not in (int, float) or v < 0 for v in rates.values()):
            raise ValueError("invalid USD/million rate")
        result.update(cost_usd=sum(tokens[k] * rates[k] for k in tokens) / 1_000_000,
                      cost_source="rate_snapshot", pricing=pricing)
    elif all(type(e.get("reported_usd")) in (int, float) and e["reported_usd"] >= 0 for e in events):
        result.update(cost_usd=sum(e["reported_usd"] for e in events), cost_source="harness_estimate")
    return result


def main():
    harness, prompt_file, *command = sys.argv[1:]
    if command[0] == "--":
        command = command[1:]
    prompt_bytes = Path(prompt_file).read_bytes()
    prompt = prompt_bytes.decode().rstrip("\n")
    prompt_digest = hashlib.sha256(prompt_bytes).hexdigest()
    argv = [f"<prompt:sha256:{prompt_digest}>" if arg == prompt else arg for arg in command]
    binary = Path(command[0]).resolve()
    try:
        version_run = subprocess.run([str(binary), "--version"], capture_output=True, timeout=5, text=True)
        version = version_run.stdout.strip()[:200] if version_run.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        version = None
    metadata = dict(harness=harness, argv=argv, binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                    version=version, prompt_sha256=prompt_digest, tokens=None, cost_usd=None,
                    usage_status="unavailable", timeout_seconds=int(os.getenv("BENCH_AGENT_TIMEOUT", "21600")))
    path = os.getenv("BENCH_CELL_METADATA")
    if path:
        Path(path).write_text(json.dumps(metadata) + "\n")
    events = []
    usage_invalid = False
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               start_new_session=True)
    timed_out = threading.Event()

    def terminate():
        timed_out.set()
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    timer = threading.Timer(metadata["timeout_seconds"], terminate)
    timer.start()
    try:
        for raw in process.stdout:
            sys.stdout.buffer.write(raw)
            sys.stdout.buffer.flush()
            try:
                event = json.loads(raw)
                usage = usage_event(harness, event) if isinstance(event, dict) else None
                if usage is not None:
                    events.append(usage)
            except (ValueError, KeyError, TypeError):
                # A malformed terminal event makes the subtotal incomplete.
                # Prose and streaming events are simply not usage.
                if raw.lstrip().startswith(b"{"):
                    usage_invalid = True
        code = process.wait()
    finally:
        timer.cancel()
    cfg = json.loads((Path(__file__).parents[1] / "models.json").read_text())
    model = next((m for m in cfg["models"] if m["id"] == os.getenv("BENCH_MODEL_ID")), {})
    metadata.update(totals(events, model.get("pricing")))
    metadata.update(exit_code=124 if timed_out.is_set() else code,
                    usage_complete=code == 0 and bool(events) and not timed_out.is_set() and not usage_invalid)
    if path:
        Path(path).write_text(json.dumps(metadata) + "\n")
    return metadata["exit_code"]


if __name__ == "__main__":
    sys.exit(main())
