"""Reference-trace and planted-boundary checks; no emulator or search runs."""
from copy import deepcopy
import csv
import gzip
from pathlib import Path
import unittest

from boss_interval import BossIntervalObserver, classify

ROOT = Path(__file__).resolve().parent


def losses(report):
    return [x['hp_loss'] for x in report['intervals'] if x['kind'] == 'hp_drop']


class BossIntervalTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with gzip.open(ROOT / 'f03-boss-area-frames.csv.gz', 'rt') as stream:
            cls.rows = [{k: int(v) for k, v in row.items()} for row in csv.DictReader(stream)]
        cls.by_frame = {row['frame']: row for row in cls.rows}
        cls.before = cls.by_frame[76001]
        cls.hit = cls.by_frame[76002]

    def test_complete_reference_keeps_all_98_decreases_and_no_other_changes(self):
        observer = BossIntervalObserver()
        events, reset_kinds = [], []
        for row in self.rows:
            report = observer.observe(0, row['frame'], row)
            for event in report['intervals']:
                if event['kind'] == 'hp_drop':
                    events.append((row['frame'], event['area'], event['hp_loss']))
                elif event['hp_loss'] is None:
                    reset_kinds.append(event['kind'])
        self.assertEqual(len(events), 98)
        self.assertEqual(sum(delta for _, area, delta in events if area == 20), 140)
        self.assertEqual(sum(delta for _, area, delta in events if area == 18), 96)
        self.assertEqual(reset_kinds.count('baseline'), 2)
        self.assertEqual(reset_kinds.count('left_or_unclassified'), 2)
        self.assertEqual(len(reset_kinds), 4)

    def test_fresh_baseline_at_every_observed_cut_preserves_future_intervals(self):
        continuous = BossIntervalObserver()
        previous = self.rows[0]
        continuous.observe(0, previous['frame'], previous)
        count = total = 0
        for current in self.rows[1:]:
            expected = continuous.observe(0, current['frame'], current)
            restored = BossIntervalObserver()
            restored.observe(0, previous['frame'], previous)
            actual = restored.observe(0, current['frame'], current)
            self.assertEqual(actual, expected)
            count += len(losses(actual))
            total += sum(losses(actual))
            previous = current
        self.assertEqual((count, total), (98, 236))

    def test_real_hit_state_is_identifiable_without_loader_or_current_tag(self):
        self.assertEqual(self.hit['loader'], 0)
        self.assertEqual(self.hit['slot0_special'], 3)
        self.assertEqual(self.hit['slot0_saved_status'], 65)
        state = classify(self.hit, 0)
        self.assertEqual((state.area, state.hp, state.status), (20, 136, 6))

    def test_epoch_or_explicit_reset_removes_a_fabricated_cross_restore_drop(self):
        # Stitch nonadjacent real snapshots into adjacent caller clock ticks.
        later = self.by_frame[76024]
        unaware = BossIntervalObserver()
        unaware.observe(0, 100, self.before)
        self.assertEqual(losses(unaware.observe(0, 101, later)), [8])
        for explicit in (False, True):
            observer = BossIntervalObserver()
            observer.observe(0, 100, self.before)
            if explicit:
                observer.reset()
            report = observer.observe(0 if explicit else 1, 101, later)
            self.assertEqual(losses(report), [])
            self.assertEqual(report['intervals'][0]['kind'], 'baseline')

    def test_gaps_and_repeated_frames_do_not_create_changes(self):
        for frame in (100, 102):
            observer = BossIntervalObserver()
            observer.observe(0, 100, self.before)
            self.assertEqual(losses(observer.observe(0, frame, self.hit)), [])

    def test_inactive_slot_and_changed_identity_break_comparison(self):
        observer = BossIntervalObserver()
        observer.observe(0, 100, self.before)
        inactive = deepcopy(self.before)
        inactive['slot0_status'] = 0
        observer.observe(0, 101, inactive)
        self.assertEqual(losses(observer.observe(0, 102, self.hit)), [])
        for key, value in (('area', 18), ('slot0_type', 8), ('slot0_saved_status', 193)):
            observer = BossIntervalObserver()
            observer.observe(0, 100, self.before)
            changed = deepcopy(self.hit)
            changed[key] = value
            report = observer.observe(0, 101, changed)
            self.assertEqual(losses(report), [])
            self.assertEqual(report['intervals'][0]['kind'], 'identity_changed')

    def test_saved_boss_bits_are_ignored_in_normal_and_inactive_states(self):
        stale = deepcopy(self.hit)
        stale['slot0_saved_status'] = 66
        stale['slot0_special'] = 0
        for status in (0, 1, 2, 4, 5):
            stale['slot0_status'] = status
            self.assertIsNone(classify(stale, 0))
        stale['slot0_status'] = 6
        self.assertIsNotNone(classify(stale, 0))
        stale['slot0_saved_status'] = 67  # Unsupported saved prior status.
        self.assertIsNone(classify(stale, 0))

    def test_hp_increase_sentinel_and_unexplained_drop_are_unavailable(self):
        for hp, status, special, expected in ((150, 6, 3, 'hp_increase'),
                                              (255, 6, 3, 'hp_unavailable'),
                                              (136, 2, 64, 'unexplained_drop')):
            observer = BossIntervalObserver()
            observer.observe(0, 100, self.before)
            changed = deepcopy(self.hit)
            changed.update(slot0_hp=hp, slot0_status=status, slot0_special=special)
            report = observer.observe(0, 101, changed)
            self.assertEqual(losses(report), [])
            self.assertEqual(report['intervals'][0]['kind'], expected)

    def test_missing_saved_byte_and_invalid_clocks_are_rejected(self):
        broken = deepcopy(self.hit)
        del broken['slot0_saved_status']
        with self.assertRaises(ValueError):
            classify(broken, 0)
        for epoch, frame in ((-1, 1), (0, -1), (False, 1)):
            with self.assertRaises(ValueError):
                BossIntervalObserver().observe(epoch, frame, self.hit)


if __name__ == '__main__':
    unittest.main()
