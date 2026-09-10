"""Planted evidence failures for the census's negative and positive decision boundary."""
from copy import deepcopy
import hashlib
import json
import unittest
from analyze_census import analyze


def fixture(positive=False):
    # Synthetic small cell, never performance evidence.
    request={'verification':'witness','actions':64,'seed':5,'workers':4,'memory_mib':8192,
             'game':'metroid','rom_sha256':'rom'}
    reg={'cells':[{'id':'cell','expected_identity':{'policies':{'observer':'v1'}},
                   'manifest':{'search':{'verification':'witness','actions':64},'seeds':[5],
                               'workers':[4],'memory_mib':[8192],
                               'cases':[{'game':'metroid','rom_sha256':'rom'}]}}],
         'binary_sha256':'binary','nominal_admitted_search_limit':100,'known_auxiliary_limit':1000}
    first={'execution':2,'route_action_end_frame':30,'area':20,'boss_slots':1} if positive else None
    counts={'actions_observed':10,'live_endpoints':8,'classified_endpoints':1 if positive else 0,'first':first}
    diagnostics={'endpoint_encounters':{'format':'metroid-live-endpoint-encounters-v1','counts':counts},
                 'named_progress':{'format':'metroid-named-progress-v2',
                                   'first_seen':{'kraid_area':{'execution':1},'ridley_area':None}}}
    result={'status':'complete','verification':'witness','stop_reason':'frame_limit',
            'frames_emulated':100,'executions':4,'witness':{'physical_suffix_frames':5},'milestone_witnesses':{}}
    artifact=None
    if positive:
        artifact=json.dumps({'format':'metroid-endpoint-encounter-input-v1','first':first,
                             'input':{'actions':[{'buttons':1,'hold_frames':30}]}}).encode()
        result['endpoint_encounter_witness']={'first':first,'verified_replays':2,'known_replay_frames':60,
            'artifact_sha256':hashlib.sha256(artifact).hexdigest(),
            'replay':{'dead':False,'physical_suffix_frames':30,
                      'diagnostics':{'endpoint_encounters':{'counts':{'first':first}}}}}
    summary={'identity':{'policies':{'observer':'v1'}},'build':{'binary_sha256':'binary'},
             'search_request':request,'status':'complete','result':result,
             'last_progress':{'frames_emulated':100,'executions':4,'workload_diagnostics':diagnostics}}
    panel={'registration_sha256':'registered','execution_complete':True,'allocation_stop':None,
           'records':[{'id':'cell','summary':summary,'checks_passed':True,'exit_code':0}]}
    return reg,panel,artifact


class CensusEvidence(unittest.TestCase):
    def test_negative_requires_complete_horizon_and_eligible_observations(self):
        reg,panel,_=fixture()
        self.assertTrue(analyze(reg,panel,'registered')['complete'])
        failed=deepcopy(panel)
        failed['records'][0]['summary']['result']['frames_emulated']=99
        self.assertFalse(analyze(reg,failed,'registered')['complete'])
        unavailable=deepcopy(panel)
        del unavailable['records'][0]['summary']['last_progress']['workload_diagnostics']['endpoint_encounters']
        with self.assertRaises(KeyError): analyze(reg,unavailable,'registered')
        empty=deepcopy(panel)
        empty['records'][0]['summary']['last_progress']['workload_diagnostics']['endpoint_encounters']['counts']['live_endpoints']=0
        with self.assertRaises(AssertionError): analyze(reg,empty,'registered')

    def test_new_positive_replays_are_charged_and_do_not_qualify_combat(self):
        reg,panel,artifact=fixture(True)
        out=analyze(reg,panel,'registered',artifact)
        self.assertEqual(out['known_auxiliary_frames'],70)
        self.assertIn('standalone observer qualification required',out['decision'])
        reg['known_auxiliary_limit']=69
        self.assertIn('ceiling exceeded',analyze(reg,panel,'registered',artifact)['decision'])

    def test_positive_requires_exact_artifact_and_endpoint_replay(self):
        reg,panel,artifact=fixture(True)
        with self.assertRaises(AssertionError): analyze(reg,panel,'registered',artifact+b' ')
        with self.assertRaises(AssertionError): analyze(reg,panel,'registered')
        early=deepcopy(panel)
        early['records'][0]['summary']['result']['endpoint_encounter_witness']['replay']['physical_suffix_frames']=31
        with self.assertRaises(AssertionError): analyze(reg,early,'registered',artifact)
        missing=deepcopy(panel)
        del missing['records'][0]['summary']['result']['endpoint_encounter_witness']
        with self.assertRaises(AssertionError): analyze(reg,missing,'registered',artifact)

    def test_failed_cells_keep_known_work_without_claiming_a_negative(self):
        reg,panel,_=fixture()
        panel['records'][0]['summary']['status']='timeout'
        panel['records'][0]['exit_code']=1
        panel['allocation_stop']='cell failure'
        out=analyze(reg,panel,'registered')
        self.assertFalse(out['complete'])
        self.assertIsNone(out['endpoint_counts'])
        self.assertEqual(out['admitted_frames_known'],100)
        self.assertEqual(out['known_auxiliary_frames'],10)


if __name__ == '__main__':
    unittest.main()
