# SPDX-License-Identifier: AGPL-3.0-or-later
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from harmony_test import Branch, Harmony, Schedule, Site


class AuthoredTests(unittest.TestCase):
    def test_schedule_uses_recipe_node_and_hook_order(self):
        recipe = {"workload": {"package": "faults", "options": {
            "nodes": {"writer": {}, "checkpoint": {}},
            "hooks": {"writer": [], "checkpoint": []}}}}
        site = Site("checkpoint")
        actions = Schedule().park("checkpoint", site, for_="10s").hook("writer").wait("1s").encode(recipe)["actions"]
        self.assertEqual(actions[0]["operation"]["SitePark"],
                         {"node": 0, "site": site.id, "hold_us": 10000000, "ticks": 10})
        self.assertEqual(actions[1]["operation"], {"Hook": [2, 10]})
        self.assertEqual(site.id, Site("checkpoint").id)
        self.assertNotEqual(site.id, Site("writer").id)
        with self.assertRaises(ValueError):
            Schedule().encode(recipe)
        with self.assertRaises(ValueError):
            Schedule().wait("11ms")

    def test_assertions_require_execution_evidence(self):
        branch = Branch(Harmony("unused"), Path("unused"))
        with patch.object(Branch, "inspect", return_value={"result": {"replays": []}}):
            with self.assertRaises(AssertionError):
                branch.clean()
        execution = {"bug": False, "violations": [], "sometimes": [], "timeline": []}
        with patch.object(Branch, "inspect", return_value={"result": {"replays": [execution]}}):
            branch.clean()
            for assertion in (lambda: branch.observed("oracle"),
                              lambda: branch.violated("loss"),
                              lambda: branch.parked(Site("checkpoint")),
                              branch.identical):
                with self.assertRaises(AssertionError):
                    assertion()

    def test_expected_bug_exit_is_distinct_from_execution_error(self):
        harmony = Harmony("unused")
        with patch("subprocess.run", return_value=subprocess.CompletedProcess([], 1, "finding", "")):
            self.assertEqual(harmony.call("branch", outcomes=(0, 1)), "finding")
        with patch("subprocess.run", return_value=subprocess.CompletedProcess([], 2, "", "guest failed")):
            with self.assertRaisesRegex(RuntimeError, "guest failed"):
                harmony.call("branch", outcomes=(0, 1))

    def test_branch_uses_public_cli_and_accepts_a_completed_finding(self):
        with tempfile.TemporaryDirectory() as directory:
            recipe = Path(directory) / "harmony.toml"
            recipe.write_text('[workload]\npackage="faults"\n')
            harmony = Harmony(Path(directory) / "results")
            calls = []
            def call(*args, **kwargs):
                calls.append(args)
                if args[0] == "branch":
                    action_path = Path(args[args.index("--actions") + 1])
                    self.assertEqual(json.loads(action_path.read_text())["actions"][0]["operation"], {"Wait": 100})
                    return ""
                return json.dumps({"manifest": {"status": "complete"}})
            with patch.object(harmony, "call", side_effect=call):
                result = harmony.branch("case", Schedule().wait("1s"), recipe=recipe, repeat=2)
            self.assertEqual(result.path.name, "case")
            self.assertEqual(calls[0][0], "branch")
            self.assertIn("--config", calls[0])
            for name in ("../escape", "nested/name", ".", ""):
                with self.assertRaises(ValueError):
                    harmony.branch(name, recipe=recipe)


if __name__ == "__main__":
    unittest.main()
