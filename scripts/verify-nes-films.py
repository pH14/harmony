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


def probe(*arguments):
    return subprocess.run(['ffprobe', '-v', 'error', *arguments], check=True,
                          capture_output=True, text=True).stdout.strip()


def mean_volume(path):
    """Decode the audio track. A digitally silent film is not game audio."""
    output = subprocess.run(['ffmpeg', '-hide_banner', '-nostats', '-i', str(path),
                             '-map', 'a:0', '-af', 'volumedetect', '-f', 'null', '-'],
                            check=True, capture_output=True, text=True).stderr
    found = re.search(r'mean_volume:\s*(-?\d+(?:\.\d+)?) dB', output)
    if not found:
        raise ValueError('ffmpeg reported no measured volume for the audio track')
    return float(found.group(1))


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def verify(record, minimum_seconds):
    directory = record.parent
    film = json.loads(record.read_text())
    mp4 = directory / Path(film['capture']['mp4']).name
    problems = []
    if not mp4.is_file() or mp4.stat().st_size == 0:
        return [f'{record}: {mp4.name} is missing or empty']
    if digest(mp4) != film['capture']['mp4_sha256']:
        problems.append(f'{mp4}: content does not match the recorded SHA-256')
    if not film.get('endpoint_verified'):
        problems.append(f'{record}: the capture did not verify its endpoint')
    frames = probe('-count_frames', '-select_streams', 'v:0',
                   '-show_entries', 'stream=nb_read_frames', '-of', 'csv=p=0', str(mp4))
    if frames != str(film['video']['frames']):
        problems.append(f'{mp4}: holds {frames} video frames, not {film["video"]["frames"]}')
    if probe('-select_streams', 'a:0', '-show_entries', 'stream=codec_type',
             '-of', 'csv=p=0', str(mp4)) != 'audio':
        problems.append(f'{mp4}: carries no audio stream')
        return problems
    duration = float(probe('-show_entries', 'format=duration', '-of', 'csv=p=0', str(mp4)))
    expected = film['video']['frames'] / 60
    if duration < minimum_seconds:
        problems.append(f'{mp4}: lasts {duration:.2f}s, under the {minimum_seconds}s floor')
    if abs(duration - expected) > 0.5:
        problems.append(f'{mp4}: lasts {duration:.2f}s but its frames cover {expected:.2f}s')
    volume = mean_volume(mp4)
    if volume <= SILENCE_DBFS:
        problems.append(f'{mp4}: the audio track is silent at {volume} dBFS')
    if not problems:
        print(f'{mp4}: {frames} frames, {duration:.2f}s, mean volume {volume} dBFS, '
              f'{film["provenance"]}')
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('films', nargs='*', type=Path, help='film.json records to check')
    parser.add_argument('--index', type=Path, help='a films.json index whose cells must all be present')
    parser.add_argument('--matrix', type=Path, help='the run directory the index describes')
    parser.add_argument('--min-seconds', type=float, default=1.0)
    parser.add_argument('--require', type=int, default=1, help='minimum number of films to check')
    arguments = parser.parse_args()
    records = list(arguments.films)
    if arguments.index:
        if not arguments.index.is_file():
            print(f'{arguments.index}: no media index; nothing rendered this panel', file=sys.stderr)
            return 1
        index = json.loads(arguments.index.read_text())
        matrix = arguments.matrix or arguments.index.parent
        records += [matrix / row['cell'] / 'film/film.json' for row in index['films']]
        for row in index['unavailable']:
            print(f'{row["cell"]}: media unavailable: {row["reason"]}', file=sys.stderr)
        if index['failed']:
            print('rendering failed for: ' + ', '.join(row['cell'] for row in index['failed']),
                  file=sys.stderr)
            return 1
    if len(records) < arguments.require:
        print(f'{len(records)} films to check, fewer than the required {arguments.require}',
              file=sys.stderr)
        return 1
    problems = [problem for record in records for problem in verify(record, arguments.min_seconds)]
    for problem in problems:
        print('::error::' + problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == '__main__':
    raise SystemExit(main())
