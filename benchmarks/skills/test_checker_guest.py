# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from . import build, checker_guest, guest_evidence
from .test_guest_evidence import IMAGE, fixture


class CheckerGuestTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.paths = {}
        self.pins = {}
        for name in ('harmony', 'kernel', 'base', 'agent', 'preparer'):
            path = self.root / name
            path.write_bytes(name.encode())
            self.paths[name] = path
            self.pins[name] = hashlib.sha256(path.read_bytes()).hexdigest()
        data = b'opaque compiled bytes'
        self.artifact = build.Artifact('app', data, hashlib.sha256(data).hexdigest(), True)
        self.calls = []

    def invoke(self, name, expected, *, bug=False, silent=False, malformed=False, mutate=False):
        case = self.root / name
        case.mkdir()

        def run(argv, **kwargs):
            self.calls.append(argv)
            report, events = fixture(violation=bug)
            report['kernel_sha256'] = self.pins['kernel']
            report['fault_agent_sha256'] = self.pins['agent']
            if silent:
                report['replays'][0]['sometimes'] = []
                events['events'] = []
            if malformed:
                events['virtual_time'] += 1
            output = Path(argv[argv.index('--out') + 1])
            output.mkdir()
            (output / 'report.json').write_text(json.dumps(report))
            (output / 'replay-1-events.json').write_text(json.dumps(events))
            if mutate:
                path = case / 'actions.json'
                path.chmod(0o644)
                path.write_text('[]')
            return SimpleNamespace(exit_code=0, timed_out=False, output_overflow=False,
                                   stdout=b'', stderr=b'')

        with patch.object(checker_guest.qualify_guest, '_prepared_digest', return_value=IMAGE), \
             patch.object(checker_guest.qualify_guest.sandbox, '_run_bounded', side_effect=run):
            return checker_guest.check_artifact(self.artifact, ('check', '2', '-1', '1'),
                                               expected, self.paths, self.pins, case)

    def test_observed_decisions_are_validated_before_comparing_private_expectation(self):
        for bug in (False, True):
            for expected in (False, True):
                result = self.invoke('case-%d-%d' % (bug, expected), expected, bug=bug)
                self.assertEqual(result['status'], 'passed' if expected == (not bug) else 'mismatch')
                self.assertIs(result['observed_holds'], not bug)
                self.assertIsNotNone(result['evidence'])

    def test_expected_verdict_and_host_labels_do_not_change_guest_bytes(self):
        self.invoke('expected-positive', True)
        self.invoke('expected-negative', False)
        first = (self.root / 'expected-positive/fixture.tar').read_bytes()
        second = (self.root / 'expected-negative/fixture.tar').read_bytes()
        self.assertEqual(first, second)

    def test_silence_is_unevaluated_and_malformed_silence_is_not_credit(self):
        result = self.invoke('silent', True, silent=True)
        self.assertEqual(result['status'], 'no_telemetry')
        self.assertIsNone(result['observed_holds'])
        self.assertIsNone(result['evidence'])
        with self.assertRaises(guest_evidence.EvidenceError):
            self.invoke('malformed', True, silent=True, malformed=True)

    def test_input_mutation_cannot_be_a_checker_mismatch(self):
        with self.assertRaisesRegex(ValueError, 'input actions changed'):
            self.invoke('mutated', False, mutate=True)

    def test_existing_case_is_rejected_before_overwriting_or_preparing(self):
        case = self.root / 'existing'
        case.mkdir()
        sentinel = case / 'app.bin'
        sentinel.write_bytes(b'keep existing evidence')
        with patch.object(checker_guest.qualify_guest, '_prepared_digest') as prepare:
            with self.assertRaises(ValueError):
                checker_guest.check_artifact(self.artifact, ('check', '0'), True,
                                             self.paths, self.pins, case)
            prepare.assert_not_called()
        self.assertEqual(sentinel.read_bytes(), b'keep existing evidence')


if __name__ == '__main__':
    unittest.main()
