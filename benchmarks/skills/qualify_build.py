# SPDX-License-Identifier: AGPL-3.0-or-later
"""No-model Linux qualification of isolated compilation and artifact collection."""
from __future__ import annotations

import argparse
import hashlib
import json
import tempfile
from pathlib import Path

from . import build, materials, sandbox


def _pair(root: Path, name: str, source: str, image: str) -> tuple[Path, str]:
    source_file = root / (name + '.c')
    source_file.write_text(source)
    destination = root / name
    materials.freeze_pair(destination, {'main.c': source_file}, {}, 'Compile the supplied program.', {'image': image})
    return destination, hashlib.sha256((destination / 'manifest.json').read_bytes()).hexdigest()


def qualify(image: str) -> dict:
    report = {
        'format': 'harmony-skill-build-qualification-v1',
        'image': image, 'model_calls': 0, 'qualified': False, 'checks': {},
        'not_claimed': ['guest boot', 'general built-source equivalence', 'checker grading', 'held-out grading'],
    }
    limits = sandbox.Limits(wall=30, toolcalls=2, memorybytes=256 * 1024**2,
                            workbytes=16 * 1024**2, cpus=1, pids=64, outputbytes=512 * 1024)
    cap = 64 * 1024
    compile_argv = ['/bin/sh', '-c', '/usr/bin/cc -O0 main.c -o app && ./app']
    with tempfile.TemporaryDirectory(prefix='harmony-build-qualification-') as temporary:
        root = Path(temporary)
        pairs = {}
        for label, value in (('first', 42), ('changed', 43)):
            pairs[label] = _pair(root, label, '#include <stdio.h>\nint main(void) { puts("%s"); return 0; }\n' % value, image)
        pairs['invalid'] = _pair(root, 'invalid', 'this is deliberately invalid C\n', image)

        def run(pair, argv, outputs):
            return build.build_frozen(pair[0], pair[1], image, argv, outputs, limits, cap)

        def check(name, function):
            try:
                detail = function()
            except Exception as error:
                report['checks'][name] = {'passed': False, 'detail': str(error)}
            else:
                report['checks'][name] = {'passed': True, 'detail': detail}

        results = {}

        def positive(label, expected):
            result = run(pairs[label], compile_argv, ['app'])
            if result.stdout != expected or result.manifest_sha256 != pairs[label][1] or result.image != image:
                raise RuntimeError('compiled execution or input receipt mismatch')
            if result.argv != tuple(compile_argv) or len(result.artifacts) != 1:
                raise RuntimeError('build recipe or artifact set mismatch')
            artifact = result.artifacts[0]
            if artifact.path != 'app' or not artifact.data.startswith(b'\x7fELF') or not artifact.executable:
                raise RuntimeError('compiler did not deliver the executable ELF artifact')
            if hashlib.sha256(artifact.data).hexdigest() != artifact.sha256:
                raise RuntimeError('collected artifact digest mismatch')
            materials.verify_pair(pairs[label][0])
            results[label] = result
            return {'manifest_sha256': result.manifest_sha256, 'artifact_sha256': artifact.sha256,
                    'artifact_bytes': len(artifact.data), 'observed_stdout': expected.decode()}

        check('compile_and_execute_first_source', lambda: positive('first', b'42\n'))
        check('compile_and_execute_changed_source', lambda: positive('changed', b'43\n'))

        def changed():
            if set(results) != {'first', 'changed'}:
                raise RuntimeError('both actual builds must pass before comparing source changes')
            if results['first'].manifest_sha256 == results['changed'].manifest_sha256:
                raise RuntimeError('source change did not change frozen input identity')
            if results['first'].artifacts[0].sha256 == results['changed'].artifacts[0].sha256:
                raise RuntimeError('source change did not change delivered executable')
            return 'controlled source change alters both delivered bytes and observed output'

        check('controlled_source_change', changed)

        def reject(pair, argv, outputs, phase):
            try:
                run(pair, argv, outputs)
            except build.BuildError as error:
                result = error.result
                if error.phase != phase or result is None or result.exit_code is None or result.exit_code <= 0 or result.termination != 'exited':
                    raise RuntimeError('expected completed-command rejection; got another failure') from error
                return {'phase': phase, 'exit_code': result.exit_code}
            raise RuntimeError('invalid build or artifact was accepted')

        check('compiler_error_rejected', lambda: reject(pairs['invalid'], compile_argv, ['app'], 'build'))
        check('missing_artifact_rejected', lambda: reject(pairs['first'], ['/bin/true'], ['absent'], 'collect'))
        check('symlink_artifact_rejected', lambda: reject(pairs['first'], ['/bin/ln', '-s', '/etc/passwd', 'app'], ['app'], 'collect'))
        check('fifo_artifact_rejected', lambda: reject(pairs['first'], ['/usr/bin/mkfifo', 'app'], ['app'], 'collect'))
        check('oversize_artifact_rejected', lambda: reject(pairs['first'], ['/usr/bin/python3', '-I', '-S', '-c', 'open("app","wb").write(b"x" * 65537)'], ['app'], 'collect'))
    report['qualified'] = bool(report['checks']) and all(item['passed'] for item in report['checks'].values())
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    args = parser.parse_args()
    try:
        report = qualify(args.image)
    except Exception as error:
        report = {'qualified': False, 'model_calls': 0, 'error': str(error)}
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report['qualified'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
