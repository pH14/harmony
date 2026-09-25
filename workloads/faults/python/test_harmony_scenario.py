import unittest
from pathlib import Path

from harmony_scenario import Result, Scenario, Site


class ScenarioContract(unittest.TestCase):
    def test_site_park_records_a_precise_action(self):
        site = Site("sqlite.wal.before_checkpoint")
        self.assertEqual(site.id, 1036543216)
        scenario = Scenario(image=Path("service.oci")).park_site(
            node=0, site=site, hold_ms=2000, then_wait_ms=10
        )
        self.assertEqual(
            scenario.actions,
            [{"SitePark": {"node": 0, "site": site.id, "hold_us": 2_000_000, "ticks": 1}}],
        )

    def test_result_checks_every_replay(self):
        result = Result(
            {
                "replays": [
                    {
                        "run": run,
                        "parks": [{"site": 7}],
                        "violations": ["lost-write"],
                        "sometimes": ["final-read"],
                        "state_hash": "same",
                    }
                    for run in (1, 2)
                ]
            },
            Path("unused"),
        )
        (
            result.reached_site(7)
            .observed("final-read")
            .not_observed("stale-backfill")
            .violated("lost-write")
            .identical_replays()
        )
        with self.assertRaises(AssertionError):
            result.reached_site(8)
        with self.assertRaises(AssertionError):
            result.not_observed("final-read")


if __name__ == "__main__":
    unittest.main()
