#!/usr/bin/env -S uv run --script
# SPDX-License-Identifier: AGPL-3.0-or-later
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Start native arcade campaigns on SSH machines or this computer and watch them in the arcade page.

The launcher serves a built arcade site on a loopback port. The page offers a
Native run dialog when it finds the launcher. A run copies this checkout's
committed source to a machine, builds the QuickNES core and `nes-eval` there
once per commit, starts `nes-eval` with a `live` request under `systemd-run`
with a memory cap, and forwards the live port back over SSH. The page watches a
run at `/runs/<id>/?game=<game>&live`, which serves the same site and proxies
`/runs/<id>/live/` to that run.

A machine with `local = true` runs `nes-eval` as a child process of the
launcher instead. It builds the QuickNES core and `nes-eval` from this working
tree and needs no SSH or systemd. It does not enforce a memory ceiling.

Every `nes-eval` campaign writes `live.json` to its output directory while it
runs. The launcher also lists the campaigns it finds under each machine's
`runs` directories, so any run can be watched the same way.

Machines and the ROM directory come from a TOML file outside the repository,
by default `~/.config/harmony/arcade-machines.toml`. See
`arcade-machines.example.toml`.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import mimetypes
import os
import re
import secrets
import shlex
import socket
import subprocess
import sys
import threading
import time
import tomllib
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import unquote, urlsplit

REPOSITORY = Path(__file__).resolve().parents[3]
DEFAULT_CONFIG = Path("~/.config/harmony/arcade-machines.toml")
UNIT_PREFIX = "harmony-arcade-"
LISTEN_PATTERN = re.compile(r"live view listening on 127\.0\.0\.1:(\d+)")
RUN_ID = re.compile(r"^[0-9]{8}-[0-9]{6}-[a-z0-9-]+$")
STATUS_SECONDS = 10.0
SCAN_SECONDS = 30.0
LAUNCHER_HEADER = "X-Harmony-Launcher"


@dataclass(frozen=True)
class Game:
    request: str
    rom: str
    from_site: bool


GAMES: dict[str, Game] = {
    "twin-dragons": Game("twin-dragons", "twin-dragons.nes", True),
    "famidash": Game("famidash", "famidash/famidash.nes", True),
    "famidash-community": Game("famidash-community", "famidash/famidash.nes", True),
    "famidash-b": Game("famidash-b", "famidash/famidash-b-sides.nes", True),
    "famidash-c": Game("famidash-c", "famidash/famidash-c-sides.nes", True),
    "famidash-d": Game("famidash-d", "famidash/famidash-d-sides.nes", True),
    "super-mario-bros": Game("smb", "super-mario-bros.nes", False),
    "mega-man-2": Game("mm2", "mega-man-2.nes", False),
    "metroid": Game("metroid", "metroid.nes", False),
    "punch-out": Game("punchout", "punch-out.nes", False),
}


@dataclass(frozen=True)
class Machine:
    name: str
    ssh: str
    dir: str
    workers: int
    memory_mib: int
    memory_max_mib: int
    runs: tuple[str, ...] = ()
    local: bool = False


@dataclass(frozen=True)
class Config:
    machines: dict[str, Machine]
    roms: Path | None


@dataclass(frozen=True)
class Settings:
    game: str
    machine: str
    workers: int
    memory_mib: int
    seed: int
    hours: float
    stage: int | None = None


@dataclass
class Run:
    id: str
    game: str
    machine: str
    settings: Settings | None = None
    directory: str = ""
    state: str = "starting"
    message: str = ""
    started: float = field(default_factory=time.time)
    local_port: int | None = None
    tunnel: subprocess.Popen[bytes] | None = None
    process: subprocess.Popen[bytes] | None = None
    cancelled: bool = False

    @classmethod
    def launched(cls, run_id: str, settings: Settings, **fields: Any) -> Run:
        return cls(run_id, settings.game, settings.machine, settings, **fields)

    def describe(self) -> dict[str, Any]:
        stage = self.settings.stage if self.settings else None
        level = "" if stage is None else f"&level={stage}"
        return {
            "id": self.id,
            "game": self.game,
            "machine": self.machine,
            **(self.settings.__dict__ if self.settings else {"directory": self.directory}),
            "external": self.settings is None,
            "state": self.state,
            "message": self.message,
            "started": self.started,
            "watch": f"/runs/{self.id}/?game={self.game}&live{level}" if self.local_port else None,
        }


