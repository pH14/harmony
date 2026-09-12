#!/usr/bin/env python3
"""Audit existing ordinary controls; allocate no emulator work."""

import argparse
from fractions import Fraction
import hashlib
import json
import math
from pathlib import Path
import subprocess
import tarfile


SOURCE_COMMIT = '780415bdea636c2bd54cf3d865fc264dd33fd649'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def function(text, signature):
    """Extract the selected brace-balanced source functions, not Rust generally."""
    start = text.index(signature)
    opening = text.index('{', start)
    depth = 0
    for end in range(opening, len(text)):
        depth += (text[end] == '{') - (text[end] == '}')
        if depth == 0:
            return text[start:end + 1]
    raise ValueError('Unclosed selected function')


def source_checks(repo):
    specs = {
        'dissonance/searcher/src/search/campaign.rs': [
            'fn all_prefixes_archived(',
            'fn note_dispatch<G:',
        ],
        'dissonance/searcher/src/search/archive.rs': [
            'pub(crate) fn all_extensions_retained(',
            'pub(crate) fn job_origin(',
        ],
        'dissonance/searcher/src/search/draw.rs': ['pub fn draw_suffix<'],
        'workloads/nes/src/metroid/archive.rs': ['pub fn sample_chord('],
    }
    rows = []
    for name, signatures in specs.items():
        old = subprocess.check_output(
            ['git', 'show', f'{SOURCE_COMMIT}:{name}'], cwd=repo
        ).decode()
        current = (repo / name).read_text()
        matches = []
        for signature in signatures:
            before = function(old, signature)
            after = function(current, signature)
            assert before == after, (name, signature)
            matches.append({'signature': signature, 'sha256': digest(before.encode())})
        if name.endswith('/campaign.rs'):
            constant = 'const CONSECUTIVE_SKIP_LIMIT: u64 = 1_024;'
            assert constant in old and constant in current
            ordinary = 'let all_prefixes_archived = consecutive_skips < CONSECUTIVE_SKIP_LIMIT'
            start_old, start_new = old.index(ordinary), current.index(ordinary)
            end_old = old.index('*reserved = reserved.saturating_add(1);', start_old)
            end_new = current.index('*reserved = reserved.saturating_add(1);', start_new)
            assert old[start_old:end_old] == current[start_new:end_new]
        rows.append({'path': name, 'registered_source_file_sha256': digest(old.encode()),
                     'unchanged_functions': matches})
    return rows


def finite_inequality():
    # exp(-1) <= 3/8, and exp(L/10) <= 10/(10-L), for L in0..6.
    # This rational envelope is sufficient; no floating-point proof premise.
    rows = []
    for length in range(7):
        envelope = (Fraction(length, 6) * Fraction(3, 8)
                    + Fraction(6 - length, 6) * Fraction(10, 10 - length))
        assert envelope <= 1
        rows.append({'retained_prefix_actions': length,
                     'exponential_moment_upper_bound': str(envelope)})
    # A length-conditioned action tape invalidates the theorem: choose a fresh
    # first action for N=1 and N-1 known actions followed by a fresh one otherwise.
    adversary = [{'length': n, 'known_prefix_frames': 120 * (n - 1), 'skipped': 0}
                 for n in range(1, 7)]
    assert sum(r['known_prefix_frames'] for r in adversary) > 0
    assert sum(r['skipped'] for r in adversary) == 0
    return rows, adversary


