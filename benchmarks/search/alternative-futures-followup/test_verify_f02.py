#!/usr/bin/env python3
"""Planted failures in witness-to-producer and suffix-evidence binding."""
import copy
import unittest
from verify_f02 import validate_replays

class BindingTests(unittest.TestCase):
    def setUp(self):
        self.files=['w.json']
        self.expected={'w.json':{'terminal_policy':'v3','producer_snapshot_sha256':'abc','endpoint':{'health':0},'prefix_actions':4,'suffix_living_maps':[[1,2,3]],'input_sha256':'input'}}
        side={'snapshot_sha256':'abc','endpoint':{'health':0},'map_from_action':4,'living_maps':[[1,2,3]],'dead':True,'physical_frames':100}
        self.result={'terminal_policy':'v3','pairs':[{'pair':0,'stratum':0,'sides':[side,copy.deepcopy(side)]}]}
    def test_valid_dead_endpoint_can_have_prior_living_suffix_evidence(self):
        self.assertEqual(validate_replays(self.files,self.result,self.expected,'v3')[0]['physical_frames'],200)
    def test_truncated_result_is_not_silently_zipped(self):
        self.result['pairs']=[]
        with self.assertRaisesRegex(ValueError,'count'):validate_replays(self.files,self.result,self.expected,'v3')
    def test_two_identical_replays_must_match_producer(self):
        for side in self.result['pairs'][0]['sides']:side['snapshot_sha256']='wrong'
        with self.assertRaisesRegex(ValueError,'producer snapshot'):validate_replays(self.files,self.result,self.expected,'v3')
    def test_prefix_only_map_cannot_satisfy_suffix_requirement(self):
        for side in self.result['pairs'][0]['sides']:side['living_maps']=[]
        with self.assertRaisesRegex(ValueError,'suffix map'):validate_replays(self.files,self.result,self.expected,'v3')
    def test_wrong_window_or_policy_is_rejected(self):
        for side in self.result['pairs'][0]['sides']:side['map_from_action']=0
        with self.assertRaisesRegex(ValueError,'suffix window'):validate_replays(self.files,self.result,self.expected,'v3')
        self.result['terminal_policy']='v2'
        with self.assertRaisesRegex(ValueError,'terminal policy'):validate_replays(self.files,self.result,self.expected,'v3')

if __name__=='__main__':unittest.main()