def load_config(path: Path) -> Config:
    data = tomllib.loads(path.expanduser().read_text())
    machines: dict[str, Machine] = {}
    for name, entry in data.get("machines", {}).items():
        archive = entry.get("memory_mib")
        local = bool(entry.get("local", False))
        whole = entry.get("memory_max_mib", archive if local else None)
        if type(archive) is not int or archive <= 0:
            raise ValueError(f"{name} memory_mib must be a positive integer archive ceiling")
        if type(whole) is not int or (whole <= archive and not local):
            raise ValueError(f"{name} memory_max_mib must be an integer whole-unit ceiling greater than memory_mib")
        machines[name] = Machine(
            name=name,
            ssh="" if local else str(entry["ssh"]),
            dir=str(entry["dir"]),
            workers=int(entry["workers"]),
            memory_mib=archive,
            memory_max_mib=whole,
            runs=() if local else tuple(str(directory) for directory in entry.get("runs", [])),
            local=local,
        )
    if not machines:
        raise ValueError(f"{path} lists no [machines.NAME] tables")
    roms = data.get("roms")
    return Config(machines, Path(str(roms)).expanduser() if roms else None)


def parse_settings(body: dict[str, Any], config: Config) -> Settings:
    game, machine = str(body.get("game", "")), str(body.get("machine", ""))
    if game not in GAMES:
        raise ValueError(f"{game} has no native live view")
    if machine not in config.machines:
        raise ValueError(f"{machine} is not a configured machine")
    limits = config.machines[machine]
    workers, memory_mib = int(body["workers"]), int(body["memory_mib"])
    seed, hours = int(body.get("seed", 1)), float(body.get("hours", 12))
    if not 1 <= workers <= limits.workers:
        raise ValueError(f"{machine} runs 1 to {limits.workers} workers")
    if not 256 <= memory_mib <= limits.memory_mib:
        raise ValueError(f"{machine} allows 256 to {limits.memory_mib} MiB")
    if seed < 0 or not 0 < hours <= 72:
        raise ValueError("the seed must be at least 0 and the time limit 72 hours or less")
    stage = body.get("stage")
    if stage is not None and not (game.startswith("famidash") and isinstance(stage, int) and stage >= 0):
        raise ValueError("only Famidash runs take a starting level")
    return Settings(game, machine, workers, memory_mib, seed, hours, stage)


def rom_path(game: Game, site: Path, config: Config) -> Path | None:
    base = site if game.from_site else config.roms
    if base is None:
        return None
    path = base / game.rom
    return path if path.is_file() else None


def available_games(site: Path, config: Config) -> list[str]:
    return [name for name, game in GAMES.items() if rom_path(game, site, config)]


def run_request(
    settings: Settings, rom: str, rom_sha256: str, core: str, core_sha256: str
) -> dict[str, Any]:
    return {
        "game": GAMES[settings.game].request,
        "rom": rom,
        "core": core,
        "rom_sha256": rom_sha256,
        "core_sha256": core_sha256,
        "seed": settings.seed,
        "workers": settings.workers,
        "executions": 1 << 40,
        "memory_mib": settings.memory_mib,
        "window": 4,
        "wall_seconds": round(settings.hours * 3600),
        "verification": "witness",
        "live": "127.0.0.1:0",
        **({} if settings.stage is None else {"stage": settings.stage}),
    }


def site_file(root: Path, path: str) -> Path | None:
    relative = unquote(urlsplit(path).path).lstrip("/") or "index.html"
    if relative.endswith("/"):
        relative += "index.html"
    candidate = (root / relative).resolve()
    if not candidate.is_relative_to(root.resolve()) or not candidate.is_file():
        return None
    return candidate


def local_request(host: str | None, origin: str | None, port: int) -> bool:
    allowed = {f"127.0.0.1:{port}", f"localhost:{port}"}
    if host not in allowed:
        return False
    return origin is None or origin.removeprefix("http://") in allowed


def remote_command(script: str, *args: str) -> list[str]:
    return ["bash", "-c", shlex.quote(script), "bash", *map(shlex.quote, args)]


