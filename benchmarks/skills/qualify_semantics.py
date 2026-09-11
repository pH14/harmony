# SPDX-License-Identifier: AGPL-3.0-or-later
"""No-model qualification of private behavioral and guest checker controls."""
from __future__ import annotations

import argparse
import hashlib
import json
import tempfile
import time
from pathlib import Path

from . import artifact_run, behavior, build, checker_guest, guest_limits, materials, semantic_cases, sandbox
from .qualify_guest import _digest


_RECIPE = ['/usr/bin/cc', '-static', '-O0', 'main.c', '-o', 'app']
_VARIANTS = ('valid', 'always-pass', 'always-fail', 'silent', 'changed-application')
_SKILLS = ('harmony-properties', 'harmony-instrument', 'harmony-build', 'harmony-run')


def qualify(args: argparse.Namespace) -> dict:
    output = args.output.resolve()
    mount = guest_limits.verify_output_mount(output.parent)
    temporary = Path(tempfile.gettempdir()).resolve(strict=True)
    if (not temporary.is_relative_to(output.parent)
            or temporary.stat().st_dev != output.parent.stat().st_dev):
        raise ValueError('host TMPDIR must be inside the verified output mount')
    output.mkdir(exist_ok=False)
    paths = {name: getattr(args, name).resolve(strict=True)
             for name in ('harmony', 'kernel', 'base', 'agent', 'preparer')}
    pins = {name: _digest(path) for name, path in paths.items()}
    build_limits = sandbox.Limits(wall=40, toolcalls=2, memorybytes=256 * 1024**2,
                                  workbytes=32 * 1024**2, cpus=1, pids=64, outputbytes=8 * 1024**2)
    run_limits = sandbox.Limits(wall=10, toolcalls=1, memorybytes=128 * 1024**2,
                                workbytes=8 * 1024**2, cpus=1, pids=32, outputbytes=4096)
    report = {'format': 'harmony-skill-semantic-qualification-v1', 'qualified': False,
              'model_calls': 0, 'compiler_image': args.image, 'input_sha256': pins,
              'output_mount': mount, 'host_tmpdir': str(temporary), 'cases': {},
              'scope': 'finite private functional domains and benign checker controls',
              'not_claimed': ['general program equivalence', 'skill effectiveness', 'paid model evaluation']}
    deadline = time.monotonic() + 1200

    def budget() -> None:
        # Reserve enough time for one bounded service call, including setup and
        # cleanup. No new call starts near the overall qualification deadline.
        if time.monotonic() + 120 > deadline:
            raise TimeoutError('qualification budget exhausted before the next service call')

    selected = semantic_cases.cases()
    semantic_cases.validate_cases(selected)
    repository = Path(__file__).resolve().parents[2]
    skills_root = repository / 'skills'
    common_docs = {
        'docs/faults.md': repository / 'workloads/faults/README.md',
        'docs/fault-agent.md': repository / 'workloads/fault-agent/README.md',
        'docs/sdk.md': repository / 'consonance/harmony-linux/sdk/README.md',
    }
    frozen = []
    for case in selected:
        root = output / case.name
        root.mkdir()
        original = root / 'original.c'
        original.write_bytes(case.source)
        cohort = root / 'cohort'
        manifest = materials.freeze_pair(cohort, {'main.c': original, **common_docs},
                              {name: skills_root / name for name in _SKILLS}, semantic_cases.TASK,
                              {'image': args.image, 'build_command': _RECIPE,
                               'build_limits': vars(build_limits), 'execution_limits': vars(run_limits)})
        manifest = materials.verify_pair(cohort)
        cohort_pin = _digest(cohort / 'manifest.json')
        case_report = {'split': case.split, 'cohort_manifest_sha256': cohort_pin,
                       'source_sha256': hashlib.sha256(case.source).hexdigest(), 'submissions': {}}
        report['cases'][case.name] = case_report
        identity = {'files': manifest['treatment'], 'directories': manifest['treatment_dirs']}
        encoded = json.dumps(identity, sort_keys=True, separators=(',', ':')).encode()
        skill_pin = hashlib.sha256(encoded).hexdigest()
        # Generic skills were frozen before either private case was authored.
        # Bind bytes, executable modes and topology, not just equality of arms.
        if skill_pin != 'c73e37ccee00859e7ddda43133bc8e300553d3ea9341e9c74b88d9cd9245ae36':
            raise ValueError('generic skills differ from the frozen baseline')
        case_report['generic_skills_sha256'] = skill_pin
        case_report['generic_skills_revision'] = '74b137051dba324a8ac56948f17a1cac07f5b29b'
        frozen.append((case, root, cohort, cohort_pin, case_report))
    # Freeze all paired initial materials before executing any submission.
    for case, root, cohort, cohort_pin, case_report in frozen:
        for variant in _VARIANTS:
            location = root / variant
            location.mkdir()
            try:
                source = location / 'main.c'
                source.write_bytes(semantic_cases.qualification_source(case, variant))
                pair = location / 'submission'
                materials.freeze_pair(pair, {'main.c': source}, {}, 'Compile the frozen submission.',
                                      {'image': args.image, 'command': _RECIPE})
                source_pin = _digest(pair / 'manifest.json')
                budget()
                compiled = build.build_frozen(pair, source_pin, args.image, _RECIPE, ['app'], build_limits, 4 * 1024**2)
                if (compiled.manifest_sha256 != source_pin or compiled.image != args.image
                        or compiled.argv != tuple(_RECIPE) or len(compiled.artifacts) != 1
                        or compiled.artifacts[0].path != 'app'):
                    raise ValueError('compiled receipt does not match the frozen source and recipe')
                artifact = compiled.artifacts[0]
                (location / 'app.bin').write_bytes(artifact.data)
                # The grader dispatches each input in a fresh container. The
                # private expected values are never passed into that container.
                original_run = artifact_run.run_artifact

                def limited_run(*arguments, **keywords):
                    budget()
                    return original_run(*arguments, **keywords)

                # Grading accepts a dispatcher so the controller can enforce
                # the shared budget without changing sandbox or process state.
                native = behavior.grade(case, artifact, args.image, run_limits, run=limited_run)
                observation = {'source_manifest_sha256': source_pin, 'artifact_sha256': artifact.sha256,
                               'behavior': native, 'checker_controls': []}
                if variant == 'changed-application':
                    accepted = native['status'] == 'mismatch'
                elif native['status'] != 'passed':
                    accepted = False
                else:
                    accepted = True
                    for index, control in enumerate(case.controls):
                        budget()
                        destination = location / ('control-%02d' % index)
                        destination.mkdir()
                        evidence = checker_guest.check_artifact(artifact, control.arguments, control.holds,
                                                                paths, pins, destination)
                        observation['checker_controls'].append(evidence)
                        if variant == 'silent':
                            expected = 'no_telemetry'
                        elif variant == 'always-pass':
                            expected = 'passed' if control.holds else 'mismatch'
                        elif variant == 'always-fail':
                            expected = 'mismatch' if control.holds else 'passed'
                        else:
                            expected = 'passed'
                        accepted = accepted and evidence['status'] == expected
                materials.verify_pair(pair)
                if _digest(pair / 'manifest.json') != source_pin:
                    raise ValueError('submission manifest changed during qualification')
                observation['passed'] = accepted
                case_report['submissions'][variant] = observation
            except Exception as error:
                case_report['submissions'][variant] = {'passed': False, 'error': str(error)}
        materials.verify_pair(cohort)
        if _digest(cohort / 'manifest.json') != cohort_pin:
            raise ValueError('paired cohort changed during qualification')
    if any(_digest(path) != pins[name] for name, path in paths.items()):
        raise ValueError('pinned tool inputs changed during qualification')
    report['qualified'] = all(submission['passed'] for case in report['cases'].values()
                              for submission in case['submissions'].values())
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    for name in ('harmony', 'kernel', 'base', 'agent', 'preparer', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    try:
        report = qualify(args)
    except Exception as error:
        report = {'qualified': False, 'model_calls': 0, 'error': str(error)}
    print(json.dumps(report, sort_keys=True, indent=2))
    return 0 if report['qualified'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
