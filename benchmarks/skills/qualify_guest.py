# SPDX-License-Identifier: AGPL-3.0-or-later
"""Build and boot fixed benign SDK-delivery fixtures; no semantic grading."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import tempfile
from pathlib import Path

from . import build, guest_evidence, guest_files, guest_image, guest_limits, materials, sandbox

_SOURCE = r'''#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc != 2) return 64;
    if (!strcmp(argv[1], "ready")) return 0;
    if (!strcmp(argv[1], "idle")) { for (;;) pause(); }
    if (strcmp(argv[1], "check")) return 65;
#if SILENT
    puts("ordinary output without SDK directives");
#else
    puts("@reachable 7");
    puts(CONDITION ? "@always 8 1" : "@always 8 0");
#endif
    return 0;
}
'''
_BUNDLE = b'ready /app/app ready\nnode idle /app/app idle\nhook 1 /app/app check\n'


def _digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def _prepared_digest(preparer: Path, image: Path, base: Path, agent: Path,
                     env: dict[str, str]) -> str:
    outcome = sandbox._run_bounded([str(preparer), str(image), str(base), str(agent)],
                                   timeout=30, output_limit=4096, env=env)
    if (outcome.exit_code != 0 or outcome.timed_out or outcome.output_overflow
            or re.fullmatch(rb"[0-9a-f]{64}\n", outcome.stdout) is None):
        raise ValueError('trusted image preparation did not return a bounded digest: '
                         + outcome.stderr.decode('utf-8', errors='replace'))
    return outcome.stdout.decode('ascii').strip()


def qualify(args: argparse.Namespace) -> dict:
    output = args.output.resolve()
    mount = guest_limits.verify_output_mount(output.parent)
    host_tmpdir = Path(tempfile.gettempdir()).resolve(strict=True)
    if (not host_tmpdir.is_relative_to(output.parent)
            or host_tmpdir.stat().st_dev != output.parent.stat().st_dev):
        raise ValueError('host TMPDIR must be inside the verified output mount before Python starts')
    output.mkdir(exist_ok=False)
    paths = {name: getattr(args, name).resolve(strict=True) for name in ('harmony', 'kernel', 'base', 'agent', 'preparer')}
    pins = {name: _digest(path) for name, path in paths.items()}
    result = {
        'format': 'harmony-skill-guest-qualification-v1', 'qualified': False,
        'model_calls': 0, 'compiler_image': args.image, 'input_sha256': pins, 'checks': {},
        'output_mount': mount, 'host_tmpdir': str(host_tmpdir),
        'trust_boundary': 'controller-selected pinned CLI and preparer; compiled artifacts execute only in the guest',
        'not_claimed': ['meaningful checker grading', 'general built-source equivalence', 'held-out grading'],
    }
    limits = sandbox.Limits(wall=40, toolcalls=2, memorybytes=256 * 1024**2,
                            workbytes=32 * 1024**2, cpus=1, pids=64, outputbytes=8 * 1024**2)
    for label, condition, silent in (('positive', 1, 0), ('violation', 0, 0), ('silent', 1, 1)):
        case = output / label
        case.mkdir()
        try:
            source = case / 'main.c'
            source.write_text(_SOURCE)
            pair = case / 'source'
            command = ['/usr/bin/cc', '-static', '-O0', '-DCONDITION=%d' % condition,
                       '-DSILENT=%d' % silent, 'main.c', '-o', 'app']
            materials.freeze_pair(pair, {'main.c': source}, {}, 'Compile the supplied fixture.',
                                  {'image': args.image, 'command': command})
            manifest = _digest(pair / 'manifest.json')
            compiled = build.build_frozen(pair, manifest, args.image, command, ['app'], limits, 4 * 1024**2)
            artifact = compiled.artifacts[0]
            if not artifact.executable or not artifact.data.startswith(b'\x7fELF'):
                raise ValueError('compiler did not deliver an executable ELF')
            (case / 'app.bin').write_bytes(artifact.data)
            archive = guest_image.package_artifacts(compiled.artifacts, _BUNDLE)
            image_path = case / 'fixture.tar'
            image_path.write_bytes(archive)
            action_path = case / 'actions.json'
            action_path.write_text('[{"Hook":1},"Wait"]\n')
            home = case / 'home'
            home.mkdir()
            temporary = case / 'tmp'
            temporary.mkdir()
            env = {'PATH': '/usr/bin:/bin', 'HOME': str(home), 'TMPDIR': str(temporary),
                   'XDG_CACHE_HOME': str(home / 'cache'), 'LC_ALL': 'C.UTF-8'}
            inputs = {'archive': image_path, 'actions': action_path, 'artifact': case / 'app.bin'}
            input_pins = {name: _digest(path) for name, path in inputs.items()}
            for path in inputs.values():
                path.chmod(0o444)
            prepared = _prepared_digest(paths['preparer'], image_path, paths['base'], paths['agent'], env)
            argv = [str(paths['harmony']), 'search', '--package', 'faults', str(image_path),
                    '--kernel', str(paths['kernel']), '--base-initramfs', str(paths['base']),
                    '--fault-agent', str(paths['agent']), '--replay', str(action_path), '--repeat', '1',
                    '--seed', '1', '--workers', '1', '--horizon-ms', '1000', '--ram-mib', '512',
                    '--wall-minutes', '1', '--out', str(case / 'run')]
            (case / 'invocation.json').write_text(json.dumps(argv, indent=2))
            # Anchor the case before executing the trusted CLI: evidence cannot
            # redirect reads through a subsequently replaced path or symlink.
            case_fd = os.open(case, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                outcome = sandbox._run_bounded(argv, timeout=90, output_limit=1024**2, env=env)
                (case / 'cli.stdout').write_bytes(outcome.stdout)
                (case / 'cli.stderr').write_bytes(outcome.stderr)
                if outcome.exit_code != 0 or outcome.timed_out or outcome.output_overflow:
                    raise ValueError('guest CLI did not complete within its bounds')
                if (any(_digest(path) != pins[name] for name, path in paths.items())
                        or any(_digest(path) != input_pins[name] for name, path in inputs.items())):
                    raise ValueError('pinned execution inputs changed')
                materials.verify_pair(pair)
                if _digest(pair / 'manifest.json') != manifest:
                    raise ValueError('frozen source manifest changed')
                report = guest_files.read_json_at(case_fd, 'run/report.json')
                events = guest_files.read_json_at(case_fd, 'run/replay-1-events.json')
            finally:
                os.close(case_fd)
            if report.get('image_sha256') != prepared:
                raise ValueError('report image does not match the prepared supplied archive')
            if silent:
                try:
                    guest_evidence.validate_fixture(report, events, pins['kernel'], pins['agent'], violation=False)
                except guest_evidence.EvidenceError as error:
                    if error.code != 'missing_hit':
                        raise
                else:
                    raise ValueError('silent fixture was accepted as SDK-delivery evidence')
                evidence = {'missing_sdk_delivery_rejected': True}
            else:
                evidence = guest_evidence.validate_fixture(report, events, pins['kernel'], pins['agent'], violation=not bool(condition))
            result['checks'][label] = {'passed': True, 'artifact_sha256': artifact.sha256,
                                       'image_archive_sha256': input_pins['archive'],
                                       'actions_sha256': input_pins['actions'],
                                       'prepared_image_sha256': prepared,
                                       'source_manifest_sha256': manifest, 'evidence': evidence}
        except Exception as error:
            result['checks'][label] = {'passed': False, 'detail': str(error)}
    result['qualified'] = all(check['passed'] for check in result['checks'].values())
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    for name in ('harmony', 'kernel', 'base', 'agent', 'preparer', 'output'):
        parser.add_argument('--' + name, required=True, type=Path)
    args = parser.parse_args()
    try:
        result = qualify(args)
    except Exception as error:
        result = {'qualified': False, 'model_calls': 0, 'error': str(error)}
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result['qualified'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
