"""Planted differences ensure qualification projection keeps physical state and decisions."""
from copy import deepcopy
import unittest
from verify import OLD_DIGEST, NEW_DIGEST, POLICY, project_stream, project_checkpoint


class Projections(unittest.TestCase):
    def test_only_declared_identity_and_result_hash_differences_are_erased(self):
        core = 'a' * 64
        header = {'format':'metroid-quicknes-campaign-stream-v4',
                  'emulator_backend':'native;result_digest=' + OLD_DIGEST + ';sha256=' + core}
        events = [header, {'event':'job', 'parent_id':1, 'mutation_seed':2, 'frames':3,
                           'decisions':[{'decision':'retained', 'id':4}], 'result_sha256':'1' * 64}]
        audited = deepcopy(events)
        audited[0].update(format='metroid-quicknes-campaign-stream-endpoint-context-v5',
                          endpoint_encounter_observation=POLICY)
        audited[0]['emulator_backend'] = header['emulator_backend'].replace(OLD_DIGEST, NEW_DIGEST)
        audited[1]['result_sha256'] = '2' * 64
        expected = project_stream(events, core, False)
        self.assertEqual(expected, project_stream(audited, core, True))
        for key, value in [('parent_id',9), ('mutation_seed',7), ('frames',1), ('decisions',[])]:
            changed = deepcopy(audited)
            changed[1][key] = value
            self.assertNotEqual(expected, project_stream(changed, core, True))

    def test_physical_ram_and_observation_changes_survive_core_header_projection(self):
        core = 'a' * 64
        state = list(b'HQNESST2' + b'x' * 40 + core.encode() + b'game state')
        value = {'format':'metroid-quicknes-snapshot-checkpoint-v4', 'entries':[
            {'id':0, 'snapshot':{'failed':False, 'emulator_state':state,
                                 'observation':{'frame_count':0, 'decoded':{'health':99}}}}]}
        audit = deepcopy(value)
        audit['format'] = 'metroid-quicknes-snapshot-checkpoint-endpoint-context-v5'
        audit['entries'][0]['snapshot']['observation']['endpoint_boss_slots'] = None
        expected = project_checkpoint(value, core, False)
        self.assertEqual(expected, project_checkpoint(audit, core, True))
        changed = deepcopy(audit)
        changed['entries'][0]['snapshot']['emulator_state'][-1] ^= 1
        self.assertNotEqual(expected, project_checkpoint(changed, core, True))
        changed = deepcopy(audit)
        changed['entries'][0]['snapshot']['observation']['decoded']['health'] = 1
        self.assertNotEqual(expected, project_checkpoint(changed, core, True))
        audit['entries'][0]['snapshot']['observation']['endpoint_boss_slots'] = 1
        with self.assertRaises(AssertionError): project_checkpoint(audit, core, True)


if __name__ == '__main__':
    unittest.main()