def audit(repo):
    root = repo / 'benchmarks/search/continuation-reassessment'
    controls, all_cells = [], []
    for phase, pairs in [('cd01', range(4)), ('cc01', range(2))]:
        master = json.loads((root / f'{phase}-registration.json').read_text())
        for pair in pairs:
            base = root / f'{phase}-output'
            name = f'{phase}-pair-{pair}'
            raw = base / f'{name}-native.tar.gz'
            verification = json.loads((base / f'{name}-verification.json').read_text())
            panel_path = base / f'{name}-results.json'
            assert verification['verified']
            assert digest(raw.read_bytes()) == verification['native_archive_sha256']
            assert digest(panel_path.read_bytes()) == verification['panel_sha256']
            members = {m['path']: m['sha256'] for m in verification['members']}
            records = json.loads(panel_path.read_text())['records']
            with tarfile.open(raw) as archive:
                for record in records:
                    summary = record['summary']
                    progress = summary['last_progress']
                    profile = progress['coordinator']
                    assert record['checks_passed'] and record['exit_code'] == 0
                    assert profile['enabled']
                    assert profile['admissions'] == progress['executions']
                    assert profile['replay_actions'] == profile['replay_jobs'] == profile['replay_time'] == 0
                    assert summary['build']['source']['source_commit'] == SOURCE_COMMIT
                    assert summary['result']['stream_retained'] is False
                    report_paths = [m for m in members
                                    if f"/{record['id']}/" in m and m.endswith('/campaign/campaign.json')]
                    assert len(report_paths) == 1
                    report_bytes = archive.extractfile(report_paths[0]).read()
                    assert digest(report_bytes) == members[report_paths[0]]
                    report = json.loads(report_bytes)
                    assert report['executions_completed'] == progress['executions']
                    assert report['frames_emulated'] == progress['frames_emulated']
                    row = {'id': record['id'], 'native_archive_sha256': verification['native_archive_sha256'],
                           'report_path': report_paths[0], 'report_sha256': digest(report_bytes),
                           'frames': progress['frames_emulated'], 'jobs': progress['executions'],
                           'duplicates_skipped': report['duplicates_skipped'],
                           'origin_replay_frames': profile['replay_time'],
                           'complete_job_stream_retained': summary['result']['stream_retained']}
                    all_cells.append(row)
                    if not record['id'].endswith('-control'):
                        continue
                    assert report['mixture_policy'] == 'alphabet_only'
                    assert report['suffix_policy'] == 'one_to_six'
                    assert report['duration_policy'] == 'stratified_short_or_long_v1'
                    assert report['duplicates_skipped'] < 1024, 'Forced duplicate execution not excluded'
                    assert master['horizon_frames'] == 200_000_000
                    controls.append(row)
    assert len(controls) == 6 and len(all_cells) == 12
    count = sum(r['duplicates_skipped'] for r in controls)
    frames = sum(r['frames'] for r in controls)
    jobs = sum(r['jobs'] for r in controls)
    rational, adversary = finite_inequality()
    # Simultaneous bounds for all eight preregistered controls, including the
    # two unrun ones, avoid pooling assumptions or selection at a favorable stop.
    registered_controls = 8
    per_control_failure = 0.05 / registered_controls
    upper = 1200 * (count + len(controls) * math.log(1 / per_control_failure))
    return {
        'format': 'alphabet-prefix-opportunity-audit-v1',
        'scope': 'Offline descriptive opportunity assessment; no pooling of efficacy panels or new native work.',
        'source_commit': SOURCE_COMMIT,
        'source_checks': source_checks(repo),
        'cells': all_cells,
        'ordinary_control_totals': {'cells': 6, 'jobs': jobs, 'duplicates_skipped': count,
                                    'draws': jobs + count, 'frames': frames,
                                    'duplicate_skip_fraction_of_draws': count / (jobs + count)},
        'model': {
            'assumption': 'At every adaptive selection, N is uniform on1..6 and independent of a fresh latent six-action tape, conditional on prior history. This ideal-random-draw premise is not a proved property of the deterministic Romu PRNG.',
            'saving_scope': 'At most the hold time of the contiguous currently retained prefix in a dispatched ordinary suffix, on the original observed selection history. No reordering, failed-state cache, nonretained intermediate shortcut, or future adaptive trajectory effect is bounded.',
            'max_action_frames': 120, 'exponential_scale_frames': 1200,
            'finite_rational_moment_bounds': rational,
            'length_dependent_tape_counterexample': adversary,
            'supermartingale': 'exp(sum W /1200 - sum Y), with W the ideal eligible prefix work and Y the existing duplicate-skip indicator; stop the process immediately before its first forced duplicate execution. Every observed control is before that guard.',
            'registered_control_trajectories': registered_controls,
            'per_control_time_uniform_failure_probability_under_model': per_control_failure,
            'familywise_time_uniform_failure_probability_under_model': 0.05,
            'upper_frames_under_model': upper, 'upper_fraction_under_model': upper / frames,
            'interpretation': 'A conditional mathematical opportunity bound, not observed saved work, a PRNG certificate, a native confidence claim, or a causal discovery improvement.'
        },
        'decision': 'Do not fund ordinary retained-prefix fast-forward or additional duplicate/exposure telemetry from this evidence. Existing full-prefix suppression and zero origin replay defeat the simpler proposals. Direct partial-prefix work opportunity is small under the model, and there is no evidence of a compensating adaptive discovery gain.',
        'emulated_frames': 0,
        'unknowns': ['Exact attempted-action history and partially known prefix hits are not recoverable from these saved aggregate artifacts.',
                     'Unretained/dead-action repetition is outside the skip-count bound; parent counters cannot estimate it.',
                     'Changing an adaptive search history can change downstream utility; the opportunity bound is not search dominance.'],
    }


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path(__file__).resolve().parents[3])
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    assert not args.out.exists()
    args.out.write_text(json.dumps(audit(args.repo.resolve()), indent=2) + '\n')
