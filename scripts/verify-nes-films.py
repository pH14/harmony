#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check that each recorded film is playable media carrying real game audio."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

SILENCE_DBFS = -90.0
FPS = 60
AUDIO_COVERAGE = 0.99
REPORT_NAME = 'film-verification.json'


def probe(*arguments):
    return subprocess.run(['ffprobe', '-v', 'error', *arguments], check=True,
                          capture_output=True, text=True).stdout.strip()


def audio_measure(path):
    """Decode the audio track. A digitally silent film is not game audio."""
    output = subprocess.run(['ffmpeg', '-hide_banner', '-nostats', '-i', str(path),
                             '-map', 'a:0', '-af', 'volumedetect', '-f', 'null', '-'],
                            check=True, capture_output=True, text=True).stderr
    volume = re.search(r'mean_volume:\s*(-?\d+(?:\.\d+)?) dB', output)
    samples = re.findall(r'n_samples:\s*(\d+)', output)
    if not volume or not samples:
        raise ValueError('ffmpeg reported no measured volume for the audio track')
    return float(volume.group(1)), max(int(value) for value in samples)


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def check_media(mp4, frames, minimum_seconds):
    """The playable-media checks that need no film record: frames, audio, volume."""
    problems = []
    read_frames = probe('-count_frames', '-select_streams', 'v:0',
                        '-show_entries', 'stream=nb_read_frames', '-of', 'csv=p=0', str(mp4))
    if read_frames != str(frames):
        problems.append(f'{mp4}: holds {read_frames} video frames, not {frames}')
    if probe('-select_streams', 'a:0', '-show_entries', 'stream=codec_type',
             '-of', 'csv=p=0', str(mp4)) != 'audio':
        problems.append(f'{mp4}: carries no audio stream')
        return problems
    duration = float(probe('-show_entries', 'format=duration', '-of', 'csv=p=0', str(mp4)))
    expected = frames / FPS
    if duration < minimum_seconds:
        problems.append(f'{mp4}: lasts {duration:.2f}s, under the {minimum_seconds}s floor')
    if abs(duration - expected) > 0.5:
        problems.append(f'{mp4}: lasts {duration:.2f}s but its frames cover {expected:.2f}s')
    rate = int(probe('-select_streams', 'a:0', '-show_entries', 'stream=sample_rate',
                     '-of', 'csv=p=0', str(mp4)))
    channels = int(probe('-select_streams', 'a:0', '-show_entries', 'stream=channels',
                         '-of', 'csv=p=0', str(mp4)))
    volume, samples = audio_measure(mp4)
    audio_seconds = samples / (rate * channels)
    if audio_seconds < expected * AUDIO_COVERAGE:
        problems.append(f'{mp4}: carries {audio_seconds:.2f}s of audio for {expected:.2f}s of video')
    if volume <= SILENCE_DBFS:
        problems.append(f'{mp4}: the audio track is silent at {volume} dBFS')
    if not problems:
        print(f'{mp4}: {read_frames} frames, {duration:.2f}s, {audio_seconds:.2f}s of audio, '
              f'mean volume {volume} dBFS')
    return problems


def verify(record, minimum_seconds, expected=None, input_path=None):
    directory = record.parent
    if not record.is_file():
        return [f'{record}: the index lists this film but its record is missing']
    film = json.loads(record.read_text())
    mp4 = directory / Path(film['capture']['mp4']).name
    problems = []
    if not mp4.is_file() or mp4.stat().st_size == 0:
        return [f'{record}: {mp4.name} is missing or empty']
    recorded = digest(mp4)
    if recorded != film['capture']['mp4_sha256']:
        problems.append(f'{mp4}: content does not match the recorded SHA-256')
    if expected is not None:
        if recorded != expected['capture']['mp4_sha256']:
            problems.append(f'{mp4}: is not the film the index recorded for this cell')
        if film.get('input_sha256') != expected.get('input_sha256'):
            problems.append(f'{mp4}: replays a different input from the one the index recorded')
        if film.get('identity') != expected.get('identity'):
            problems.append(f'{mp4}: carries a different scenario identity from the index')
    if input_path is not None and input_path.is_file():
        if film.get('input_sha256') != digest(input_path):
            problems.append(f'{mp4}: replays an input this cell never recorded')
    if not film.get('endpoint_verified'):
        problems.append(f'{record}: the capture did not verify its endpoint')
    frames = film['video']['frames']
    rate = film['video']['audio_sample_rate']
    if film['video']['audio_frames'] < frames * rate // FPS * AUDIO_COVERAGE:
        problems.append(f'{record}: records {film["video"]["audio_frames"]} audio samples '
                        f'for {frames} video frames')
    problems += check_media(mp4, frames, minimum_seconds)
    if not problems:
        print(f'{mp4}: {film["provenance"]}')
    return problems


def from_index(arguments):
    """Pair each index row with the film directory it names, keeping its record."""
    index = json.loads(arguments.index.read_text())
    matrix = arguments.matrix or arguments.index.parent
    for row in index['unavailable']:
        print(f'{row["cell"]}: media unavailable: {row["reason"]}', file=sys.stderr)
    if index['failed']:
        print('rendering failed for: ' + ', '.join(row['cell'] for row in index['failed']),
              file=sys.stderr)
    return [(row['cell'], matrix / row['cell'] / 'film/film.json', row,
             matrix / row['cell'] / 'campaign/witness-input.json')
            for row in index['films']], bool(index['failed'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('films', nargs='*', type=Path, help='film.json records to check')
    parser.add_argument('--index', type=Path, help='a films.json index whose cells must all be present')
    parser.add_argument('--matrix', type=Path, help='the run directory the index describes')
    parser.add_argument('--media', type=Path, help='an mp4 a workload wrote without a film record')
    parser.add_argument('--media-frames', type=int, help='the video frames --media must hold')
    parser.add_argument('--min-seconds', type=float, default=1.0)
    parser.add_argument('--require', type=int, default=1, help='minimum number of films to check')
    arguments = parser.parse_args()
    if arguments.media is not None:
        if arguments.media_frames is None:
            parser.error('--media needs --media-frames')
        problems = check_media(arguments.media, arguments.media_frames, arguments.min_seconds)
        for problem in problems:
            print('::error::' + problem, file=sys.stderr)
        return 1 if problems else 0
    records = [(str(record.parent.parent.name), record, None, None) for record in arguments.films]
    failed = False
    if arguments.index:
        if not arguments.index.is_file():
            print(f'{arguments.index}: no media index; nothing rendered this panel', file=sys.stderr)
            return 1
        listed, failed = from_index(arguments)
        records += listed
    if len(records) < arguments.require:
        print(f'{len(records)} films to check, fewer than the required {arguments.require}',
              file=sys.stderr)
        return 1
    verdicts = {cell: verify(record, arguments.min_seconds, expected, input_path)
                for cell, record, expected, input_path in records}
    if arguments.index:
        report = {'format': 'nes-film-verification-v1',
                  'verified': sorted(cell for cell, problems in verdicts.items() if not problems),
                  'problems': {cell: problems for cell, problems in verdicts.items() if problems}}
        matrix = arguments.matrix or arguments.index.parent
        (matrix / REPORT_NAME).write_text(json.dumps(report, indent=2, sort_keys=True) + '\n')
    problems = [problem for found in verdicts.values() for problem in found]
    for problem in problems:
        print('::error::' + problem, file=sys.stderr)
    return 1 if problems or failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
