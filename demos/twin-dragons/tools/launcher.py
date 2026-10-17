#!/usr/bin/env -S uv run --script
# SPDX-License-Identifier: AGPL-3.0-or-later
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Start native arcade campaigns on SSH machines and watch them in the arcade page.

The launcher serves a built arcade site on a loopback port. The page offers a
Native run dialog when it finds the launcher. A run copies this checkout's
committed source to a machine, builds the QuickNES core and `nes-eval` there
once per commit, starts `nes-eval` with a `live` request under `systemd-run`
with a memory cap, and forwards the live port back over SSH. The page watches a
run at `/runs/<id>/?game=<game>&live`, which serves the same site and proxies
`/runs/<id>/live/` to that run.

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
import re
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
MEMORY_HEADROOM_MIB = 1024
LISTEN_PATTERN = re.compile(r"live view listening on 127\.0\.0\.1:(\d+)")
RUN_ID = re.compile(r"^[0-9]{8}-[0-9]{6}-[a-z0-9-]+$")
STATUS_SECONDS = 10.0


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
}


@dataclass(frozen=True)
class Machine:
    name: str
    ssh: str
    dir: str
    workers: int
    memory_mib: int


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


@dataclass
class Run:
    id: str
    settings: Settings
    state: str = "starting"
    message: str = ""
    started: float = field(default_factory=time.time)
    local_port: int | None = None
    tunnel: subprocess.Popen[bytes] | None = None

    def describe(self) -> dict[str, Any]:
        s = self.settings
        return {
            "id": self.id,
            "game": s.game,
            "machine": s.machine,
            "workers": s.workers,
            "memory_mib": s.memory_mib,
            "seed": s.seed,
            "hours": s.hours,
            "state": self.state,
            "message": self.message,
            "started": self.started,
            "watch": f"/runs/{self.id}/?game={s.game}&live" if self.local_port else None,
        }


def load_config(path: Path) -> Config:
    data = tomllib.loads(path.expanduser().read_text())
    machines: dict[str, Machine] = {}
    for name, entry in data.get("machines", {}).items():
        machines[name] = Machine(
            name=name,
            ssh=str(entry["ssh"]),
            dir=str(entry["dir"]),
            workers=int(entry["workers"]),
            memory_mib=int(entry["memory_mib"]),
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
    return Settings(game, machine, workers, memory_mib, seed, hours)


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
systemd-run $user --unit "$4" --collect -p MemoryMax="$5"M -p Nice=10 \\
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


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


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
        run = Run(f"{time.strftime('%Y%m%d-%H%M%S')}-{settings.game}", settings)
        with self.lock:
            self.runs[run.id] = run
        threading.Thread(target=self.deploy, args=(run,), daemon=True).start()
        return run

    def deploy(self, run: Run) -> None:
        settings = run.settings
        machine = self.config.machines[settings.machine]
        try:
            commit = self.commit()
            run.message = f"Copying {commit[:9]} to {machine.name}"
            archive = subprocess.run(
                ["git", "-C", str(REPOSITORY), "archive", "--format=tar", commit],
                capture_output=True,
                check=True,
            ).stdout
            ssh(machine, UNPACK, machine.dir, commit, stdin=archive)
            run.message = f"Building {commit[:9]} on {machine.name}"
            core_sha256 = ssh(machine, BUILD, machine.dir, commit).strip()
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
            run.message = f"Starting on {machine.name}"
            line = ssh(
                machine,
                START,
                machine.dir,
                run.id,
                launch,
                UNIT_PREFIX + run.id,
                str(settings.memory_mib + MEMORY_HEADROOM_MIB),
                commit,
                stdin=json.dumps(request).encode(),
            )
            self.attach(run, machine, line)
        except (RuntimeError, OSError, subprocess.CalledProcessError, ValueError) as error:
            run.state, run.message = "failed", str(error)

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
        run.local_port, run.state, run.message = port, "running", ""

    def stop(self, run: Run) -> None:
        machine = self.config.machines[run.settings.machine]
        ssh(machine, STOP, UNIT_PREFIX + run.id)
        run.state = "stopped"
        self.close_tunnel(run)

    def close_tunnel(self, run: Run) -> None:
        if run.tunnel is not None:
            run.tunnel.terminate()
            run.tunnel = None

    def discover(self) -> None:
        for machine in self.config.machines.values():
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
                run = Run(run_id, settings, state="running")
                with self.lock:
                    self.runs[run_id] = run
                try:
                    self.attach(run, machine, line)
                except RuntimeError as error:
                    run.state, run.message = "failed", str(error)

    def refresh(self) -> None:
        while True:
            time.sleep(STATUS_SECONDS)
            with self.lock:
                running = [run for run in self.runs.values() if run.state == "running"]
            for name, machine in self.config.machines.items():
                mine = [run for run in running if run.settings.machine == name]
                if not mine:
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
        self.end_headers()
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
                {"name": m.name, "workers": m.workers, "memory_mib": m.memory_mib}
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
    Handler.launcher, Handler.port = launcher, args.port
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"Open http://127.0.0.1:{args.port}/")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    for run in launcher.runs.values():
        launcher.close_tunnel(run)
    return 0


if __name__ == "__main__":
    sys.exit(main())
