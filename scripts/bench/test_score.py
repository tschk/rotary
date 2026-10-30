"""Public upload and reporting contracts, independent of Docker and network."""
import json
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch
from pathlib import Path

import score


class ScoringTests(unittest.TestCase):
    def test_limits_reach_fresh_collections_and_fail_closed_on_bad_inspection(self):
        class API:
            ignore_limits = False
            removed = False

            def create_container(self, **kwargs):
                self.config = {} if self.ignore_limits else kwargs["host_config"]
                return {"Id": "fixture"}

            def inspect_container(self, _):
                return {"HostConfig": self.config}

            def remove_container(self, _, force):
                self.removed = force

        class Client:
            api = API()

            @property
            def containers(self):
                return SimpleNamespace(create=self.api.create_container)

        client = Client()
        observed = score.enforce_container_limits(client)
        self.assertIsNot(client.containers, client.containers)
        client.containers.create(image="fixture", host_config={"CapAdd": ["SYS_ADMIN"]})
        self.assertEqual(client.api.config, dict(Memory=6442450944, PidsLimit=1024,
                                                NetworkMode="none", CapAdd=["SYS_ADMIN"]))
        self.assertEqual(observed[0]["host_config"], dict(Memory=6442450944, PidsLimit=1024, NetworkMode="none"))
        client.api.ignore_limits = True
        with self.assertRaisesRegex(RuntimeError, "daemon did not apply"):
            client.containers.create(image="fixture")
        self.assertTrue(client.api.removed)
        self.assertEqual(len(observed), 1)

    def test_real_docker_sdk_serializes_limits_and_inspects_returned_configuration(self):
        try:
            import docker
        except ImportError:
            self.skipTest("Docker SDK unavailable; run again in the scorer environment")
        client = docker.DockerClient(version="1.45")
        requests = []

        def post(_, **kwargs):
            requests.append(kwargs["data"])
            return {"Id": "fixture"}

        def inspect(_):
            return {"Id": "fixture", "HostConfig": requests[-1]["HostConfig"]}

        with patch.object(client.api, "_post_json", side_effect=post), \
             patch.object(client.api, "_result", side_effect=lambda response, *args: response), \
             patch.object(client.api, "inspect_container", side_effect=inspect):
            observed = score.enforce_container_limits(client)
            for _ in range(2):
                client.containers.create("fixture", command="true", cap_add=["SYS_ADMIN"])
            self.assertEqual(len(observed), 2)
            for request in requests:
                self.assertEqual(request["HostConfig"]["Memory"], 6442450944)
                self.assertEqual(request["HostConfig"]["PidsLimit"], 1024)
                self.assertEqual(request["HostConfig"]["NetworkMode"], "none")
                self.assertEqual(request["HostConfig"]["CapAdd"], ["SYS_ADMIN"])
        client.close()

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
            self.assertFalse(report["isolation_verified"])
            for cell in results:
                cell["evidence_sha256"] = "fixture"
                cell["inspected_host_configs"] = [dict(host_config=dict(
                    Memory=6442450944, PidsLimit=1024, NetworkMode="none"))]
            (root / "cells.jsonl").write_text("".join(json.dumps(c) + "\n" for c in results))
            score.summarize(root, [root / "cells.jsonl"], root / "report", False)
            verified = json.loads((root / "report/summary.json").read_text())
            self.assertTrue(verified["isolation_verified"])
            results[0]["inspected_host_configs"][0]["host_config"]["NetworkMode"] = "bridge"
            (root / "cells.jsonl").write_text("".join(json.dumps(c) + "\n" for c in results))
            score.summarize(root, [root / "cells.jsonl"], root / "report", False)
            unverified = json.loads((root / "report/summary.json").read_text())
            self.assertFalse(unverified["isolation_verified"])


if __name__ == "__main__":
    unittest.main()
