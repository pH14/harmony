# SPDX-License-Identifier: AGPL-3.0-or-later
"""Small supervisor contract tests; no build or large allocation is run."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
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

    def run_supervised(self, temporary, code, *, seconds=2, memory_gb=0.256, fake_rss=1):
        root = Path(temporary)
        artifacts = root / 'artifacts'
        artifacts.mkdir(exist_ok=True)
        out = root / 'out'
        with patch.object(supervise, '_admit', return_value=('test-token', [supervise._cpu_universe()[0]], {
                'total_bytes': 64_000_000_000, 'available_bytes': 40_000_000_000,
                'disk_available_bytes': 10_000_000_000, 'load_1m': 0.0,
                'cpu_slots_available': 1})), \
             patch.object(supervise, 'process_group_rss', return_value=fake_rss), \
             patch.object(supervise, 'host_memory', return_value=(64_000_000_000, 40_000_000_000)):
            rc = supervise.main(['--out', str(out), '--artifact-root', str(artifacts),
                                 '--cpu-slots', '1', '--memory-gb', str(memory_gb),
                                 '--disk-gb', '0.01', '--seconds', str(seconds), '--',
                                 sys.executable, '-c', code])
        return rc, json.loads((out / 'summary.json').read_text()), out

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
