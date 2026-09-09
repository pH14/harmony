"""Counterexamples for horizon pooling and terminal cost interpretation."""
import copy
import unittest

from analyze_h02 import analyze_prefixes


def outcome(maps=(), frames=6, dead=False):
    return {"frames": frames, "dead": dead, "equipment_gained": 0, "boss_gain": False,
            "capacity_gain": False, "reached_maps": [list(point) for point in maps]}


def fixture(states):
    registration = {"trials_per_competition": 1, "competitions": [
        {"source_sample": 0, "execution": 9, "candidate_survivor_index": None,
         "discarded_role": "candidate", "survivor_roles": ["incumbent_1", "incumbent_2"]}]}
    prefixes = [{"stratum": 3, "pair": i, "trial": 0, "execution": 9, "candidate_replaces": False,
                 "discarded": copy.deepcopy(states[0]), "survivor": copy.deepcopy(states[i+1])} for i in range(2)]
    full = [{**{k: row[k] for k in ["stratum", "pair", "trial", "execution", "candidate_replaces"]},
             "discarded": copy.deepcopy(row["discarded"][-1]), "survivor": copy.deepcopy(row["survivor"][-1])}
            for row in prefixes]
    return registration, full, prefixes


class HorizonContracts(unittest.TestCase):
    def test_later_event_does_not_establish_an_immediate_loss(self):
        registration, full, prefixes = fixture([[outcome() for _ in range(6)] for _ in range(3)])
        for row in full:
            row["discarded"]["reached_maps"] = [[1, 2, 3]]
        result = analyze_prefixes(registration, full, prefixes)
        self.assertTrue(all(r["totals"]["map_events"]["unretained_only"] == 0 for r in result["horizons"]))

    def test_later_catchup_can_hide_an_early_loss(self):
        door = [(1, 2, 3)]
        result = analyze_prefixes(*fixture([
            [outcome(door) for _ in range(6)],
            [outcome()] + [outcome(door) for _ in range(5)],
            [outcome()] + [outcome(door) for _ in range(5)],
        ]))
        counts = [r["totals"]["map_events"]["unretained_only"] for r in result["horizons"]]
        self.assertEqual(counts, [1, 0, 0, 0, 0, 0])
        self.assertEqual(result["uniform_length_mean_totals"]["map_events"]["unretained_only"], 1/6)

    def test_joint_horizon_cover_can_need_more_states_than_the_final_horizon(self):
        doors = [(1, 2, i) for i in range(3)]
        states = [[outcome([door])] + [outcome(doors) for _ in range(5)] for door in doors]
        result = analyze_prefixes(*fixture(states))
        self.assertEqual(result["horizons"][-1]["minimum_cover_histogram"], {1: 1})
        self.assertEqual(result["joint_horizon_cover"]["minimum_representatives_histogram"], {3: 1})

    def test_terminal_prefix_cannot_keep_accumulating_frames(self):
        dead = outcome(frames=2, dead=True)
        registration, full, prefixes = fixture([[copy.deepcopy(dead) for _ in range(6)] for _ in range(3)])
        analyze_prefixes(registration, full, prefixes)
        prefixes[0]["discarded"][2]["frames"] = 3
        with self.assertRaises(AssertionError):
            analyze_prefixes(registration, full, prefixes)


if __name__ == "__main__":
    unittest.main()
