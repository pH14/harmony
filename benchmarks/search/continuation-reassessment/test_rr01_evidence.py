import copy
import gzip
import json
from pathlib import Path
import unittest

from score_rr01 import assess, qualified_slots


class LocalLossQuery(unittest.TestCase):
    def endpoint(self, hp):
        path = Path(__file__).resolve().parents[1] / 'endpoint-encounter/ap01-output/root.json.gz'
        root = json.loads(gzip.decompress(path.read_bytes()))
        context = root['context']
        context['memory']['enemies'][0]['hit_points'] = hp
        return dict(alive=True, victory=False, context=context, state=root['endpoint'],
                    classified_slots=qualified_slots(context))

    def test_both_loss_directions_and_an_improving_replacement(self):
        low, high = self.endpoint(129), self.endpoint(140)
        for replaces, candidate, incumbent, direction in [
            (False, low, high, 'rejected_candidate'), (True, high, low, 'replaced_incumbent')]:
            result = assess(dict(replaces=replaces, candidate=candidate, incumbent=incumbent))
            self.assertTrue(result['lower_hp_lost'])
            self.assertTrue(result['equal_resources'])
            self.assertEqual(result['direction'], direction)
        result = assess(dict(replaces=True, candidate=low, incumbent=high))
        self.assertFalse(result['lower_hp_lost'])

    def test_resource_tradeoff_is_not_resource_equality(self):
        low, high = self.endpoint(129), self.endpoint(140)
        low['state']['health'] -= 1
        result = assess(dict(replaces=False, candidate=low, incumbent=high))
        self.assertTrue(result['lower_hp_lost'])
        self.assertFalse(result['equal_resources'])

    def test_missing_unknown_hp_and_multiple_slots_stay_unavailable(self):
        row = dict(replaces=False, candidate=self.endpoint(129), incumbent=None)
        self.assertIsNone(assess(row)['lower_hp_lost'])
        row['incumbent'] = self.endpoint(255)
        self.assertIsNone(assess(row)['lower_hp_lost'])
        row['incumbent'] = self.endpoint(140)
        row['incumbent']['context']['memory']['enemies'][1]['special'] = 64
        row['incumbent']['classified_slots'] = qualified_slots(row['incumbent']['context'])
        self.assertIsNone(assess(row)['lower_hp_lost'])

    def test_rust_classification_must_agree_with_independent_raw_context(self):
        row = dict(replaces=False, candidate=self.endpoint(129), incumbent=self.endpoint(140))
        bad = copy.deepcopy(row)
        bad['candidate']['classified_slots'][0]['hp'] = 128
        with self.assertRaises(AssertionError):
            assess(bad)


if __name__ == '__main__':
    unittest.main()