def ssh(machine: Machine, script: str, *args: str, stdin: bytes | None = None) -> str:
    result = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", machine.ssh, *remote_command(script, *args)],
        input=stdin,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        tail = result.stderr.decode(errors="replace").strip().splitlines()[-5:]
        raise RuntimeError(f"{machine.name}: " + " / ".join(tail))
    return result.stdout.decode()


SYSTEMD = 'if [ "$(id -u)" = 0 ]; then user=; else user=--user; fi\n'

UNPACK = """set -euo pipefail
src=$1/src/$2
if [ ! -f "$src/.unpacked" ]; then
  rm -rf "$src" && mkdir -p "$src" && tar -x -m -C "$src" && touch "$src/.unpacked"
else
  cat >/dev/null
fi
"""

BUILD = """set -euo pipefail
export PATH=$HOME/.cargo/bin:$PATH
src=$1/src/$2
exec 9>"$1/build.lock"
flock 9
if [ ! -f "$src/.built" ]; then
  bash "$src/scripts/build-quicknes-core.sh" "$src/quicknes_libretro.so" >"$src/core.log" 2>&1
  CARGO_TARGET_DIR=$1/target cargo build --release --locked --bin nes-eval \\
    --manifest-path "$src/workloads/nes/Cargo.toml" >"$src/build.log" 2>&1
  cp "$1/target/release/nes-eval" "$src/nes-eval"
  touch "$src/.built"
fi
sha256sum "$src/quicknes_libretro.so" | cut -d ' ' -f 1
"""

UPLOAD = """set -euo pipefail
mkdir -p "$(dirname "$1")"
if [ -f "$1" ]; then cat >/dev/null; else cat >"$1.part" && mv "$1.part" "$1"; fi
"""

START = (
    """set -euo pipefail
"""
    + SYSTEMD
    + """run=$1/runs/$2
mkdir -p "$run"
cat >"$run/request.json"
printf '%s\\n' "$3" >"$run/launch.json"
systemd-run $user --unit "$4" --collect -p MemoryMax="$5"M -p MemorySwapMax=0 -p Nice=10 \\
  -p WorkingDirectory="$run" "$1/src/$6/nes-eval" "$run/request.json" "$run/out" >/dev/null
for _ in $(seq 1 120); do
  line=$(journalctl $user -u "$4" -o cat --no-pager 2>/dev/null | grep -m1 'live view listening' || true)
  if [ -n "$line" ]; then printf '%s\\n' "$line"; exit 0; fi
  if ! systemctl $user is-active --quiet "$4"; then
    journalctl $user -u "$4" -o cat --no-pager | tail -5 >&2
    exit 1
  fi
  sleep 0.5
done
echo "the run did not open its live port" >&2
exit 1
"""
)

STATUS = (
    SYSTEMD
    + """for unit in "$@"; do printf '%s %s\\n' "$unit" "$(systemctl $user is-active "$unit" || true)"; done
"""
)

STOP = SYSTEMD + 'systemctl $user stop "$1" || true\n'

DISCOVER = (
    SYSTEMD + """for unit in $(systemctl $user list-units --plain --no-legend 'harmony-arcade-*' \\
    | cut -d ' ' -f 1); do
  id=${unit#harmony-arcade-}; id=${id%.service}
  line=$(journalctl $user -u "$unit" -o cat --no-pager | grep -m1 'live view listening' || true)
  printf '%s\\t%s\\t%s\\n' "$id" "$line" "$(cat "$1/runs/$id/launch.json" 2>/dev/null)"
done
"""
)

SCAN = """own=$1/runs/; shift
for dir in "$@"; do
  [ -d "$dir" ] && find "$dir" -maxdepth 5 -name live.json 2>/dev/null
done | while read -r file; do
  case $file in "$own"*) continue ;; esac
  pid=$(grep -o '"pid":[0-9]*' "$file" | cut -d : -f 2)
  start=$(grep -o '"process_start":[0-9]*' "$file" | cut -d : -f 2)
  case $(cat "/proc/${pid:-0}/comm" 2>/dev/null) in nes-eval*) ;; *) continue ;; esac
  [ -n "$start" ] && [ "$(sed 's/.*) //' "/proc/$pid/stat" | cut -d ' ' -f 20)" = "$start" ] || continue
  printf '%s\\t%s\\t%s\\n' "$file" "$(stat -c %Y "$file")" "$(cat "$file")"
done
"""


