# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import http.client
import json
import os
import shlex
import subprocess
import tempfile
import threading
import unittest
from unittest import mock
from http.server import ThreadingHTTPServer
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
        self.assertEqual(machine.memory_max_mib, 24576)
        self.assertEqual(config.roms, Path("~/arcade-roms").expanduser())

    def test_config_requires_separate_typed_whole_memory_ceiling(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "machines.toml"
            prefix = '[machines.example]\nssh="host"\ndir="/srv"\nworkers=128\nmemory_mib=1024\n'
            for whole in (None, '"2048"', "true", "2048.0", "0", "-1", "1024", "512"):
                with self.subTest(whole=whole):
                    path.write_text(prefix + (f"memory_max_mib={whole}\n" if whole is not None else ""))
                    with self.assertRaisesRegex(ValueError, "memory_max_mib"):
                        launcher.load_config(path)
            path.write_text(prefix + "memory_max_mib=3072\n")
            machine = launcher.load_config(path).machines["example"]
            self.assertEqual((machine.memory_mib, machine.memory_max_mib), (1024, 3072))

    def test_deployment_passes_the_machine_whole_limit_without_archive_arithmetic(self) -> None:
        launch = launcher.Launcher(self.config(), Path("."))
        for workers, archive in ((1, 256), (8, 4096)):
            with self.subTest(workers=workers, archive=archive):
                run = launcher.Run.launched("20260101-000000-metroid-00000000", launcher.Settings(
                    "metroid", "example", workers, archive, 1, 1.0))
                starts = []
                def remote(machine, script, *args, stdin=None):
                    if script == launcher.START:
                        starts.append((args, json.loads(stdin)))
                        return "live view listening on 127.0.0.1:4100"
                    return ""
                with (
                    mock.patch.object(launcher, "ssh", side_effect=remote),
                    mock.patch.object(launcher.Launcher, "commit", return_value="0" * 40),
                    mock.patch.object(launcher.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, stdout=b"")),
                    mock.patch.object(launcher, "rom_path", return_value=EXAMPLE),
                    mock.patch.object(launcher.Launcher, "attach"),
                ):
                    launch.deploy(run)
                self.assertEqual(len(starts), 1)
                self.assertEqual(starts[0][0][4], "24576")
                self.assertEqual(starts[0][1]["memory_mib"], archive)
                self.assertEqual(starts[0][1]["workers"], workers)

    def test_actual_start_script_sets_whole_memory_and_disables_swap(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            commands = root / "bin"
            commands.mkdir()
            for name, body in {"systemd-run": 'printf "%s\\n" "$@" > "$ARG_LOG"',
                               "journalctl": 'echo "live view listening on 127.0.0.1:4100"'}.items():
                path = commands / name
                path.write_text("#!/bin/sh\n" + body + "\n")
                path.chmod(0o755)
            log = root / "arguments"
            subprocess.run(["bash", "-c", launcher.START, "start", str(root), "one", "{}", "unit", "24576", "commit"],
                           input=b"{}", capture_output=True, check=True, timeout=5,
                           env={**os.environ, "PATH": str(commands) + os.pathsep + os.environ["PATH"], "ARG_LOG": str(log)})
            args = log.read_text().splitlines()
            self.assertIn("MemoryMax=24576M", args)
            self.assertIn("MemorySwapMax=0", args)
            self.assertIn("Nice=10", args)
            self.assertEqual((root / "runs/one/request.json").read_bytes(), b"{}")

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
            {"stage": -1},
            {"game": "super-mario-bros", "stage": 2},
        ):
            with self.subTest(change=change), self.assertRaises(ValueError):
                launcher.parse_settings({**body, **change}, config)

    def test_a_famidash_start_level_reaches_the_request(self) -> None:
        body = {"game": "famidash-b", "machine": "example", "workers": 8, "memory_mib": 4096, "stage": 5}
        settings = launcher.parse_settings(body, self.config())
        self.assertEqual(launcher.run_request(settings, "/r.nes", "aa", "/c.so", "bb")["stage"], 5)

    def test_a_request_names_the_eval_game_and_its_own_live_port(self) -> None:
        settings = launcher.Settings("super-mario-bros", "example", 4, 2048, 7, 1.5)
        request = launcher.run_request(settings, "/r.nes", "aa", "/c.so", "bb")
        self.assertEqual(request["game"], "smb")
        self.assertEqual(request["live"], "127.0.0.1:0")
        self.assertEqual(request["wall_seconds"], 5400)
        self.assertNotIn("mixture", request)

    def test_all_games_keep_per_worker_window_and_live_routing(self) -> None:
        for game in (
            "famidash", "famidash-community", "famidash-b", "famidash-c", "famidash-d",
            "super-mario-bros", "mega-man-2", "metroid", "twin-dragons",
        ):
            for workers in (1, 128):
                with self.subTest(game=game, workers=workers):
                    settings = launcher.Settings(game, "cloud", workers, 4096, 19, 1)
                    request = launcher.run_request(settings, "/r.nes", "aa", "/c.so", "bb")
                    self.assertEqual(request["game"], launcher.GAMES[game].request)
                    self.assertEqual(request["live"], "127.0.0.1:0")
                    self.assertEqual(request["workers"], workers)
                    self.assertEqual(request["window"], 4)
                    self.assertNotIn("algorithm", request)

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

    def test_found_runs_come_from_live_records(self) -> None:
        machine = self.config().machines["example"]
        record = '{"address":"127.0.0.1:4100","game":"metroid","pid":42,"process_start":7}'
        listing = "\n".join(
            [
                f"/srv/runs/a/campaign/live.json\t1700000000\t{record}",
                '/srv/runs/b/live.json\t1700000000\t{"address":"127.0.0.1:1","game":"stb"}',
                "/srv/runs/c/live.json\t1700000000\tnot json",
                "/srv/runs/d/live.json\t1700000000\t[42]",
                '/srv/runs/e/live.json\t1700000000\t{"address":["x"],"game":"metroid","pid":4}',
                '/srv/runs/f/live.json\t1700000000\t{"address":"10.0.0.1:80","game":"metroid","pid":4}',
                '/srv/runs/g/live.json\t1700000000\t{"address":"127.0.0.1:1","game":["metroid"]}',
                f"/srv/runs/h/live.json\tsoon\t{record}",
            ]
        )
        (run,) = launcher.external_runs(machine, listing)
        self.assertEqual((run.game, run.machine), ("metroid", "example"))
        self.assertEqual(run.directory, "/srv/runs/a/campaign")
        self.assertEqual(run.started, 1700000000.0)
        self.assertRegex(run.id, r"^found-[0-9a-f]{10}$")
        self.assertEqual(launcher.external_runs(machine, listing)[0].id, run.id)
        other = launcher.Machine("other", "other", "/srv", 1, 1024, 2048, ("/srv/runs",))
        self.assertNotEqual(launcher.external_runs(other, listing)[0].id, run.id)
        described = run.describe()
        self.assertTrue(described["external"])
        self.assertNotIn("workers", described)
        with self.assertRaises(ValueError):
            launcher.Launcher(self.config(), Path(".")).stop(run)

    def test_runs_started_in_the_same_second_get_their_own_ids(self) -> None:
        launch = launcher.Launcher(self.config(), Path("."))
        settings = launcher.Settings("metroid", "example", 4, 2048, 1, 1.0)
        with mock.patch.object(launcher.Launcher, "deploy"):
            first, second = launch.start(settings), launch.start(settings)
        self.assertNotEqual(first.id, second.id)
        self.assertEqual(set(launch.runs), {first.id, second.id})
        for run in (first, second):
            self.assertRegex(run.id, launcher.RUN_ID)

    def local_config(self, directory: str) -> launcher.Config:
        path = Path(directory) / "machines.toml"
        path.write_text(f'[machines.mac]\nlocal=true\ndir="{directory}/arcade"\nworkers=4\nmemory_mib=2048\n')
        return launcher.load_config(path)

    def test_a_local_machine_needs_no_ssh_or_whole_memory_ceiling(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            machine = self.local_config(directory).machines["mac"]
        self.assertTrue(machine.local)
        self.assertEqual((machine.ssh, machine.runs, machine.memory_max_mib), ("", (), 2048))

    def test_a_local_run_starts_a_child_process_and_stops_it(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = self.local_config(directory)
            target = Path(directory) / "arcade/target/release"
            target.mkdir(parents=True)
            fake = target / "nes-eval"
            fake.write_text("#!/bin/sh\necho 'live view listening on 127.0.0.1:4321'\nexec sleep 60\n")
            fake.chmod(0o755)
            core = Path(directory) / "arcade/quicknes_libretro.dylib"
            core.write_bytes(b"core")
            launch = launcher.Launcher(config, Path("."))
            run = launcher.Run.launched("20260101-000000-metroid-00000000", launcher.Settings(
                "metroid", "mac", 2, 1024, 1, 1.0))
            with (
                mock.patch.object(launcher, "rom_path", return_value=EXAMPLE),
                mock.patch.object(launcher, "build_logged") as build,
                mock.patch.object(launcher.sys, "platform", "darwin"),
            ):
                launch.deploy(run)
            self.assertEqual((run.state, run.local_port), ("running", 4321))
            self.assertEqual(build.call_count, 1)
            request = json.loads((Path(directory) / "arcade/runs" / run.id / "request.json").read_text())
            self.assertEqual(request["live"], "127.0.0.1:0")
            self.assertEqual(request["core"], str(core.resolve()))
            assert run.process is not None
            self.assertIsNone(run.process.poll())
            launch.stop(run)
            self.assertEqual(run.state, "stopped")
            self.assertIsNotNone(run.process.poll())

    def test_a_local_run_resolves_a_relative_rom_before_changing_directory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = self.local_config(directory)
            target = Path(directory) / "arcade/target/release"
            target.mkdir(parents=True)
            fake = target / "nes-eval"
            fake.write_text("#!/bin/sh\necho 'live view listening on 127.0.0.1:4321'\nexec sleep 60\n")
            fake.chmod(0o755)
            (Path(directory) / "arcade/quicknes_libretro.dylib").write_bytes(b"core")
            launch = launcher.Launcher(config, Path("."))
            run = launcher.Run.launched("20260101-000000-metroid-00000000", launcher.Settings(
                "metroid", "mac", 2, 1024, 1, 1.0))
            with (
                mock.patch.object(launcher, "rom_path", return_value=Path(os.path.relpath(EXAMPLE))),
                mock.patch.object(launcher, "build_logged"),
                mock.patch.object(launcher.sys, "platform", "darwin"),
            ):
                launch.deploy(run)
            request = json.loads((Path(directory) / "arcade/runs" / run.id / "request.json").read_text())
            self.assertTrue(Path(request["rom"]).is_absolute())
            launch.stop(run)

    def test_a_stop_before_the_child_is_recorded_still_ends_the_child(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = self.local_config(directory)
            target = Path(directory) / "arcade/target/release"
            target.mkdir(parents=True)
            fake = target / "nes-eval"
            fake.write_text("#!/bin/sh\nexec sleep 60\n")
            fake.chmod(0o755)
            (Path(directory) / "arcade/quicknes_libretro.dylib").write_bytes(b"core")
            launch = launcher.Launcher(config, Path("."))
            run = launcher.Run.launched("20260101-000000-metroid-00000000", launcher.Settings(
                "metroid", "mac", 2, 1024, 1, 1.0))
            real = subprocess.Popen
            children: list[subprocess.Popen[bytes]] = []

            def spawn(*args, **kwargs):
                run.cancelled = True
                children.append(real(*args, **kwargs))
                return children[-1]

            with (
                mock.patch.object(launcher, "rom_path", return_value=EXAMPLE),
                mock.patch.object(launcher, "build_logged"),
                mock.patch.object(launcher.sys, "platform", "darwin"),
                mock.patch.object(launcher.subprocess, "Popen", spawn),
            ):
                launch.deploy(run)
            self.assertIsNotNone(children[0].poll())

    def test_a_local_run_that_exits_before_listening_fails_with_its_log(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = self.local_config(directory)
            target = Path(directory) / "arcade/target/release"
            target.mkdir(parents=True)
            fake = target / "nes-eval"
            fake.write_text("#!/bin/sh\necho 'bad request'\nexit 2\n")
            fake.chmod(0o755)
            (Path(directory) / "arcade/quicknes_libretro.dylib").write_bytes(b"core")
            launch = launcher.Launcher(config, Path("."))
            run = launcher.Run.launched("20260101-000000-metroid-00000000", launcher.Settings(
                "metroid", "mac", 2, 1024, 1, 1.0))
            with (
                mock.patch.object(launcher, "rom_path", return_value=EXAMPLE),
                mock.patch.object(launcher, "build_logged"),
                mock.patch.object(launcher.sys, "platform", "darwin"),
            ):
                launch.deploy(run)
        self.assertEqual(run.state, "failed")
        self.assertIn("bad request", run.message)

    def test_a_run_stopped_while_copying_never_starts(self) -> None:
        launch = launcher.Launcher(self.config(), Path("."))
        run = launcher.Run.launched("20260101-000000-metroid-00000000", launcher.Settings(
            "metroid", "example", 4, 2048, 1, 1.0))
        scripts = []

        def remote(machine, script, *args, stdin=None):
            scripts.append(script)
            if script == launcher.UNPACK:
                launch.stop(run)
            return ""

        archive = subprocess.CompletedProcess([], 0, stdout=b"")
        with (
            mock.patch.object(launcher, "ssh", side_effect=remote),
            mock.patch.object(launcher.Launcher, "commit", return_value="0" * 40),
            mock.patch.object(launcher.subprocess, "run", return_value=archive),
        ):
            launch.deploy(run)
        self.assertNotIn(launcher.START, scripts)
        self.assertEqual(run.state, "stopped")

    def test_a_run_stopped_during_its_start_is_stopped_again(self) -> None:
        launch = launcher.Launcher(self.config(), Path("."))
        run = launcher.Run.launched("20260101-000000-metroid-00000000", launcher.Settings(
            "metroid", "example", 4, 2048, 1, 1.0))
        scripts = []

        def remote(machine, script, *args, stdin=None):
            scripts.append(script)
            if script == launcher.START:
                launch.stop(run)
                return "live view listening on 127.0.0.1:4100"
            return ""

        archive = subprocess.CompletedProcess([], 0, stdout=b"")
        with (
            mock.patch.object(launcher, "ssh", side_effect=remote),
            mock.patch.object(launcher.Launcher, "commit", return_value="0" * 40),
            mock.patch.object(launcher.subprocess, "run", return_value=archive),
            mock.patch.object(launcher, "rom_path", return_value=EXAMPLE),
            mock.patch.object(launcher.Launcher, "attach") as attach,
        ):
            launch.deploy(run)
        attach.assert_not_called()
        self.assertEqual(scripts[-2:], [launcher.STOP, launcher.STOP])
        self.assertEqual(run.state, "stopped")

    def test_a_failed_start_stops_its_unit(self) -> None:
        launch = launcher.Launcher(self.config(), Path("."))
        run = launcher.Run.launched("20260101-000000-metroid-00000000", launcher.Settings(
            "metroid", "example", 4, 2048, 1, 1.0))
        scripts = []

        def remote(machine, script, *args, stdin=None):
            scripts.append(script)
            if script == launcher.START:
                launch.stop(run)
                raise RuntimeError("no live port")
            return ""

        archive = subprocess.CompletedProcess([], 0, stdout=b"")
        with (
            mock.patch.object(launcher, "ssh", side_effect=remote),
            mock.patch.object(launcher.Launcher, "commit", return_value="0" * 40),
            mock.patch.object(launcher.subprocess, "run", return_value=archive),
            mock.patch.object(launcher, "rom_path", return_value=EXAMPLE),
        ):
            launch.deploy(run)
        self.assertEqual(scripts[-3:], [launcher.START, launcher.STOP, launcher.STOP])
        self.assertEqual(run.state, "stopped")

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

    def test_the_server_marks_its_pages_and_refuses_other_hosts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            site = Path(directory)
            (site / "index.html").write_text("page")
            server = ThreadingHTTPServer(("127.0.0.1", 0), launcher.Handler)
            port = server.server_address[1]
            launcher.Handler.launcher = launcher.Launcher(self.config(), site)
            launcher.Handler.port = port
            threading.Thread(target=server.serve_forever, daemon=True).start()
            try:

                def get(method: str, path: str, host: str) -> http.client.HTTPResponse:
                    connection = http.client.HTTPConnection("127.0.0.1", port)
                    connection.request(method, path, headers={"Host": host})
                    return connection.getresponse()

                page = get("HEAD", "/?game=famidash", f"127.0.0.1:{port}")
                self.assertEqual(page.status, 200)
                self.assertEqual(page.getheader(launcher.LAUNCHER_HEADER), "1")
                machines = get("GET", "/launcher/machines", f"localhost:{port}")
                self.assertEqual(machines.status, 200)
                machine_options = json.loads(machines.read())
                self.assertEqual(machine_options["machines"][0]["memory_mib"], 16384)
                self.assertEqual(machine_options["machines"][0]["memory_max_mib"], 24576)
                self.assertEqual(get("GET", "/", f"evil.example:{port}").status, 403)
                self.assertEqual(
                    get("GET", "/runs/none/live/events", f"127.0.0.1:{port}").status, 404
                )
            finally:
                server.shutdown()
                server.server_close()


if __name__ == "__main__":
    unittest.main()
