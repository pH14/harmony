import copy
import unittest
from score_pc01 import new_progress_advantage, surviving_defeat


class Counterexamples(unittest.TestCase):
    def point(self, hp=137, loss=0):
        return dict(complete=True, dead=False, defeat=False, frames=10, action=1,
                    root_interval_valid=True, new_hp_loss=loss, hp=hp,
                    state=dict(health=79, missiles=0, missile_capacity=20,
                               energy_tanks=1, equipment=17, bosses=0))

    def test_inherited_hp_advantage_is_not_new_progress(self):
        self.assertFalse(new_progress_advantage(self.point(), self.point(hp=140)))
        self.assertFalse(new_progress_advantage(self.point(136,1), self.point(139,1)))

    def test_more_new_damage_requires_lower_remaining_hp_and_resources(self):
        a, b = self.point(132,5), self.point(137,3)
        self.assertTrue(new_progress_advantage(a,b))
        a['state']['health'] = 78
        self.assertFalse(new_progress_advantage(a,b))
        a['state']['health'] = 79
        a['hp'] = 138
        self.assertFalse(new_progress_advantage(a,b))

    def test_missing_dead_incomplete_and_unmatched_evidence_cannot_win(self):
        a, b = self.point(132,5), self.point(137,3)
        for field, value in [('complete',False),('dead',True),('root_interval_valid',False),
                             ('hp',None),('new_hp_loss',None),('frames',11),('action',2)]:
            changed = copy.deepcopy(a)
            changed[field] = value
            self.assertFalse(new_progress_advantage(changed,b),field)

    def test_death_and_partial_command_do_not_become_surviving_defeat(self):
        p = self.point()
        p['defeat'] = True
        self.assertTrue(surviving_defeat(dict(points=[p])))
        p['dead'] = True
        self.assertFalse(surviving_defeat(dict(points=[p])))
        p['dead'],p['complete'] = False,False
        self.assertFalse(surviving_defeat(dict(points=[p])))