def external_runs(machine: Machine, listing: str) -> list[Run]:
    runs = []
    for row in listing.splitlines():
        path, modified, record = (row.split("\t") + ["", ""])[:3]
        try:
            live = json.loads(record)
            started = float(modified)
        except ValueError:
            continue
        if not isinstance(live, dict):
            continue
        game, address, pid = live.get("game"), live.get("address"), live.get("pid")
        if not isinstance(game, str) or not isinstance(address, str) or not isinstance(pid, int):
            continue
        if game not in GAMES or not re.fullmatch(r"127\.0\.0\.1:\d{1,5}", address):
            continue
        key = f"{machine.name}:{path}:{pid}:{live.get('process_start')}"
        digest = hashlib.sha256(key.encode()).hexdigest()[:10]
        directory = str(Path(path).parent)
        run = Run(f"found-{digest}", game, machine.name, directory=directory)
        run.started = started
        run.message = f"live view listening on {address}"
        runs.append(run)
    return runs


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


class Cancelled(Exception):
    pass


def build_logged(command: list[str], log: Path, environment: dict[str, str] | None = None) -> None:
    with log.open("wb") as output:
        result = subprocess.run(
            command,
            stdin=subprocess.DEVNULL,
            stdout=output,
            stderr=subprocess.STDOUT,
            env={**os.environ, **(environment or {})},
            check=False,
        )
    if result.returncode != 0:
        tail = log.read_text(errors="replace").strip().splitlines()[-5:]
        raise RuntimeError(f"{command[0]} failed: " + " / ".join(tail))


