"""Census inference must fail closed outside its recorded source preconditions."""
import copy
import gzip
import json
from pathlib import Path
import unittest

import score_checkpoint_inspection as scorer

ROOT = Path(__file__).parent


class CheckpointInspection(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.rows = json.loads(gzip.decompress((ROOT/'ci01-output/ci01-inventory.json.gz').read_bytes()))['entries']
        cls.stream = [json.loads(x) for x in scorer.raw(ROOT/'ap01-output','stream.jsonl').splitlines()]
        cls.sidecar = json.loads(scorer.raw(ROOT/'ap01-output','progress.jsonl').splitlines()[-1])

    def test_source_conditions_resolve_cached_history_separately(self):
        ids, bound = scorer.active_ids(self.rows, self.stream, self.sidecar)
        self.assertEqual((len(ids), bound, len(self.rows)-len(ids)), (557,156,31))

    def test_missing_snapshots_drops_or_action_limit_refuse_mapping(self):
        for kind in ('missing','drop','cap'):
            side, stream = copy.deepcopy(self.sidecar), copy.deepcopy(self.stream)
            if kind == 'missing': side['retained_diagnostics']['missing_snapshots'] = 1
            elif kind == 'drop': side['entry_drops'] = 1
            else: stream[0]['action_limit'] = 156
            with self.assertRaises(AssertionError): scorer.active_ids(self.rows, stream, side)

    def test_hp255_and_unclassified_states_are_not_damage(self):
        q = json.loads((ROOT/'ci01-request.json').read_text())
        context = copy.deepcopy(q['expected_root_context'])
        row = copy.deepcopy(self.rows[0]);row['context']=context
        row['endpoint_boss_slots']=1
        context['memory']['enemies'][0]['hit_points']=255
        result=scorer.describe([row])
        self.assertEqual(result['unavailable_hp_slots'],1)
        self.assertEqual(result['snapshot_local_hp_counts'],{})
        self.assertEqual(result['entries_with_classified_hp_below_root_140'],[])
        context['memory']['enemies'][0]['status']=0
        row['endpoint_boss_slots']=0
        self.assertEqual(scorer.describe([row])['classified_entries'],0)
