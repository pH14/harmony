#!/usr/bin/env python3
"""Driver contract checks using synthetic runner outputs, with no emulator work."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

DRIVER = Path(__file__).resolve().parents[1] / "alternative-futures/mm2_chain.py"
spec = importlib.util.spec_from_file_location("mm2_chain", DRIVER)
chain = importlib.util.module_from_spec(spec)
spec.loader.exec_module(chain)


class ChainLimits(unittest.TestCase):
    def exercise(self, late=False):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "binary"
            binary.write_bytes(b"synthetic driver fixture")
            assets = root / "assets.json"
            assets.write_text(json.dumps({"core": {"path": "fixture-core"},
                                          "mm2": {"path": "fixture-rom", "sha256": "fixture-hash"}}))
            out = root / "chain"
            calls = []

            def run(command, **kwargs):
                if command[0] == "python3":
                    suite = json.loads(Path(command[3]).read_text())
                    case = suite["cases"][0]
                    index = len(calls)
                    calls.append(case["id"])
                    self.assertEqual(suite["search"]["frames"], [10, 20][index])
                    if index == 0:
                        self.assertNotIn("prefix_input", suite["search"])
                    else:
                        prefix = Path(suite["search"]["prefix_input"])
                        self.assertEqual(chain.sha(prefix), suite["search"]["prefix_sha256"])
                    cell = Path(command[command.index("--out") + 1]) / f"{case['id']}-s9-w4-m8192"
                    (cell / "campaign").mkdir(parents=True)
                    cost = suite["search"]["frames"] + int(late and index == 1)
                    result = {"solved": True, "frames_emulated": cost,
                              "frames_to_first_victory": cost, "executions": 1,
                              "witness": {"physical_suffix_frames": 0}}
                    (cell / "summary.json").write_text(json.dumps({"status": "complete", "result": result}))
                    (cell / "campaign/next-prefix.json").write_text(json.dumps({"actions": []}))
                else:
                    stage = dict(chain.STAGES)[command[-1]]
                    json.dump({"verified_replays": 2, "result": {"setup_frames_after_tape": 1,
                              "endpoint": {"stage": stage, "weapons_obtained": 65}}}, kwargs["stdout"])
                return subprocess.CompletedProcess(command, 0, stderr=b"")

            arguments = [str(DRIVER), "--root", str(root), "--out", str(out),
                         "--binary", str(binary), "--build-info", str(root / "unused-build.json"),
                         "--progress-binary", str(binary), "--assets", str(assets), "--seed", "9",
                         "--max-stages", "2", "--stage-frames", "10", "20"]
            with patch.object(sys, "argv", arguments), patch.object(chain.subprocess, "run", run), contextlib.redirect_stdout(io.StringIO()):
                chain.main()
            manifest = json.loads((out / "chain.json").read_text())
            self.assertEqual(calls, ["mm2-metal", "mm2-heat"])
            self.assertNotIn("wily4_replay", manifest)
            return manifest

    def test_stage_cap_does_not_claim_wily_or_start_a_third_stage(self):
        result = self.exercise()
        self.assertEqual(result["status"], "stage_limit")
        self.assertEqual(result["stages"][-1]["bridge_replay"]["result"]["endpoint"]["stage"], 1)

    def test_late_victory_is_recorded_without_a_bridge_or_next_stage(self):
        result = self.exercise(late=True)
        self.assertEqual(result["status"], "victory_after_frame_limit")
        self.assertTrue(result["stages"][-1]["result"]["solved"])
        self.assertNotIn("bridge_replay", result["stages"][-1])
        self.assertNotIn("next_prefix_sha256", result["stages"][-1])


if __name__ == "__main__":
    unittest.main()
