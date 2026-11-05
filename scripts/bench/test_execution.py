"""Verify adapter flags, token accounting and metadata without making model calls."""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).parent


class ExecutionTests(unittest.TestCase):
    def test_adapter_records_exact_flags_and_terminal_usage_not_stream_deltas(self):
        cases = {
            "codex": ({"type": "turn.completed", "usage": {
                "input_tokens": 137, "cached_input_tokens": 31, "output_tokens": 19}},
                {"input": 106, "output": 19, "cache_read": 31, "cache_write": 0}, None, "--json"),
            "pi": ({"type": "message_end", "message": {"role": "assistant", "usage": {
                "input": 101, "output": 23, "cacheRead": 29, "cacheWrite": 7, "cost": {"total": 0.12}}}},
                {"input": 101, "output": 23, "cache_read": 29, "cache_write": 7}, 0.12, "--mode"),
            "omp": ({"type": "message_end", "message": {"role": "assistant", "usage": {
                "input": 13, "output": 17, "cacheRead": 11, "cacheWrite": 3, "cost": {"total": 0.04}}}},
                {"input": 13, "output": 17, "cache_read": 11, "cache_write": 3}, 0.04, "--mode"),
            "opencode": ({"type": "step_finish", "part": {"tokens": {
                "input": 37, "output": 41, "cache": {"read": 5, "write": 2}}, "cost": 0.06}},
                {"input": 37, "output": 41, "cache_read": 5, "cache_write": 2}, 0.06, "--format"),
            "tk": ({"ok": True, "text": "done"}, None, None, "--json"),
            "fx": ({"text": "done"}, None, None, "--yolo"),
        }
        for harness, (event, tokens, cost, flag) in cases.items():
            with self.subTest(harness=harness), tempfile.TemporaryDirectory() as tmp:
                task = Path(tmp)
                prompt = task / "prompt"
                prompt.write_text("private prompt content\n")
                binary = task / "fake"
                binary.write_text(f"#!{sys.executable}\n" + "\n".join([
                    "import json,sys",
                    "if '--version' in sys.argv: print('fixture 1.2.3'); sys.exit(0)",
                    "if '--help' in sys.argv: print('ask --json --mode --format --provider --effort --thinking --yolo'); sys.exit(0)",
                    "if 'status' in sys.argv: print(json.dumps({'auth':'configured'})); sys.exit(0)",
                    # A streaming delta with large counts must not be charged too.
                    "print(json.dumps({'type':'message_update','usage':{'input_tokens':999999}}))",
                    f"print({json.dumps(json.dumps(event))})",
                ]) + "\n")
                binary.chmod(0o755)
                metadata = task / "metadata.json"
                result = subprocess.run([str(ROOT / f"adapters/{harness}.sh"), str(task), str(prompt)],
                                        env={**os.environ, f"{harness.upper()}_BIN": str(binary),
                                             "BENCH_CELL_METADATA": str(metadata), "BENCH_AGENT_TIMEOUT": "10"},
                                        capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                actual = json.loads(metadata.read_text())
                self.assertEqual(actual["version"], "fixture 1.2.3")
                self.assertIn(flag, actual["argv"])
                self.assertNotIn("private prompt content", metadata.read_text())
                self.assertEqual(actual["tokens"], tokens)
                self.assertEqual(actual["cost_usd"], cost)
                self.assertEqual(actual["usage_complete"], tokens is not None)

    def test_pricing_snapshot_counts_cached_input_only_once(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "lib").mkdir()
            (root / "lib/execute.py").write_bytes((ROOT / "lib/execute.py").read_bytes())
            pricing = dict(input=2, output=8, cache_read=0.5, cache_write=3,
                           source="fixture tariff", as_of="2026-10-29")
            (root / "models.json").write_text(json.dumps({"models": [{"id": "m", "pricing": pricing}]}))
            prompt = root / "prompt"
            prompt.write_text("test")
            binary = root / "fake"
            binary.write_text(f"#!{sys.executable}\nimport sys,json\n"
                              "print('fixture' if '--version' in sys.argv else json.dumps({'type':'turn.completed',"
                              "'usage':{'input_tokens':1000000,'cached_input_tokens':400000,'output_tokens':200000}}))\n")
            binary.chmod(0o755)
            metadata = root / "meta.json"
            subprocess.run([sys.executable, str(root / "lib/execute.py"), "codex", str(prompt), "--", str(binary)],
                           env={**os.environ, "BENCH_MODEL_ID": "m", "BENCH_CELL_METADATA": str(metadata)},
                           check=True, capture_output=True)
            actual = json.loads(metadata.read_text())
            # 600k × $2 + 400k × $0.5 + 200k × $8 per million = $3.
            self.assertEqual(actual["cost_usd"], 3)
            self.assertEqual(actual["cost_source"], "rate_snapshot")
            self.assertEqual(actual["pricing"], pricing)


if __name__ == "__main__":
    unittest.main()