class Launcher:
    def __init__(self, config: Config, site: Path) -> None:
        self.config = config
        self.site = site
        self.runs: dict[str, Run] = {}
        self.lock = threading.Lock()

    def commit(self) -> str:
        return subprocess.run(
            ["git", "-C", str(REPOSITORY), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()

    def start(self, settings: Settings) -> Run:
        with self.lock:
            while True:
                run_id = f"{time.strftime('%Y%m%d-%H%M%S')}-{settings.game}-{secrets.token_hex(4)}"
                if run_id not in self.runs:
                    break
            run = Run.launched(run_id, settings)
            self.runs[run.id] = run
        threading.Thread(target=self.deploy, args=(run,), daemon=True).start()
        return run

    def deploy(self, run: Run) -> None:
        settings = run.settings
        assert settings is not None
        machine = self.config.machines[settings.machine]
        if machine.local:
            return self.deploy_local(run, machine, settings)
        try:
            self.check_cancelled(run)
            commit = self.commit()
            run.message = f"Copying {commit[:9]} to {machine.name}"
            archive = subprocess.run(
                ["git", "-C", str(REPOSITORY), "archive", "--format=tar", commit],
                capture_output=True,
                check=True,
            ).stdout
            ssh(machine, UNPACK, machine.dir, commit, stdin=archive)
            self.check_cancelled(run)
            run.message = f"Building {commit[:9]} on {machine.name}"
            core_sha256 = ssh(machine, BUILD, machine.dir, commit).strip()
            self.check_cancelled(run)
            rom = rom_path(GAMES[settings.game], self.site, self.config)
            if rom is None:
                raise RuntimeError(f"no ROM for {settings.game}")
            data = rom.read_bytes()
            rom_sha256 = hashlib.sha256(data).hexdigest()
            remote_rom = f"{machine.dir}/roms/{rom_sha256}.nes"
            ssh(machine, UPLOAD, remote_rom, stdin=data)
            request = run_request(
                settings,
                remote_rom,
                rom_sha256,
                f"{machine.dir}/src/{commit}/quicknes_libretro.so",
                core_sha256,
            )
            launch = json.dumps(settings.__dict__)
            self.check_cancelled(run)
            run.message = f"Starting on {machine.name}"
            try:
                line = ssh(
                    machine,
                    START,
                    machine.dir,
                    run.id,
                    launch,
                    UNIT_PREFIX + run.id,
                    str(machine.memory_max_mib),
                    commit,
                    stdin=json.dumps(request).encode(),
                )
            except (RuntimeError, OSError, subprocess.CalledProcessError, ValueError):
                ssh(machine, STOP, UNIT_PREFIX + run.id)
                raise
            if self.cancelled(run):
                ssh(machine, STOP, UNIT_PREFIX + run.id)
                return
            self.attach(run, machine, line)
        except Cancelled:
            return
        except (RuntimeError, OSError, subprocess.CalledProcessError, ValueError) as error:
            with self.lock:
                if not run.cancelled:
                    run.state, run.message = "failed", str(error)

    def deploy_local(self, run: Run, machine: Machine, settings: Settings) -> None:
        try:
            root = Path(machine.dir).expanduser()
            directory = root / "runs" / run.id
            directory.mkdir(parents=True)
            rom = rom_path(GAMES[settings.game], self.site, self.config)
            if rom is None:
                raise RuntimeError(f"no ROM for {settings.game}")
            core = root / ("quicknes_libretro.dylib" if sys.platform == "darwin" else "quicknes_libretro.so")
            run.message = "Building the QuickNES core and nes-eval"
            if not core.is_file():
                build_logged(["bash", str(REPOSITORY / "scripts/build-quicknes-core.sh"), str(core)], directory / "core.log")
            self.check_cancelled(run)
            eval_binary = root / "target/release/nes-eval"
            build_logged(
                ["cargo", "build", "--release", "--locked", "--bin", "nes-eval",
                 "--manifest-path", str(REPOSITORY / "workloads/nes/Cargo.toml")],
                directory / "build.log",
                {"CARGO_TARGET_DIR": str(root / "target")},
            )
            self.check_cancelled(run)
            request = run_request(
                settings,
                str(rom),
                hashlib.sha256(rom.read_bytes()).hexdigest(),
                str(core),
                hashlib.sha256(core.read_bytes()).hexdigest(),
            )
            (directory / "request.json").write_text(json.dumps(request))
            (directory / "launch.json").write_text(json.dumps(settings.__dict__))
            run.message = "Starting"
            log = (directory / "nes-eval.log").open("wb")
            run.process = subprocess.Popen(
                [str(eval_binary), str(directory / "request.json"), str(directory / "out")],
                cwd=directory,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
                preexec_fn=lambda: os.nice(10),
            )
            log.close()
            port = self.listening_port(run, directory / "nes-eval.log")
            with self.lock:
                if run.cancelled:
                    stopped = True
                else:
                    stopped = False
                    run.local_port, run.state, run.message = port, "running", ""
            if stopped:
                self.close_process(run)
        except Cancelled:
            return
        except (RuntimeError, OSError, subprocess.CalledProcessError, ValueError) as error:
            self.close_process(run)
            with self.lock:
                if not run.cancelled:
                    run.state, run.message = "failed", str(error)

    def listening_port(self, run: Run, log: Path) -> int:
        assert run.process is not None
        for _ in range(240):
            self.check_cancelled(run)
            match = LISTEN_PATTERN.search(log.read_text(errors="replace"))
            if match:
                return int(match.group(1))
            if run.process.poll() is not None:
                tail = log.read_text(errors="replace").strip().splitlines()[-5:]
                raise RuntimeError(" / ".join(tail) or "nes-eval exited before opening its live port")
            time.sleep(0.5)
        raise RuntimeError("the run did not open its live port")

    def close_process(self, run: Run) -> None:
        if run.process is not None and run.process.poll() is None:
            run.process.terminate()
            try:
                run.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                run.process.kill()

    def cancelled(self, run: Run) -> bool:
        with self.lock:
            return run.cancelled

    def check_cancelled(self, run: Run) -> None:
        if self.cancelled(run):
            raise Cancelled

    def attach(self, run: Run, machine: Machine, line: str) -> None:
        match = LISTEN_PATTERN.search(line)
        if match is None:
            raise RuntimeError(f"unexpected live line: {line.strip()}")
        port = free_port()
        run.tunnel = subprocess.Popen(
            [
                "ssh",
                "-N",
                "-o",
                "BatchMode=yes",
                "-o",
                "ExitOnForwardFailure=yes",
                "-L",
                f"{port}:127.0.0.1:{match.group(1)}",
                machine.ssh,
            ],
            stdin=subprocess.DEVNULL,
        )
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=1).close()
                break
            except OSError:
                time.sleep(0.1)
        with self.lock:
            if not run.cancelled:
                run.local_port, run.state, run.message = port, "running", ""
                return
        self.close_tunnel(run)
        ssh(machine, STOP, UNIT_PREFIX + run.id)

    def stop(self, run: Run) -> None:
        if run.settings is None:
            raise ValueError("the launcher stops only the runs it started")
        machine = self.config.machines[run.machine]
        with self.lock:
            run.cancelled = True
            run.state, run.message = "stopped", ""
        if machine.local:
            self.close_process(run)
            return
        ssh(machine, STOP, UNIT_PREFIX + run.id)
        self.close_tunnel(run)

    def close_tunnel(self, run: Run) -> None:
        if run.tunnel is not None:
            run.tunnel.terminate()
            run.tunnel = None

    def discover(self) -> None:
        for machine in self.config.machines.values():
            if machine.local:
                continue
            try:
                listing = ssh(machine, DISCOVER, machine.dir)
            except RuntimeError as error:
                print(error, file=sys.stderr)
                continue
            for row in listing.splitlines():
                run_id, line, launch = (row.split("\t") + ["", ""])[:3]
                if not RUN_ID.match(run_id) or not launch:
                    continue
                settings = Settings(**json.loads(launch))
                started = time.mktime(time.strptime(run_id[:15], "%Y%m%d-%H%M%S"))
                run = Run.launched(run_id, settings, state="running", started=started)
                with self.lock:
                    self.runs[run_id] = run
                try:
                    self.attach(run, machine, line)
                except RuntimeError as error:
                    run.state, run.message = "failed", str(error)

    def scan(self) -> None:
        while True:
            for machine in self.config.machines.values():
                if machine.local or not machine.runs:
                    continue
                try:
                    self.scan_machine(machine)
                except Exception as error:
                    print(f"{machine.name}: scan failed: {error}", file=sys.stderr)
            time.sleep(SCAN_SECONDS)

    def scan_machine(self, machine: Machine) -> None:
        try:
            found = external_runs(machine, ssh(machine, SCAN, machine.dir, *machine.runs))
        except RuntimeError as error:
            print(error, file=sys.stderr)
            return
        live = {run.id for run in found}
        with self.lock:
            known = dict(self.runs)
        for run in known.values():
            if run.settings is None and run.machine == machine.name and run.id not in live:
                if run.state == "running":
                    run.state = "finished"
                    self.close_tunnel(run)
        for run in found:
            if run.id in known and known[run.id].state == "running":
                continue
            run.state = "running"
            with self.lock:
                self.runs[run.id] = run
            try:
                self.attach(run, machine, run.message)
            except RuntimeError as error:
                run.state, run.message = "failed", str(error)

    def refresh(self) -> None:
        while True:
            time.sleep(STATUS_SECONDS)
            with self.lock:
                running = [
                    run
                    for run in self.runs.values()
                    if run.state == "running" and run.settings is not None
                ]
            for run in running:
                if run.process is not None and run.process.poll() is not None:
                    run.state = "finished"
            for name, machine in self.config.machines.items():
                mine = [run for run in running if run.machine == name]
                if not mine or machine.local:
                    continue
                try:
                    states = ssh(machine, STATUS, *(UNIT_PREFIX + run.id for run in mine))
                except RuntimeError:
                    continue
                active = dict(row.split(" ", 1) for row in states.splitlines() if " " in row)
                for run in mine:
                    if active.get(UNIT_PREFIX + run.id) != "active":
                        run.state = "finished"
                        self.close_tunnel(run)


class Handler(BaseHTTPRequestHandler):
    launcher: Launcher
    port: int

    def log_message(self, format: str, *args: Any) -> None:
        pass

    def allowed(self) -> bool:
        if local_request(self.headers.get("Host"), self.headers.get("Origin"), self.port):
            return True
        self.reply(403, b"forbidden", "text/plain")
        return False

    def reply(self, status: int, body: bytes, kind: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header(LAUNCHER_HEADER, "1")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def json(self, status: int, value: Any) -> None:
        self.reply(status, json.dumps(value).encode(), "application/json")

    def do_GET(self) -> None:
        if not self.allowed():
            return
        path = urlsplit(self.path).path
        launcher = self.launcher
        if path == "/launcher/machines":
            machines = [
                {"name": m.name, "workers": m.workers, "memory_mib": m.memory_mib, "memory_max_mib": m.memory_max_mib}
                for m in launcher.config.machines.values()
            ]
            games = available_games(launcher.site, launcher.config)
            return self.json(200, {"machines": machines, "games": games})
        if path == "/launcher/runs":
            with launcher.lock:
                runs = [run.describe() for run in launcher.runs.values()]
            return self.json(200, {"runs": sorted(runs, key=lambda run: -run["started"])})
        prefix, rest = path, self.path
        match = re.match(r"^/runs/([^/]+)(/.*)$", path)
        if match:
            run = launcher.runs.get(match.group(1))
            if run is None:
                return self.reply(404, b"no such run", "text/plain")
            prefix = match.group(2)
            rest = self.path[len("/runs/" + run.id) :]
            if prefix.startswith("/live/"):
                return self.proxy(run, rest)
        if prefix.startswith("/roms/"):
            return self.rom(prefix.removeprefix("/roms/"))
        file = site_file(launcher.site, rest)
        if file is None:
            return self.reply(404, b"not found", "text/plain")
        kind = mimetypes.guess_type(file.name)[0] or "application/octet-stream"
        self.reply(200, file.read_bytes(), kind)

    def rom(self, name: str) -> None:
        roms = self.launcher.config.roms
        if name == "manifest.json":
            games = [
                game
                for game, entry in GAMES.items()
                if not entry.from_site and roms and (roms / entry.rom).is_file()
            ]
            return self.json(200, {"games": games})
        file = site_file(roms, name) if roms else None
        if file is None:
            return self.reply(404, b"not found", "text/plain")
        self.reply(200, file.read_bytes(), "application/octet-stream")

    def proxy(self, run: Run, path: str) -> None:
        if run.local_port is None:
            return self.reply(503, b"the run has no live port yet", "text/plain")
        upstream = http.client.HTTPConnection("127.0.0.1", run.local_port, timeout=30)
        try:
            upstream.request("GET", path)
            response = upstream.getresponse()
            self.send_response(response.status)
            for name in ("Content-Type", "Cache-Control", "Content-Length"):
                if response.getheader(name):
                    self.send_header(name, str(response.getheader(name)))
            self.end_headers()
            while chunk := response.read1(65536):
                self.wfile.write(chunk)
                self.wfile.flush()
        except OSError:
            pass
        finally:
            upstream.close()

    def do_HEAD(self) -> None:
        if self.allowed():
            self.reply(200, b"", "text/html")

    def do_POST(self) -> None:
        if not self.allowed():
            return
        if self.headers.get("Content-Type") != "application/json":
            return self.reply(415, b"send application/json", "text/plain")
        launcher = self.launcher
        path = urlsplit(self.path).path
        try:
            if path == "/launcher/runs":
                length = min(int(self.headers.get("Content-Length", "0")), 65536)
                body = json.loads(self.rfile.read(length) or b"{}")
                run = launcher.start(parse_settings(body, launcher.config))
                return self.json(200, run.describe())
            match = re.match(r"^/launcher/runs/([^/]+)/stop$", path)
            if match and match.group(1) in launcher.runs:
                run = launcher.runs[match.group(1)]
                launcher.stop(run)
                return self.json(200, run.describe())
        except (KeyError, ValueError, RuntimeError) as error:
            return self.json(400, {"error": str(error)})
        self.reply(404, b"not found", "text/plain")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--site", type=Path, default=REPOSITORY / "demos/twin-dragons/dist")
    parser.add_argument("--config", type=Path, default=DEFAULT_CONFIG)
    parser.add_argument("--port", type=int, default=8790)
    args = parser.parse_args()
    config = load_config(args.config)
    if not (args.site / "index.html").is_file():
        parser.error(f"{args.site} has no built arcade site; run npm run build")
    launcher = Launcher(config, args.site)
    threading.Thread(target=launcher.discover, daemon=True).start()
    threading.Thread(target=launcher.refresh, daemon=True).start()
    threading.Thread(target=launcher.scan, daemon=True).start()
    Handler.launcher, Handler.port = launcher, args.port
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"Open http://127.0.0.1:{args.port}/")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    for run in launcher.runs.values():
        launcher.close_tunnel(run)
        launcher.close_process(run)
    return 0


if __name__ == "__main__":
    sys.exit(main())
