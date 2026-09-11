# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import copy
import unittest

try:
    from . import guest_evidence
except ImportError:  # unittest discovery can load this directory as top-level.
    import guest_evidence  # type: ignore[no-redef]


HASH = "ab" * 32
KERNEL = "11" * 32
AGENT = "22" * 32
IMAGE = "33" * 32
ENDPOINT = 2_000_000_000
REACHABLE_EVENT = (1 << 24) | 7
VIOLATION_EVENT = (1 << 24) | 8


def _event(position: int, event: int, payload: str, virtual_time: int) -> dict:
    return {
        "position": position,
        "virtual_time": virtual_time,
        "event": event,
        "payload": payload,
    }


def fixture(*, violation: bool) -> tuple[dict, dict]:
    expected = 1 if violation else 2
    summary = {
        "run": 1,
        "bug": violation,
        "stop": {"Assertion": {"point": 8}} if violation else "Deadline",
        "state_hash": HASH,
        "state_hash_encoding": "engine_digest",
        "violations": [8] if violation else [],
        "sometimes": [7],
        "actions_applied": expected,
        "guest_horizons": expected,
    }
    report = {
        "package": "faults",
        "mode": "replay",
        "image_sha256": IMAGE,
        "kernel_sha256": KERNEL,
        "fault_agent_sha256": AGENT,
        "identity": "hook1-controlled-fixture",
        "seed": 1,
        "workers": 1,
        "horizon_ms": 1000,
        "ram_mib": 512,
        "executions": 1,
        "bug_found": violation,
        "first_bug_execution": 1 if violation else None,
        "bugs": [],
        "replays": [summary],
        "horizons_clocked": expected,
        "wall_seconds": 0,
    }
    if violation:
        report["bugs"] = [
            {
                "execution": 1,
                "actions": [{"Hook": 1}, "Wait"],
                "stop": {"Assertion": {"point": 8}},
                "violations": [8],
                "sometimes": [7],
                "state_hash": HASH,
                "state_hash_encoding": "engine_digest",
                "confirmed": True,
                "replay": copy.deepcopy(summary),
            }
        ]
    events = [_event(0, REACHABLE_EVENT, "000000", ENDPOINT // 2)]
    if violation:
        events.append(_event(1, VIOLATION_EVENT, "010000", ENDPOINT))
    sidecar = {
        "format": "harmony-replay-events-v1",
        "run": 1,
        "virtual_time": ENDPOINT,
        "state_hash": HASH,
        "state_hash_encoding": "engine_digest",
        "events": events,
    }
    return report, sidecar


class GuestEvidenceTests(unittest.TestCase):
    def validate(self, report: dict, sidecar: dict, *, violation: bool) -> dict:
        return guest_evidence.validate_fixture(report, sidecar, KERNEL, AGENT, violation=violation)

    def assertRejected(self, report: dict, sidecar: dict, *, violation: bool) -> guest_evidence.EvidenceError:
        with self.assertRaises(guest_evidence.EvidenceError) as context:
            self.validate(report, sidecar, violation=violation)
        return context.exception

    def test_controlled_deadline_and_violation_endpoints(self) -> None:
        for violation in (False, True):
            with self.subTest(violation=violation):
                report, sidecar = fixture(violation=violation)
                self.assertEqual(
                    self.validate(report, sidecar, violation=violation),
                    {"state_hash": HASH, "virtual_time": ENDPOINT, "event_count": 2 if violation else 1},
                )

    def test_missing_reachable_event_has_specific_code_after_shape_checks(self) -> None:
        report, sidecar = fixture(violation=False)
        sidecar["events"] = []
        report["replays"][0]["sometimes"] = []
        error = self.assertRejected(report, sidecar, violation=False)
        self.assertEqual(error.code, "missing_hit")

        malformed = copy.deepcopy(sidecar)
        malformed.pop("state_hash")
        error = self.assertRejected(report, malformed, violation=False)
        self.assertNotEqual(error.code, "missing_hit")

    def test_full_event_array_and_event_shape_are_checked(self) -> None:
        report, sidecar = fixture(violation=False)
        sidecar["events"].append(_event(1, REACHABLE_EVENT, "000000", ENDPOINT))
        sidecar["events"][1]["position"] = 99
        error = self.assertRejected(report, sidecar, violation=False)
        self.assertNotEqual(error.code, "missing_hit")

        report, sidecar = fixture(violation=False)
        del sidecar["events"][0]["payload"]
        self.assertNotEqual(self.assertRejected(report, sidecar, violation=False).code, "missing_hit")

    def test_report_pins_and_replay_shape_are_strict(self) -> None:
        cases = []
        report, sidecar = fixture(violation=False)
        changed = copy.deepcopy(report)
        changed["package"] = "other"
        cases.append((changed, sidecar))
        report, sidecar = fixture(violation=False)
        changed = copy.deepcopy(report)
        changed["kernel_sha256"] = "00" * 32
        cases.append((changed, sidecar))
        report, sidecar = fixture(violation=False)
        changed = copy.deepcopy(report)
        changed["workers"] = True
        cases.append((changed, sidecar))
        report, sidecar = fixture(violation=False)
        changed = copy.deepcopy(report)
        changed["replays"][0].pop("state_hash_encoding")
        cases.append((changed, sidecar))
        report, sidecar = fixture(violation=False)
        changed = copy.deepcopy(sidecar)
        changed["state_hash_encoding"] = "legacy_sha256_of_digest"
        cases.append((report, changed))
        for changed_report, changed_sidecar in cases:
            with self.subTest(report=changed_report, sidecar=changed_sidecar):
                self.assertNotEqual(
                    self.assertRejected(changed_report, changed_sidecar, violation=False).code,
                    "missing_hit",
                )

    def test_event_ranges_hex_and_time_order_are_strict(self) -> None:
        mutations = []
        report, sidecar = fixture(violation=False)
        changed = copy.deepcopy(sidecar)
        changed["events"][0]["virtual_time"] = ENDPOINT + 1
        mutations.append(changed)
        report, sidecar = fixture(violation=False)
        changed = copy.deepcopy(sidecar)
        changed["events"][0]["payload"] = "00000"
        mutations.append(changed)
        report, sidecar = fixture(violation=False)
        changed = copy.deepcopy(sidecar)
        changed["events"][0]["payload"] = "00000G"
        mutations.append(changed)
        report, sidecar = fixture(violation=False)
        changed = copy.deepcopy(sidecar)
        changed["events"][0]["event"] = True
        mutations.append(changed)
        report, sidecar = fixture(violation=False)
        changed = copy.deepcopy(sidecar)
        changed["events"][0]["payload"] = "00" * (guest_evidence.MAX_PAYLOAD_BYTES + 1)
        mutations.append(changed)
        for changed_sidecar in mutations:
            with self.subTest(sidecar=changed_sidecar):
                self.assertNotEqual(
                    self.assertRejected(report, changed_sidecar, violation=False).code,
                    "missing_hit",
                )

        report, sidecar = fixture(violation=True)
        changed = copy.deepcopy(sidecar)
        changed["events"][1]["virtual_time"] = ENDPOINT // 4
        self.assertNotEqual(self.assertRejected(report, changed, violation=True).code, "missing_hit")

        report, sidecar = fixture(violation=False)
        changed = copy.deepcopy(sidecar)
        changed["virtual_time"] = 0
        self.assertNotEqual(self.assertRejected(report, changed, violation=False).code, "missing_hit")

    def test_violation_requires_exact_event_and_summary(self) -> None:
        report, sidecar = fixture(violation=True)
        sidecar["events"][1]["payload"] = "000000"
        self.assertNotEqual(self.assertRejected(report, sidecar, violation=True).code, "missing_hit")

        report, sidecar = fixture(violation=True)
        report["replays"][0]["violations"] = []
        self.assertNotEqual(self.assertRejected(report, sidecar, violation=True).code, "missing_hit")

        report, sidecar = fixture(violation=False)
        sidecar["events"].append(_event(1, VIOLATION_EVENT, "010000", ENDPOINT))
        self.assertNotEqual(self.assertRejected(report, sidecar, violation=False).code, "missing_hit")

        report, sidecar = fixture(violation=False)
        sidecar["events"] = []
        report["replays"][0]["stop"] = {"Assertion": {"point": 8}}
        self.assertNotEqual(self.assertRejected(report, sidecar, violation=False).code, "missing_hit")

        report, sidecar = fixture(violation=False)
        sidecar["events"] = []
        report["replays"][0]["sometimes"] = [7]
        self.assertNotEqual(self.assertRejected(report, sidecar, violation=False).code, "missing_hit")

        report, sidecar = fixture(violation=False)
        sidecar["events"] = []
        report["replays"][0]["sometimes"] = [9]
        self.assertNotEqual(self.assertRejected(report, sidecar, violation=False).code, "missing_hit")

    def test_violation_binds_one_confirmed_bug_to_the_actual_replay(self) -> None:
        mutations = []
        report, sidecar = fixture(violation=True)
        changed = copy.deepcopy(report)
        changed["bugs"] = []
        mutations.append((changed, sidecar))

        report, sidecar = fixture(violation=True)
        changed = copy.deepcopy(report)
        changed["bugs"].append(copy.deepcopy(changed["bugs"][0]))
        mutations.append((changed, sidecar))

        report, sidecar = fixture(violation=True)
        changed = copy.deepcopy(report)
        changed["bugs"][0]["confirmed"] = False
        mutations.append((changed, sidecar))

        report, sidecar = fixture(violation=True)
        changed = copy.deepcopy(report)
        changed["bugs"][0]["actions"] = []
        mutations.append((changed, sidecar))

        report, sidecar = fixture(violation=True)
        changed = copy.deepcopy(report)
        changed["bugs"][0]["stop"] = "Deadline"
        mutations.append((changed, sidecar))

        report, sidecar = fixture(violation=True)
        changed = copy.deepcopy(report)
        changed["bugs"][0]["replay"] = None
        mutations.append((changed, sidecar))

        for changed_report, changed_sidecar in mutations:
            with self.subTest(report=changed_report):
                self.assertNotEqual(
                    self.assertRejected(changed_report, changed_sidecar, violation=True).code,
                    "missing_hit",
                )

        nested_fields = ("stop", "violations", "sometimes", "state_hash", "state_hash_encoding")
        for field in nested_fields:
            report, sidecar = fixture(violation=True)
            report["bugs"][0]["replay"][field] = copy.deepcopy(report["replays"][0][field])
            if field == "stop":
                report["bugs"][0]["replay"][field] = "Deadline"
            elif field == "violations":
                report["bugs"][0]["replay"][field] = []
            elif field == "sometimes":
                report["bugs"][0]["replay"][field] = []
            elif field == "state_hash":
                report["bugs"][0]["replay"][field] = "cd" * 32
            else:
                report["bugs"][0]["replay"][field] = "legacy_sha256_of_digest"
            self.assertNotEqual(self.assertRejected(report, sidecar, violation=True).code, "missing_hit")

        report, sidecar = fixture(violation=True)
        sidecar["events"] = []
        report["bugs"][0]["confirmed"] = False
        self.assertNotEqual(self.assertRejected(report, sidecar, violation=True).code, "missing_hit")

    def test_event_count_and_payload_bounds_are_enforced_without_truncation(self) -> None:
        report, sidecar = fixture(violation=False)
        sidecar["events"] = [
            _event(index, REACHABLE_EVENT, "000000", ENDPOINT // 2)
            for index in range(guest_evidence.MAX_EVENTS + 1)
        ]
        error = self.assertRejected(report, sidecar, violation=False)
        self.assertEqual(error.code, "event_limit")


if __name__ == "__main__":
    unittest.main()
