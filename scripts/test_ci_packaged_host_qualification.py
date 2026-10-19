# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("packaged_qualification", Path(__file__).with_name("qualify-packaged-host.py"))
P = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(P)
EXEC = 'execve("/proc/self/exe", ["/proc/self/exe", "session-worker"], 0x123 /* 2 vars */) = 0\n'
KVM = ('open("/dev/kvm", O_RDWR|O_CLOEXEC) = 3\n'
       'ioctl(3, KVM_CREATE_VM, 0) = 4\n'
       'ioctl(4, KVM_CREATE_VCPU, 0) = 5\n'
       'ioctl(5, KVM_RUN, 0) = -1 EINTR (Interrupted system call)\n'
       'ioctl(5, KVM_RUN, 0) = 0\n')
EXIT = '+++ exited with 0 +++\n'


class PackagedHostTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def trace(self, text, pid=123):
        path = self.root / f"search.{pid}"
        path.write_text(text)
        return path

    def test_actual_worker_exec_kvm_fd_chain_and_normal_exit(self):
        path = self.trace(EXEC + KVM + EXIT)
        result = P.trace_workers(self.root)
        self.assertEqual(result, [{"pid": 123, "kvm_runs": 1, "trace_sha256": P.digest(path)}])
        self.trace(EXEC.replace('execve("/proc/self/exe",', 'execveat(7, "",').replace('2 vars */)', '2 vars */, AT_EMPTY_PATH)') + KVM + EXIT)
        self.assertEqual(P.trace_workers(self.root)[0]["kvm_runs"], 1)

    def test_parent_or_other_pid_cannot_supply_worker_execution(self):
        self.trace(KVM + EXIT, 1)
        with self.assertRaisesRegex(ValueError, "no successfully"):
            P.trace_workers(self.root)
        self.trace(EXEC + EXIT, 2)
        with self.assertRaisesRegex(ValueError, "lacks"):
            P.trace_workers(self.root)

    def test_attempt_failed_or_unexecuted_worker_is_not_a_witness(self):
        for text in (EXEC.replace('= 0', '= -1 ENOENT (No such file)'),
                     EXEC.replace('session-worker', 'some-other-command'),
                     KVM + EXEC + EXIT):
            self.trace(text)
            with self.subTest(text=text), self.assertRaises(ValueError):
                P.trace_workers(self.root)

    def test_kvm_requires_live_worker_vcpu_fd_and_successful_run(self):
        for changed in (KVM.replace('open("/dev/kvm", O_RDWR|O_CLOEXEC) = 3\n', ''),
                        KVM.replace('ioctl(3, KVM_CREATE_VM, 0) = 4\n', ''),
                        KVM.replace('ioctl(4, KVM_CREATE_VCPU, 0) = 5\n', ''),
                        KVM.replace('ioctl(5, KVM_RUN', 'ioctl(9, KVM_RUN'),
                        KVM.replace('ioctl(5, KVM_RUN, 0) = 0\n', ''),
                        KVM.replace('ioctl(5, KVM_RUN', 'close(5) = 0\nioctl(5, KVM_RUN')):
            self.trace(EXEC + changed + EXIT)
            with self.subTest(changed=changed), self.assertRaisesRegex(ValueError, "lacks"):
                P.trace_workers(self.root)

    def test_abnormal_or_missing_child_exit_refuses_even_with_guest_execution(self):
        for ending in ('', '+++ killed by SIGKILL +++\n', EXIT.replace('0', '2')):
            self.trace(EXEC + KVM + ending)
            with self.subTest(ending=ending), self.assertRaisesRegex(ValueError, "normal exit"):
                P.trace_workers(self.root)

    def command(self):
        serial = b'boot\n' + P.READBACK + b'\nHARMONY_OCI_EXIT rc=0\n'
        (self.root / 'serial.log').write_bytes(serial)
        record = {"container_rc": 0, "supervisor_failure_rc": None, "serial_sha256": P.digest(self.root / 'serial.log'), "steps": 3, "terminal": "Idle"}
        P.write(self.root / 'run.json', record)
        return record

    def test_nonempty_readback_and_full_serial_commitment_are_required(self):
        record = self.command()
        self.assertEqual(P.command_result(self.root), record)
        for serial in (b'', b'something else', P.READBACK + b'\n' + P.READBACK):
            (self.root / 'serial.log').write_bytes(serial)
            changed = {**record, 'serial_sha256': P.digest(self.root / 'serial.log')}
            P.write(self.root / 'run.json', changed)
            with self.subTest(serial=serial), self.assertRaisesRegex(ValueError, 'readback'):
                P.command_result(self.root)
        self.command()
        (self.root / 'serial.log').write_bytes(P.READBACK)
        with self.assertRaisesRegex(ValueError, 'commitment'):
            P.command_result(self.root)

    def test_command_failures_or_empty_work_cannot_pass(self):
        record = self.command()
        for key, value in (("container_rc", None), ("container_rc", 1), ("supervisor_failure_rc", 2), ("steps", 0), ("terminal", "")):
            P.write(self.root / 'run.json', {**record, key: value})
            with self.subTest(key=key), self.assertRaises(ValueError):
                P.command_result(self.root)

    def test_public_search_requires_completed_work_without_failures(self):
        record = {"mode": "search", "executions": 1, "execution_failures": 0, "watchdog_cutoffs": 0, "bug_found": False, "execution_ticks": 1, "workers": 1}
        P.write(self.root / 'report.json', record)
        P.write(self.root / 'campaign-summary.json', {"telemetry": {"workers": [{"jobs": 1, "target": {"vmm.transport.child.loop.endpoint.receive.completed": 3}}]}})
        self.assertEqual(P.search_result(self.root)['executions'], 1)
        for key, value in (("mode", "replay"), ("executions", 0), ("executions", 2), ("execution_failures", 1), ("watchdog_cutoffs", 1), ("bug_found", True), ("execution_ticks", 0), ("workers", 0)):
            P.write(self.root / 'report.json', {**record, key: value})
            with self.subTest(key=key), self.assertRaises(ValueError):
                P.search_result(self.root)

    def test_public_search_requires_endpoint_completion_on_the_job_worker(self):
        record = {"mode": "search", "executions": 1, "execution_failures": 0, "watchdog_cutoffs": 0, "bug_found": False, "execution_ticks": 1, "workers": 1}
        P.write(self.root / 'report.json', record)
        for target in ({}, {"vmm.transport.child.loop.endpoint.receive.completed": 0},
                       {"vmm.transport.child.loop.run.receive.completed": 1},
                       {"vmm.transport.child.loop.hello.receive.completed": 1},
                       {"vmm.transport.child.loop.identity.receive.completed": 1},
                       {"vmm.transport.child.loop.endpoint.receive.attempts": 1,
                        "vmm.transport.child.loop.endpoint.receive.errors": 1}):
            P.write(self.root / 'campaign-summary.json', {"telemetry": {"workers": [{"jobs": 1, "target": target}]}})
            with self.subTest(target=target), self.assertRaisesRegex(ValueError, "completed production worker"):
                P.search_result(self.root)
        P.write(self.root / 'report.json', {**record, "workers": 2})
        P.write(self.root / 'campaign-summary.json', {"telemetry": {"workers": [
            {"jobs": 1, "target": {"vmm.transport.child.loop.run.receive.completed": 1}},
            {"jobs": 0, "target": {"vmm.transport.child.loop.endpoint.receive.completed": 1}}]}})
        with self.assertRaisesRegex(ValueError, "completed production worker"):
            P.search_result(self.root)

    def test_public_manifest_requires_completed_strict_kvm_host(self):
        record = {"status": "complete", "error": None, "binary_identity": "linux-sealed-static-memfd-elf-sha256-v1:" + 'a' * 64, "runner_identity": {"backend": "kvm"}}
        P.write(self.root / 'manifest.json', record)
        self.assertEqual(P.manifest(self.root), record)
        for key, value in (("status", "running"), ("error", "failed"), ("binary_identity", "ordinary"), ("runner_identity", {"backend": "uml"})):
            P.write(self.root / 'manifest.json', {**record, key: value})
            with self.subTest(key=key), self.assertRaises(ValueError):
                P.manifest(self.root)

    def archive(self, entries):
        path = self.root / 'archive.tar.gz'
        with tarfile.open(path, 'w:gz') as archive:
            for name, data, kind in entries:
                member = tarfile.TarInfo(name)
                member.type = kind
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
        return path

    def test_only_exact_regular_package_members_are_extracted(self):
        archive = self.archive([('harmony', b'front', tarfile.REGTYPE), ('harmony-consonance-host', b'host', tarfile.REGTYPE)])
        P.extract(archive, self.root / 'unpacked', {name: name for name in ('harmony', 'harmony-consonance-host')})
        self.assertEqual((self.root / 'unpacked/harmony').read_bytes(), b'front')
        for entries in ([('../outside', b'x', tarfile.REGTYPE)], [('harmony', b'x', tarfile.SYMTYPE)], [('harmony', b'x', tarfile.REGTYPE)] * 2):
            archive = self.archive(entries)
            with self.subTest(entries=entries), self.assertRaises(ValueError):
                P.extract(archive, self.root / 'refused', {'harmony': 'harmony'})
        self.assertFalse((self.root / 'outside').exists())

    def test_matrix_cannot_claim_unavailable_transforms_or_drop_required_rows(self):
        record = P.registry()
        for key, value in (("version", 2), ("required_architectures", ["x86_64"]), ("transforms", {**record['transforms'], 'whole-state-equivalence': 'passed'}), ("transforms", {k: v for k, v in record['transforms'].items() if k != 'search-worker'})):
            changed = {**record, key: value}
            with patch.object(P, 'read', return_value=changed), self.subTest(key=key), self.assertRaises(ValueError):
                P.registry()

    def flow_inputs(self):
        header = bytearray(64)
        header[:6] = b"\x7fELF\x02\x01"
        header[18:20] = (62).to_bytes(2, 'little')
        fixture = self.root / 'fixture'
        fixture.write_bytes(header)
        cli = self.archive([(name, bytes(header), tarfile.REGTYPE) for name in ('harmony', 'harmony-consonance-host')]).rename(self.root / 'cli.tar.gz')
        source = P.load('runtime_test_source', P.ROOT / 'consonance/harmony-linux/scripts/runtime-artifacts.py')
        blobs = {'bzImage': b'owned-kernel', 'initramfs-oci.cpio.gz': b'owned-base'}
        manifest = {'version': 1, 'architecture': 'x86_64', 'source_digest': source.source_digest(P.ROOT),
                    'files': {name: P.hashlib.sha256(data).hexdigest() for name, data in blobs.items()}}
        blobs['runtime-manifest.json'] = json.dumps(manifest).encode()
        runtime = self.archive([('x86_64/' + name, data, tarfile.REGTYPE) for name, data in blobs.items()]).rename(self.root / 'runtime.tar.gz')
        return cli, runtime, fixture

    def fake_execution(self, arguments, log, cpus, trace=None):
        log.write_text('internal synthetic executor for boundary tests\n')
        destination = Path(arguments[arguments.index('--out') + 1])
        destination.mkdir()
        artifacts = destination / 'artifacts'
        artifacts.mkdir()
        (artifacts / 'kernel').write_bytes(b'owned-kernel')
        (artifacts / 'initramfs').write_bytes(b'owned-base' + b'composed-test-root')
        P.write(destination / 'manifest.json', {'status': 'complete', 'error': None,
                'binary_identity': 'linux-sealed-static-memfd-elf-sha256-v1:' + 'a' * 64,
                'runner_identity': {'backend': 'kvm'},
                'artifacts': {name: P.digest(artifacts / name) for name in ('kernel', 'initramfs')}})
        if trace:
            (trace / 'search.123').write_text(EXEC + KVM + EXIT)
            P.write(destination / 'report.json', {'mode': 'search', 'executions': 1, 'execution_failures': 0,
                    'watchdog_cutoffs': 0, 'bug_found': False, 'execution_ticks': 2, 'workers': 1})
            P.write(destination / 'campaign-summary.json', {'telemetry': {'workers': [{'jobs': 1,
                    'target': {'vmm.transport.child.loop.endpoint.receive.completed': 3}}]}})
        else:
            serial = P.READBACK + b'\n'
            (destination / 'serial.log').write_bytes(serial)
            P.write(destination / 'run.json', {'container_rc': 0, 'supervisor_failure_rc': None,
                    'serial_sha256': P.digest(destination / 'serial.log'), 'steps': 3, 'terminal': 'Idle'})

    def flow(self, inputs, output, execute=None, hardware=True):
        with patch.object(P.platform, 'system', return_value='Linux'), patch.object(P.platform, 'machine', return_value='x86_64'), \
                patch.object(P.os, 'access', return_value=hardware), patch.object(P.os, 'sched_getaffinity', return_value={0, 1}, create=True), \
                patch.object(P, 'execute', side_effect=execute or self.fake_execution), patch.object(P, 'host', return_value={"architecture": "x86_64", "kernel": "internal", "cpuinfo": "synthetic"}):
            return P.qualify(*inputs, output)

    def test_synthetic_public_flow_binds_inputs_and_keeps_unavailable_rows(self):
        inputs = self.flow_inputs()
        record = self.flow(inputs, self.root / 'positive')
        self.assertEqual(record['outcome'], 'passed')
        self.assertEqual(record['inputs'], record['inputs_after'])
        self.assertEqual(record['active_files'], record['active_files_after'])
        self.assertEqual(record['transforms']['search-worker']['workers'][0]['kvm_runs'], 1)
        self.assertTrue(all(record['transforms'][name]['outcome'] == 'passed' for name in ('direct', 'same-host-replay', 'search-worker')))
        self.assertTrue(all(record['transforms'][name]['outcome'] == 'unavailable' for name in P.UNAVAILABLE))

    def test_actual_failed_worker_or_changed_runtime_cannot_pass_public_flow(self):
        inputs = self.flow_inputs()
        for mode in ('missing-worker', 'missing-endpoint-work', 'wrong-kernel', 'wrong-base', 'wrong-readback', 'different-replay'):
            def changed(arguments, log, cpus, trace=None):
                self.fake_execution(arguments, log, cpus, trace)
                destination = Path(arguments[arguments.index('--out') + 1])
                if trace and mode == 'missing-worker':
                    (trace / 'search.123').write_text(KVM + EXIT)
                if trace and mode == 'missing-endpoint-work':
                    P.write(destination / 'campaign-summary.json', {'telemetry': {'workers': [{'jobs': 1,
                            'target': {'vmm.transport.child.loop.run.receive.completed': 3}}]}})
                if mode == 'wrong-readback' and not trace:
                    (destination / 'serial.log').write_bytes(b'other readback\n')
                if mode == 'different-replay' and destination.name == 'replay':
                    record = P.read(destination / 'run.json')
                    record['steps'] += 1
                    P.write(destination / 'run.json', record)
                if mode in ('wrong-kernel', 'wrong-base'):
                    name = 'kernel' if mode == 'wrong-kernel' else 'initramfs'
                    file = destination / 'artifacts' / name
                    file.write_bytes(b'other bytes')
                    record = P.read(destination / 'manifest.json')
                    record['artifacts'][name] = P.digest(file)
                    P.write(destination / 'manifest.json', record)
            output = self.root / mode
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                self.flow(inputs, output, changed)
            record = P.read(output / 'receipt.json')
            self.assertEqual(record['outcome'], 'failed')
            failed = 'search-worker' if mode in ('missing-worker', 'missing-endpoint-work') else 'same-host-replay' if mode == 'different-replay' else 'direct'
            self.assertEqual(record['transforms'][failed]['outcome'], 'failed')
            self.assertTrue(all(record['transforms'][name]['outcome'] == 'unavailable' for name in P.UNAVAILABLE))

    def test_archive_or_active_executable_changes_refuse_public_flow(self):
        inputs = self.flow_inputs()
        for mode in ('archive', 'active'):
            def changed(arguments, log, cpus, trace=None):
                self.fake_execution(arguments, log, cpus, trace)
                if trace:
                    path = inputs[0] if mode == 'archive' else trace.parent / 'bin/harmony-consonance-host'
                    with path.open('ab') as stream:
                        stream.write(b'changed')
            output = self.root / mode
            with self.subTest(mode=mode), self.assertRaisesRegex(ValueError, 'inputs changed'):
                self.flow(inputs, output, changed)
            record = P.read(output / 'receipt.json')
            self.assertEqual(record['outcome'], 'failed')
            self.assertFalse(any(row['outcome'] == 'passed' for row in record['transforms'].values()))

    def test_missing_kvm_is_blocked_and_nonzero_not_green_skip(self):
        inputs = self.flow_inputs()
        output = self.root / 'blocked'
        with self.assertRaisesRegex(ValueError, 'KVM is unavailable'):
            self.flow(inputs, output, hardware=False)
        self.assertEqual(P.read(output / 'receipt.json')['outcome'], 'blocked')
        self.assertFalse(any(row['outcome'] == 'passed' for row in P.read(output / 'receipt.json')['transforms'].values()))

    def test_evidence_archive_keeps_failed_diagnostics_without_packaging_host_or_ram(self):
        (self.root / 'trace').mkdir()
        (self.root / 'trace/search.123').write_text(EXEC + KVM + EXIT)
        P.write(self.root / 'receipt.json', {'outcome': 'failed'})
        (self.root / 'unrelated').write_text('must not be packaged')
        archive = self.root / 'evidence.tar.gz'
        P.archive_evidence(self.root, archive)
        with tarfile.open(archive) as stream:
            self.assertEqual(set(stream.getnames()), {'receipt.json', 'trace/search.123'})

    def test_release_check_uses_exact_publication_archives_and_normal_ci_runs_same_cell(self):
        import yaml
        release = yaml.safe_load((P.ROOT / '.github/workflows/release.yml').read_text())['jobs']
        check = release['packaged-composition']
        self.assertEqual(check['needs'], ['cli', 'guest'])
        self.assertEqual({row['isa'] for row in check['strategy']['matrix']['include']}, set(P.ARCHITECTURES))
        steps = str(check['steps'])
        self.assertIn('cli-${{ matrix.triple }}', steps)
        self.assertIn('guest-${{ matrix.isa }}', steps)
        self.assertIn('-Wl,-z,separate-code', steps)
        self.assertNotIn('cargo ', steps)
        self.assertIn('--cli-archive "packages/harmony-${{ github.ref_name }}-${{ matrix.triple }}.tar.gz"', steps)
        self.assertIn('packaged-composition', release['publish']['needs'])
        self.assertIn('harmony-composition-*.json', str(release['publish']['steps']))
        checks = yaml.safe_load((P.ROOT / '.github/workflows/harmony-workloads-languages-checks.yml').read_text())['jobs']
        self.assertEqual(checks['packaged-composition']['needs'], ['scope', 'guest', 'tools'])
        self.assertIn('scripts/qualify-packaged-host.py', str(checks['packaged-composition']['steps']))
        self.assertIn('-Wl,-z,separate-code', str(checks['packaged-composition']['steps']))


if __name__ == '__main__':
    unittest.main()
