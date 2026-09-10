"""The resource oracle changes only named RAM bytes and carries its operation."""
import copy
import gzip
import json
from pathlib import Path
import unittest

from resource_program import expected_intervention, ram_payloads, verify_program

ROOT = Path(__file__).parent


class ResourceProgram(unittest.TestCase):
    def before(self):
        value = json.loads(gzip.decompress((ROOT/'aq01-output/one-slot/root-snapshot.json.gz').read_bytes()))
        value['observation'].pop('endpoint_boss_slots')  # S01 native build has no optional features.
        return value

    def test_exact_resource_patch_and_noop(self):
        before = self.before()
        self.assertEqual(expected_intervention(before, 79, 0), before)
        after = expected_intervention(before, 1999, 20)
        wram, sram = ram_payloads(bytes(before['emulator_state']))
        self.assertEqual([i for i, (a,b) in enumerate(zip(before['emulator_state'], after['emulator_state'])) if a != b],
                         [wram+0x106, wram+0x107, sram+0x879])
        corrupt = copy.deepcopy(after)
        corrupt['emulator_state'][sram+0x87c] ^= 2  # A fabricated boss defeat cannot hide among resource writes.
        self.assertNotEqual(corrupt, expected_intervention(before, 1999, 20))
        for health, missiles in [(0,0), (2000,0), (1999,21)]:
            with self.assertRaises(AssertionError):
                expected_intervention(before, health, missiles)

    def test_witness_cannot_move_or_hide_its_operation(self):
        operation = {'prefix_actions': 2, 'resources': {'health': 1999, 'missiles': 20},
                     'before_snapshot_sha256': 'a'*64, 'after_snapshot_sha256': 'b'*64}
        prefix = [1,2]
        self.assertEqual(verify_program(operation, operation, [1,2,3], prefix), [3])
        for changed in [{**operation,'prefix_actions':1}, {**operation,'after_snapshot_sha256':'c'*64},
                        {**operation,'resources':{'health':1999,'missiles':21}}]:
            with self.assertRaises(AssertionError):
                verify_program(changed, operation, [1,2,3], prefix)
        with self.assertRaises(AssertionError):
            verify_program(operation, operation, [1,9,3], prefix)
