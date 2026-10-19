# SPDX-License-Identifier: AGPL-3.0-or-later

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("nes_scaling", Path(__file__).with_name("qualify-nes-scaling.py"))
scaling = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scaling)


class ScalingTests(unittest.TestCase):
    def write_run(self, directory, changed=False, workers=1):
        directory.mkdir()
        (directory / "stream.jsonl").write_text('complete stream\n' if not changed else 'changed stream\n')
        stream_hash = scaling.digest(directory / "stream.jsonl")
        data = {
            "result": {"status": "complete", "campaign_replay": True, "witness_replays": 2,
                       "executions": 10, "frames_emulated": 20, "search_seconds": 1,
                       "executions_per_second": 10, "stop_reason": "execution_limit", "stream_sha256": stream_hash,
                       "stream_bytes_generated": (directory / "stream.jsonl").stat().st_size},
            "campaign": {"stream_sha256": stream_hash, "executions_completed": 10},
            "telemetry": {"search_ns": 100, "workers": [{"jobs": 10, "busy_ns": 50}] + [{"jobs": 0, "busy_ns": 0}] * (workers - 1)},
            "checkpoint": {"endpoint": "fixed"}, "identity": {"source": "fixed"},
        }
        for name, value in data.items():
            (directory / f"{name}.json").write_text(json.dumps(value))

    def test_validation_rejects_corruption_time_limits_and_missing_work(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "run"
            self.write_run(directory)
            scaling.validate_result(directory)
            path = directory / "result.json"
            original = json.loads(path.read_text())
            for field, value in (("campaign_replay", False), ("stop_reason", "wall_limit"),
                                 ("witness_replays", 0), ("executions", 9), ("stream_sha256", "corrupt"),
                                 ("stream_bytes_generated", 0)):
                with self.subTest(field=field):
                    path.write_text(json.dumps(dict(original, **{field: value})))
                    with self.assertRaises(ValueError):
                        scaling.validate_result(directory)

    def test_fixed_window_and_divergent_trial_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            evaluator = root / "evaluator"
            evaluator.write_text("immutable executable")
            request = root / "request.json"
            request.write_text(json.dumps({"executions": 10}))
            observed = []

            def run(command, directory, timeout):
                configured = json.loads(Path(command[1]).read_text())
                observed.append(configured["workers"] * configured["window"])
                self.write_run(Path(command[2]), changed=configured["workers"] == 2, workers=configured["workers"])
                return 1.0

            output = root / "evidence"
            args = ["qualify", "--evaluator", str(evaluator), "--request", str(request),
                    "--out", str(output), "--workers", "1", "2", "--window", "512", "--repeats", "1",
                    "--allow-oversubscription"]
            with patch.object(sys, "argv", args), patch.object(scaling, "run_trial", side_effect=run):
                with self.assertRaisesRegex(ValueError, "logical work differs"):
                    scaling.main()
            self.assertEqual(observed, [512, 512])
            self.assertEqual(json.loads((output / "summary.json").read_text())["status"], "failed")
            self.assertTrue((output / "r1-w2/run/stream.jsonl").exists())

    def test_success_keeps_one_reference_and_alternates_trial_order(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            evaluator = root / "evaluator"
            evaluator.write_text("immutable executable")
            request = root / "request.json"
            request.write_text(json.dumps({"executions": 10}))
            observed = []

            def run(command, directory, timeout):
                configured = json.loads(Path(command[1]).read_text())
                observed.append(configured["workers"])
                self.write_run(Path(command[2]), workers=configured["workers"])
                return 1.0

            output = root / "evidence"
            args = ["qualify", "--evaluator", str(evaluator), "--request", str(request),
                    "--out", str(output), "--workers", "1", "2", "--window", "512", "--repeats", "2",
                    "--allow-oversubscription"]
            with patch.object(sys, "argv", args), patch.object(scaling, "run_trial", side_effect=run):
                scaling.main()
            self.assertEqual(observed, [1, 2, 2, 1])
            summary = json.loads((output / "summary.json").read_text())
            self.assertEqual(summary["status"], "verified")
            self.assertEqual(summary["rates"]["2"]["speedup"], 1)

    def test_failed_evaluator_retains_log(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            with self.assertRaisesRegex(ValueError, "exited 3"):
                scaling.run_trial([sys.executable, "-c", "print('failure evidence'); raise SystemExit(3)"], directory, 5)
            self.assertIn("failure evidence", (directory / "execution.log").read_text())


if __name__ == "__main__":
    unittest.main()
