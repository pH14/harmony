# SPDX-License-Identifier: AGPL-3.0-or-later
"""Media contract checks; ffmpeg synthesizes the films, no ROM or emulator required."""
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().with_name('verify-nes-films.py')
SPEC = importlib.util.spec_from_file_location('verify_nes_films', SCRIPT)
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)

FRAMES = 120


def synthesize(path, frames=FRAMES, audio='sine=frequency=440:sample_rate=44100', with_audio=True):
    seconds = frames / 60
    command = ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y',
               '-f', 'lavfi', '-i', f'testsrc=size=256x224:rate=60:duration={seconds}']
    if with_audio:
        command += ['-f', 'lavfi', '-i', f'{audio}:duration={seconds}', '-c:a', 'aac', '-ac', '2']
    command += ['-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-frames:v', str(frames), str(path)]
    subprocess.run(command, check=True, capture_output=True)


@unittest.skipUnless(shutil.which('ffmpeg') and shutil.which('ffprobe'), 'ffmpeg is required')
class FilmVerificationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def film(self, **overrides):
        directory = self.root / overrides.pop('name', 'film') / 'film'
        directory.mkdir(parents=True)
        mp4 = directory / 'witness.mp4'
        synthesize(mp4, frames=overrides.pop('rendered', FRAMES),
                   audio=overrides.pop('audio', 'sine=frequency=440:sample_rate=44100'),
                   with_audio=overrides.pop('with_audio', True))
        record = {'format': 'nes-film-v1', 'endpoint_verified': True,
                  'provenance': 'native QuickNES replay of the input this run recorded',
                  'video': {'frames': FRAMES},
                  'capture': {'mp4': str(mp4), 'mp4_sha256': hashlib.sha256(mp4.read_bytes()).hexdigest()}}
        for key, value in overrides.items():
            if isinstance(value, dict): record[key].update(value)
            else: record[key] = value
        path = directory / 'film.json'
        path.write_text(json.dumps(record))
        return path

    def test_a_film_with_frames_duration_and_real_audio_passes(self):
        self.assertEqual(VERIFY.verify(self.film(), 1.0), [])

    def test_a_silent_track_is_not_game_audio(self):
        problems = VERIFY.verify(self.film(audio='anullsrc=r=44100:cl=stereo'), 1.0)
        self.assertTrue(any('silent' in problem for problem in problems), problems)

    def test_a_film_without_an_audio_stream_fails(self):
        problems = VERIFY.verify(self.film(with_audio=False), 1.0)
        self.assertTrue(any('no audio stream' in problem for problem in problems), problems)

    def test_a_truncated_film_fails_its_recorded_frame_count(self):
        problems = VERIFY.verify(self.film(rendered=FRAMES // 2), 1.0)
        self.assertTrue(any('video frames' in problem for problem in problems), problems)
        self.assertTrue(any('frames cover' in problem for problem in problems), problems)

    def test_altered_media_fails_its_recorded_digest(self):
        path = self.film()
        record = json.loads(path.read_text())
        record['capture']['mp4_sha256'] = 'f' * 64
        path.write_text(json.dumps(record))
        self.assertTrue(any('SHA-256' in problem for problem in VERIFY.verify(path, 1.0)))

    def test_missing_media_is_reported_before_probing(self):
        path = self.film()
        (path.parent / 'witness.mp4').unlink()
        self.assertEqual(len(VERIFY.verify(path, 1.0)), 1)
        self.assertIn('missing or empty', VERIFY.verify(path, 1.0)[0])

    def test_a_capture_that_skipped_its_endpoint_check_fails(self):
        self.assertTrue(any('endpoint' in problem
                            for problem in VERIFY.verify(self.film(endpoint_verified=False), 1.0)))

    def test_a_film_under_the_duration_floor_fails(self):
        self.assertTrue(any('floor' in problem for problem in VERIFY.verify(self.film(), 60.0)))

    def index(self, films, unavailable=(), failed=()):
        path = self.root / 'films.json'
        path.write_text(json.dumps({'films': films, 'unavailable': list(unavailable),
                                    'failed': list(failed)}))
        return path

    def run_main(self, *arguments):
        return subprocess.run(['python3', str(SCRIPT), *map(str, arguments)],
                              capture_output=True, text=True)

    def test_an_index_with_a_failed_render_fails_the_command(self):
        index = self.index([], failed=[{'cell': 'nova-s1', 'exit_code': 3}])
        result = self.run_main('--index', index, '--matrix', self.root, '--require', '0')
        self.assertEqual(result.returncode, 1)
        self.assertIn('rendering failed for: nova-s1', result.stderr)

    def test_an_index_reports_unavailable_media_and_still_checks_the_rest(self):
        self.film(name='nova-s1')
        index = self.index([{'cell': 'nova-s1'}],
                           unavailable=[{'cell': 'stb-s1', 'reason': 'the search produced no renderable input'}])
        result = self.run_main('--index', index, '--matrix', self.root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('media unavailable', result.stderr)
        self.assertIn('witness.mp4', result.stdout)

    def test_fewer_films_than_required_fails(self):
        result = self.run_main('--index', self.index([]), '--matrix', self.root, '--require', '1')
        self.assertEqual(result.returncode, 1)
        self.assertIn('fewer than the required', result.stderr)


if __name__ == '__main__':
    unittest.main()
