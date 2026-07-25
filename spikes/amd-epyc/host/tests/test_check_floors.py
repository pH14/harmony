# SPDX-License-Identifier: AGPL-3.0-or-later
"""Negative controls for the AMD floor checker (schemas/check-floors.py).

A grader that has never been seen to go red is unproven (work order hm-537). Each control here
starts from a KNOWN-GOOD set of records the checker accepts, applies ONE mutation, and asserts the
gate goes red — built on the shared planted-failure harness (spikes/negcontrol.py), the same
discipline the ARM comparators use.
"""

import sys
import tempfile
import unittest
from pathlib import Path

SPIKE = Path(__file__).resolve().parents[2]
CHECK_FLOORS = SPIKE / "schemas" / "check-floors.py"
# The shared harness lives at the spikes/ root: …/spikes/amd-epyc/host/tests/ -> parents[3].
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from negcontrol import mutate, run_grader, write_records_json  # noqa: E402


def exactness_record(rep, *, count_n1=10, count_n2=110, oracle_delta=100,
                     taken_per_iter=1, n1=10, clean=True, multiplexed=False):
    """One clean, oracle-exact exactness window: count_n2 - count_n1 == oracle_delta, and a
    stable per-class offset (count_n1 - taken_per_iter*n1)."""
    return {
        "kind": "exactness", "payload": "brn", "rep": rep,
        "count_n1": count_n1, "count_n2": count_n2, "oracle_delta": oracle_delta,
        "taken_per_iter": taken_per_iter, "n1": n1, "clean": clean,
        "multiplexed": multiplexed,
    }


def overflow_summary(*, hits_1_ok=50, hits_0_lost=0, hits_gt1_dup=0, arms_total=50):
    return {
        "kind": "overflow_summary", "payload": "brn", "period": 100000,
        "hits_1_ok": hits_1_ok, "hits_0_lost": hits_0_lost, "hits_gt1_dup": hits_gt1_dup,
        "arms_total": arms_total, "skid_max": 5, "clean_skid_max": 5,
        "skid_hist": [0, 50, 0, 0, 0, 0], "clean_arms": 50,
    }


def ae3_arm(idx, *, preempt_exit=True, work_at_preempt=100000, work_landed=100000,
            landed_exact=True):
    return {
        "kind": "arm", "idx": idx, "period": 100000, "target": 100000,
        "preempt_exit": preempt_exit, "work_at_preempt": work_at_preempt,
        "work_landed": work_landed, "landed_exact": landed_exact,
    }


class CheckFloorsNegativeControls(unittest.TestCase):
    def _run(self, records, *args):
        with tempfile.TemporaryDirectory() as tmp:
            path = write_records_json(Path(tmp) / "records.json", records)
            return run_grader(CHECK_FLOORS, *args, "--records", path)

    # ---- exactness ----
    def test_exactness_good_passes(self):
        good = [exactness_record(0), exactness_record(1)]
        r = self._run(good, "exactness", "--min-reps", "2")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("FLOOR_CHECK: PASS", r.stdout)

    def test_exactness_count_mismatch_goes_red(self):
        # Planted failure: one window's measured delta disagrees with the oracle.
        good = [exactness_record(0), exactness_record(1)]
        bad = mutate(good, 0, count_n2=111)  # 111-10 = 101 != oracle_delta 100
        r = self._run(bad, "exactness", "--min-reps", "2")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("FLOOR_CHECK: FAIL", r.stdout)

    def test_exactness_missing_rep_goes_red(self):
        # A dropped sample is a failure to account, not a pass (#6): reps must be contiguous.
        good = [exactness_record(0), exactness_record(1), exactness_record(2)]
        bad = [good[0], good[2]]  # rep 1 dropped -> reps [0,2] not contiguous
        r = self._run(bad, "exactness", "--min-reps", "2")
        self.assertNotEqual(r.returncode, 0)

    # ---- overflow ----
    def test_overflow_good_passes(self):
        r = self._run([overflow_summary()], "overflow", "--min-overflows", "50")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("FLOOR_CHECK: PASS", r.stdout)

    def test_overflow_lost_pmi_goes_red(self):
        # Planted failure: a lost overflow (hits_0_lost > 0) with no corroborating anomaly record.
        bad = [overflow_summary(hits_0_lost=1, arms_total=51)]
        r = self._run(bad, "overflow", "--min-overflows", "50")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("FLOOR_CHECK: FAIL", r.stdout)

    # ---- ae3 landing ----
    def test_ae3_good_passes(self):
        good = [ae3_arm(0), {"kind": "end", "rc": 0, "arms": 1}]
        r = self._run(good, "ae3", "--min-arms", "1")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("FLOOR_CHECK: PASS", r.stdout)

    def test_ae3_inexact_landing_goes_red(self):
        # Planted failure: a landing that did not hit the target exactly.
        bad = [ae3_arm(0, work_landed=99999, landed_exact=False),
               {"kind": "end", "rc": 0, "arms": 1}]
        r = self._run(bad, "ae3", "--min-arms", "1")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("FLOOR_CHECK: FAIL", r.stdout)

    def test_ae3_stock_path_masquerade_goes_red(self):
        # The PR-98 class: an overflow-driven arm that did NOT force KVM_EXIT_PREEMPT (stock path).
        bad = [ae3_arm(0, preempt_exit=False), {"kind": "end", "rc": 0, "arms": 1}]
        r = self._run(bad, "ae3", "--min-arms", "1")
        self.assertNotEqual(r.returncode, 0)

    # ---- gate-RC propagation (#1) ----
    def test_harness_end_rc_nonzero_short_circuits_red(self):
        # A harness that ended non-zero (a mismatch was seen in-run) cannot pass here, whatever the
        # per-sample records look like.
        good = [exactness_record(0), exactness_record(1), {"kind": "end", "rc": 1}]
        r = self._run(good, "exactness", "--min-reps", "2")
        self.assertNotEqual(r.returncode, 0)


if __name__ == "__main__":
    unittest.main()
