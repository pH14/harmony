import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from harmony_scenario import Scenario


class ScenarioContract(unittest.TestCase):
    def test_replay_requires_the_park_and_the_assertion_in_every_run(self):
        site = 0x514C0001
        scenario = (
            Scenario(image=Path("sqlite.oci"))
            .park_site(0, site, hold_ms=2000, then_wait_ms=10)
            .hook(1)
            .wait(2500)
        )

        def replay(command, **_):
            report_dir = Path(command[command.index("--out") + 1])
            report_dir.mkdir()
            (report_dir / "report.json").write_text(
                json.dumps(
                    {
                        "replays": [
                            {
                                "run": number,
                                "parks": [{"site": site}],
                                "violations": ["no-lost-committed-writes"],
                                "sometimes": ["stale-backfill-advanced"],
                                "state_hash": "same",
                            }
                            for number in (1, 2)
                        ]
                    }
                )
            )
            return type("Completed", (), {"returncode": 0})()

        with tempfile.TemporaryDirectory() as directory, patch(
            "harmony_scenario.subprocess.run", side_effect=replay
        ):
            result = scenario.run(Path(directory) / "case")
            recorded = json.loads((Path(directory) / "case" / "scenario.json").read_text())
            self.assertEqual(
                recorded["actions"][0],
                {
                    "SitePark": {
                        "node": 0,
                        "site": site,
                        "hold_us": 2_000_000,
                        "ticks": 1,
                    }
                },
            )
            (
                result.reached_site(site)
                .observed("stale-backfill-advanced")
                .not_observed("fixed-only")
                .violated("no-lost-committed-writes")
                .identical_replays()
            )
            with self.assertRaises(AssertionError):
                result.reached_site(site + 1)
            with self.assertRaises(AssertionError):
                result.not_observed("stale-backfill-advanced")


if __name__ == "__main__":
    unittest.main()
