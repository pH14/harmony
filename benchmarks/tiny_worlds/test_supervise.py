# SPDX-License-Identifier: AGPL-3.0-or-later
"""Small supervisor contract tests; no build or large allocation is run."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import ExitStack
from unittest.mock import patch

from benchmarks.tiny_worlds import supervise


class SupervisorTests(unittest.TestCase):
    def test_disk_usage_counts_files_once_and_ignores_symlinks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            first = root / 'first'
            second = root / 'second'
            first.mkdir()
            second.mkdir()
            payload = first / 'payload'
            payload.write_bytes(b'x' * 4096)
            os.link(payload, second / 'same-inode')
            try:
                (second / 'outside').symlink_to('/etc/hosts')
            except (OSError, NotImplementedError):
                pass
            self.assertEqual(supervise.disk_usage([first, second])['logical_bytes'], 4096)

    def test_shared_admission_reserves_distinct_slots_and_rejects_40gb(self):
        with tempfile.TemporaryDirectory() as temporary:
            state = Path(temporary) / 'state'
            roots = [Path(temporary)]
            common = {'cpu_slots': 1, 'disk_gb': 0.01, 'seconds': 2}
            first = type('Args', (), {**common, 'memory_gb': 20.0})()
            second = type('Args', (), {**common, 'memory_gb': 20.0})()
            with patch.object(supervise, 'host_memory', return_value=(64_000_000_000, 40_000_000_000)), \
                 patch.object(supervise, '_host_disk', return_value=10_000_000_000), \
                 patch.object(supervise, '_cpu_universe', return_value=[0, 1]), \
                 patch.object(supervise, 'process_group_rss', return_value=1), \
                 patch.object(supervise, '_pid_identity', return_value='same-process'), \
                 patch.object(supervise.os, 'getloadavg', return_value=(0.0, 0.0, 0.0)):
                token, cpus, _ = supervise._admit(first, state, roots, {'logical_bytes': 0})
                self.assertEqual(cpus, [0])
                with self.assertRaisesRegex(RuntimeError, 'shared task reservations'):
                    supervise._admit(second, state, roots, {'logical_bytes': 0})
                supervise._release(state, token)

    def run_supervised(self, temporary, code, *, seconds=2, memory_gb=0.256, fake_rss=1,
                       attach=None, admission=None):
        root = Path(temporary)
        artifacts = root / 'artifacts'
        artifacts.mkdir(exist_ok=True)
        out = root / 'out'
        cpu_id = supervise._cpu_universe()[0]
        default_admission = ('test-token', [cpu_id], {
            'total_bytes': 64_000_000_000, 'available_bytes': 40_000_000_000,
            'disk_available_bytes': 10_000_000_000, 'load_1m': 0.0,
            'cpu_slots_available': 1})
        with ExitStack() as stack:
            stack.enter_context(patch.object(
                supervise, '_admit', side_effect=admission) if admission else
                patch.object(supervise, '_admit', return_value=default_admission))
            stack.enter_context(patch.object(supervise, 'process_group_rss', return_value=fake_rss))
            stack.enter_context(patch.object(supervise, 'host_memory',
                                              return_value=(64_000_000_000, 40_000_000_000)))
            if attach is not None:
                stack.enter_context(patch.object(supervise, '_attach_process_group', side_effect=attach))
            rc = supervise.main(['--out', str(out), '--artifact-root', str(artifacts),
                                 '--cpu-slots', '1', '--memory-gb', str(memory_gb),
                                 '--disk-gb', '0.01', '--seconds', str(seconds), '--',
                                 sys.executable, '-c', code])
        return rc, json.loads((out / 'summary.json').read_text()), out

    def reservation_writer(self, cpu_id):
        def admit(_args, state_dir, _roots, _initial_disk):
            token = 'test-token'
            record = {'token': token, 'pid': os.getpid(),
                      'pid_identity': supervise._pid_identity(os.getpid()),
                      'cpu_slots': 1, 'cpus': [cpu_id],
                      'memory_bytes': 256_000_000, 'disk_bytes': 10_000_000}
            with supervise._ledger_lock(state_dir) as ledger_path:
                supervise._write_json(ledger_path, {'reservations': [record]})
            return token, [cpu_id], {
                'total_bytes': 64_000_000_000, 'available_bytes': 40_000_000_000,
                'disk_available_bytes': 10_000_000_000, 'load_1m': 0.0,
                'cpu_slots_available': 1}
        return admit

    def test_sigterm_during_attach_cleans_child_and_releases_reservation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            marker = root / 'child-terminated'
            pid_path = root / 'child.pid'
            ready_path = root / 'child-ready'
            child_code = (f'import signal,time,sys; marker={str(marker)!r}; '
                          'signal.signal(signal.SIGTERM, lambda *_: '
                          '(open(marker, "w").write("stopped"), sys.exit(0))); '
                          f'open({str(pid_path)!r}, "w").write(str(__import__("os").getpid())); '
                          f'open({str(ready_path)!r}, "w").write("ready"); '
                          'time.sleep(30)')
            real_attach = supervise._attach_process_group

            def attach_then_signal(state_dir, token, pgid):
                real_attach(state_dir, token, pgid)
                deadline = time.monotonic() + 2
                while not ready_path.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready_path.exists(), 'child did not finish installing its handler')
                os.kill(os.getpid(), signal.SIGTERM)

            cpu_id = supervise._cpu_universe()[0]
            rc, summary, _ = self.run_supervised(
                temporary, child_code, attach=attach_then_signal,
                admission=self.reservation_writer(cpu_id))
            self.assertEqual(rc, 2)
            self.assertEqual(summary['status'], 'incomplete')
            self.assertEqual(summary['incomplete_reason'], 'supervisor_cancelled')
            self.assertTrue(marker.exists(), 'the child should receive cleanup SIGTERM')
            self.assertIsNone(supervise._pid_identity(int(pid_path.read_text())))
            ledger = json.loads((root / '.tiny-worlds-supervisor' / 'reservations.json').read_text())
            self.assertEqual(ledger['reservations'], [])

    def test_attach_failure_cleans_started_child_and_releases_reservation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            marker = root / 'child-terminated'
            pid_path = root / 'child.pid'
            ready_path = root / 'child-ready'
            child_code = (f'import signal,time,sys; marker={str(marker)!r}; '
                          'signal.signal(signal.SIGTERM, lambda *_: '
                          '(open(marker, "w").write("stopped"), sys.exit(0))); '
                          f'open({str(pid_path)!r}, "w").write(str(__import__("os").getpid())); '
                          f'open({str(ready_path)!r}, "w").write("ready"); '
                          'time.sleep(30)')
            real_attach = supervise._attach_process_group

            def attach_then_fail(state_dir, token, pgid):
                real_attach(state_dir, token, pgid)
                deadline = time.monotonic() + 2
                while not ready_path.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready_path.exists(), 'child did not finish installing its handler')
                raise OSError('injected attach failure')

            cpu_id = supervise._cpu_universe()[0]
            rc, summary, _ = self.run_supervised(
                temporary, child_code, attach=attach_then_fail,
                admission=self.reservation_writer(cpu_id))
            self.assertEqual(rc, 1)
            self.assertEqual(summary['status'], 'infrastructure_error')
            self.assertTrue(marker.exists(), 'the child should receive cleanup SIGTERM')
            self.assertIsNone(supervise._pid_identity(int(pid_path.read_text())))
            ledger = json.loads((root / '.tiny-worlds-supervisor' / 'reservations.json').read_text())
            self.assertEqual(ledger['reservations'], [])

    def test_wall_timeout_cleans_the_entire_process_group(self):
        with tempfile.TemporaryDirectory() as temporary:
            marker = Path(temporary) / 'child-terminated'
            child_code = (f'import signal,time,sys; path={str(marker)!r}; '
                          'signal.signal(signal.SIGTERM, lambda *_: (open(path, "w").write("stopped"), sys.exit(0))); '
                          'time.sleep(30)')
            code = (f'import subprocess,sys,time; subprocess.Popen([sys.executable, "-c", {child_code!r}]); '
                    'time.sleep(30)')
            rc, summary, _ = self.run_supervised(
                temporary, code, seconds=0.3, fake_rss=1)
            self.assertEqual(rc, 2)
            self.assertEqual(summary['status'], 'incomplete')
            self.assertEqual(summary['incomplete_reason'], 'wall_timeout')
            self.assertTrue(marker.exists(), 'the child in the command process group should receive SIGTERM')

    def test_sampled_memory_stop_is_reported_as_incomplete(self):
        with tempfile.TemporaryDirectory() as temporary:
            rc, summary, _ = self.run_supervised(
                temporary, 'import time; time.sleep(5)', memory_gb=0.016,
                fake_rss=20_000_000)
            self.assertEqual(rc, 2)
            self.assertEqual(summary['status'], 'incomplete')
            self.assertEqual(summary['incomplete_reason'], 'sampled_process_tree_memory_limit')


if __name__ == '__main__':
    unittest.main()
