"""Public upload and reporting contracts, independent of Docker and network."""
import json
import tempfile
import unittest
from pathlib import Path

import score


class ScoringTests(unittest.TestCase):
    def test_bundle_retains_missing_cells_without_exporting_private_metadata(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            source.mkdir()
            (source / "instance_ids.txt").write_text("repo__a-1\nrepo__a-2\nrepo__a-3\n")
            (source / "cells.jsonl").write_text(json.dumps(dict(
                harness="tk", model="gpt-5.6-sol", instance_id="repo__a-1", resolved=True,
                patch="/private/credentials", raw_log="secret-log", environment={"API_KEY": "secret"}
            )) + "\n")
            (source / "patches").mkdir()
            (source / "patches/tk__gpt-5.6-sol__repo__a-1.patch").write_text("diff --git a/f b/f\n")
            (source / "patches/tk__gpt-5.6-sol__repo__a-3.patch").write_text(
                "diff --git a/.venv39/bin/activate b/.venv39/bin/activate\n+private-environment\n")
            out = root / "bundle"
            score.bundle([("tk", source)], out)
            meta, predictions = score.load_bundle(out)
            exported = "".join(f.read_text() for f in out.iterdir())
            self.assertNotIn("secret", exported)
            self.assertNotIn("/private/credentials", exported)
            self.assertNotIn("private-environment", exported)
            self.assertEqual(len(predictions), 3)
            self.assertEqual(predictions[1]["model_patch"], "")
            self.assertFalse(meta["cells"][1]["patch_available"])
            self.assertEqual(predictions[2]["model_patch"], "")
            self.assertEqual(meta["cells"][2]["upload_exclusion"], "bundled_environment_or_credentials_path")
            with (out / "predictions.jsonl").open("a") as file:
                file.write("\n")
            with self.assertRaisesRegex(ValueError, "checksum"):
                score.load_bundle(out)

    def test_report_exposes_gold_exclusions_missing_verdicts_and_calibration_disagreement(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            predictions = [dict(instance_id=i, model_name_or_path="tk", model_patch="patch")
                           for i in ["a", "b", "c", "d"]]
            (root / "predictions.jsonl").write_text("".join(json.dumps(p) + "\n" for p in predictions))
            score.write(root / "manifest.json", dict(dataset=score.DATASET, revision=score.REVISION,
                        scorer=score.SCORER, predictions_sha256=score.digest(root / "predictions.jsonl"),
                        cells=[dict(instance_id=i, model_name_or_path="tk", historical_resolved=True)
                               for i in ["a", "b", "c", "d"]]))
            results = [dict(model_name_or_path=n, instance_id=i, resolved=r) for n, i, r in [
                ("gold", "a", True), ("gold", "b", False), ("gold", "c", True),
                ("tk", "a", True), ("tk", "b", True), ("tk", "d", False)]]
            (root / "cells.jsonl").write_text("".join(json.dumps(c) + "\n" for c in results))
            self.assertFalse(score.summarize(root, [root / "cells.jsonl"], root / "report", True))
            report = json.loads((root / "report/summary.json").read_text())
            self.assertEqual(report["gold_fail"], ["b"])
            self.assertEqual(report["gold_unscored"], ["d"])
            self.assertEqual(report["models"]["tk"], dict(planned=4, resolved=2, eligible=2,
                             eligible_resolved=1, scored=1, gold_excluded=2, status="INCOMPLETE"))
            self.assertEqual(len(report["calibration_mismatches"]), 3)


if __name__ == "__main__":
    unittest.main()
