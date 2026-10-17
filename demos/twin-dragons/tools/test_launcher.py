# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path

import launcher

EXAMPLE = Path(__file__).with_name("arcade-machines.example.toml")


class LauncherTest(unittest.TestCase):
    def config(self) -> launcher.Config:
        return launcher.load_config(EXAMPLE)

    def test_the_example_config_loads(self) -> None:
        config = self.config()
        machine = config.machines["example"]
        self.assertEqual(machine.workers, 8)
        self.assertEqual(machine.memory_mib, 16384)
        self.assertEqual(config.roms, Path("~/arcade-roms").expanduser())

    def test_settings_stay_within_the_machine_limits(self) -> None:
        config = self.config()
        body = {"game": "famidash-b", "machine": "example", "workers": 8, "memory_mib": 4096}
        settings = launcher.parse_settings(body, config)
        self.assertEqual((settings.seed, settings.hours), (1, 12.0))
        for change in (
            {"workers": 9},
            {"workers": 0},
            {"memory_mib": 16385},
            {"game": "tobu-tobu-girl"},
            {"machine": "other"},
            {"hours": 0},
        ):
            with self.subTest(change=change), self.assertRaises(ValueError):
                launcher.parse_settings({**body, **change}, config)

    def test_a_request_names_the_eval_game_and_its_own_live_port(self) -> None:
        settings = launcher.Settings("super-mario-bros", "example", 4, 2048, 7, 1.5)
        request = launcher.run_request(settings, "/r.nes", "aa", "/c.so", "bb")
        self.assertEqual(request["game"], "smb")
        self.assertEqual(request["live"], "127.0.0.1:0")
        self.assertEqual(request["wall_seconds"], 5400)
        self.assertNotIn("mixture", request)

    def test_roms_come_from_the_site_or_the_rom_directory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "site/famidash").mkdir(parents=True)
            (root / "site/famidash/famidash.nes").write_bytes(b"x")
            (root / "roms").mkdir()
            (root / "roms/metroid.nes").write_bytes(b"x")
            config = launcher.Config({}, root / "roms")
            games = launcher.available_games(root / "site", config)
            self.assertEqual(games, ["famidash", "famidash-community", "metroid"])

    def test_site_files_stay_inside_the_site(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "index.html").write_text("page")
            (root / "assets").mkdir()
            (root / "assets/a.js").write_text("js")
            self.assertEqual(
                launcher.site_file(root, "/?game=famidash&live"), (root / "index.html").resolve()
            )
            self.assertEqual(
                launcher.site_file(root, "/assets/a.js?v=1"), (root / "assets/a.js").resolve()
            )
            self.assertIsNone(launcher.site_file(root, "/../index.html"))
            self.assertIsNone(launcher.site_file(root, "/%2e%2e/etc/passwd"))
            self.assertIsNone(launcher.site_file(root, "/missing.js"))

    def test_only_this_origin_reaches_the_launcher(self) -> None:
        self.assertTrue(launcher.local_request("127.0.0.1:8790", None, 8790))
        self.assertTrue(launcher.local_request("localhost:8790", "http://localhost:8790", 8790))
        self.assertFalse(launcher.local_request("evil.example:8790", None, 8790))
        self.assertFalse(launcher.local_request("127.0.0.1:8790", "https://evil.example", 8790))
        self.assertFalse(launcher.local_request(None, None, 8790))

    def test_remote_arguments_reach_the_script_unchanged(self) -> None:
        args = ["/srv/a b", '{"game": "famidash"}', "$HOME;rm -rf /"]
        command = launcher.remote_command('printf "%s\\n" "$@"', *args)
        shell = " ".join(command)
        output = subprocess.run(
            ["sh", "-c", shell], capture_output=True, text=True, check=True
        ).stdout
        self.assertEqual(output.splitlines(), args)
        self.assertEqual(shlex.split(shell)[:2], ["bash", "-c"])


if __name__ == "__main__":
    unittest.main()
