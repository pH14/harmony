# SPDX-License-Identifier: AGPL-3.0-or-later
"""Grade observed integer-valued application behavior against private cases."""
from __future__ import annotations

from . import artifact_run, build, sandbox, semantic_cases


def _integers(output: bytes) -> tuple[int, ...] | None:
    if type(output) is not bytes or len(output) > 4096:
        return None
    words = output.split()
    if not 1 <= len(words) <= 16:
        return None
    values = []
    for word in words:
        digits = word[1:] if word[:1] in (b'+', b'-') else word
        if not digits or len(digits) > 20 or not digits.isdigit():
            return None
        values.append(int(word))
    return tuple(values)


def grade(case: semantic_cases.Case, artifact: build.Artifact, image: str,
          limits: sandbox.Limits, *, run=None) -> dict:
    """Execute the same collected artifact freshly for every private input.

    A mismatch is a completed application execution with the wrong observable
    values. Timeouts, signals, nonzero exit statuses and service failures do not
    masquerade as a successfully rejected semantic negative control.
    """
    if not case.behavior:
        raise ValueError('behavioral grading requires a nonempty private contract')
    dispatch = artifact_run.run_artifact if run is None else run
    records = []
    for sample in case.behavior:
        actual = dispatch(artifact, image, list(sample.arguments), limits)
        if (actual.artifact_sha256 != artifact.sha256 or actual.image != image
                or actual.argv != sample.arguments):
            raise ValueError('artifact execution receipt does not match the requested observation')
        result = actual.result
        record = {'arguments': list(sample.arguments), 'stdout_hex': result.stdout.hex(),
                  'stderr_hex': result.stderr.hex(), 'exit_code': result.exit_code,
                  'termination': result.termination}
        records.append(record)
        if result.termination != 'exited' or result.exit_code != 0:
            return {'status': 'execution_error', 'checked': len(records), 'total': len(case.behavior),
                    'artifact_sha256': artifact.sha256, 'observations': records}
        values = _integers(result.stdout)
        record['observed'] = list(values) if values is not None else None
        record['expected'] = list(sample.expected)
        if values != sample.expected:
            return {'status': 'mismatch', 'checked': len(records), 'total': len(case.behavior),
                    'artifact_sha256': artifact.sha256, 'observations': records}
    return {'status': 'passed', 'checked': len(records), 'total': len(case.behavior),
            'artifact_sha256': artifact.sha256, 'observations': records}
