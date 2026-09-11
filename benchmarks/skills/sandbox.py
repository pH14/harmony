# SPDX-License-Identifier: AGPL-3.0-or-later
"""Bounded Linux/Docker execution for the paired skill evaluator.

The evaluator owns the host-side Docker client and passes only verified bytes to
the container.  The agent-facing command is always a typed ``docker exec``
argv; no host shell, host path, socket, device, environment, or network is
available inside the container.
"""

from __future__ import annotations

import base64
import hashlib
import json
import math
import os
import platform
import re
import selectors
import signal
import shutil
import stat
import subprocess
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import BinaryIO, Literal, Sequence

try:
    from . import materials
except ImportError:  # unittest discovery loads this directory as top-level modules.
    import materials  # type: ignore[no-redef]


_IMAGE_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
_HEX_RE = re.compile(r"^[0-9a-f]{64}$")
_CONTAINER_RE = re.compile(r"^[0-9a-f]{64}$")
_UNTRUSTED_OUTPUT_LIMIT = 256 * 1024
_DOCKER_COMMAND_TIMEOUT = 30.0
_CLEANUP_TIMEOUT = 5.0
_DOCKER_ENDPOINT = "unix:///var/run/docker.sock"
_ENV_ASSIGNMENTS = (
    "PATH=/usr/bin:/bin",
    "HOME=/work/home",
    "XDG_CONFIG_HOME=/work/config",
    "XDG_CACHE_HOME=/tmp/cache",
    "TMPDIR=/tmp",
    "LC_ALL=C.UTF-8",
)
_KEEPALIVE = ("/bin/sh", "-c", "while :; do sleep 3600; done")
_TMPFS_DESTINATIONS = frozenset({"/work", "/tmp"})


class SandboxError(RuntimeError):
    """Raised when the requested isolated execution cannot be established."""


def _positive_real(name: str, value: object) -> None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{name} must be a positive finite number")
    if not math.isfinite(float(value)) or float(value) <= 0:
        raise ValueError(f"{name} must be a positive finite number")


def _positive_int(name: str, value: object) -> None:
    if type(value) is not int or value <= 0:
        raise ValueError(f"{name} must be a positive integer")


@dataclass(frozen=True)
class Limits:
    """Per-sandbox resource ceilings.

    ``wall`` is the total host time available to calls to :meth:`Sandbox.run`.
    ``toolcalls`` counts those calls before dispatch.  Byte fields are bytes;
    ``cpus`` is a positive fractional CPU quota and ``pids`` is the container
    process limit.
    """

    wall: float
    toolcalls: int
    memorybytes: int
    workbytes: int
    cpus: float
    pids: int
    outputbytes: int

    def __post_init__(self) -> None:
        _positive_real("wall", self.wall)
        _positive_int("toolcalls", self.toolcalls)
        _positive_int("memorybytes", self.memorybytes)
        _positive_int("workbytes", self.workbytes)
        _positive_real("cpus", self.cpus)
        _positive_int("pids", self.pids)
        _positive_int("outputbytes", self.outputbytes)


@dataclass(frozen=True)
class RunResult:
    """The bounded result of one command dispatched inside the container."""

    exit_code: int | None
    stdout: bytes
    stderr: bytes
    termination: str


@dataclass(frozen=True)
class _ProcessResult:
    exit_code: int | None
    stdout: bytes
    stderr: bytes
    timed_out: bool
    output_overflow: bool


def _validate_image(image: object) -> str:
    if not isinstance(image, str) or not _IMAGE_RE.fullmatch(image):
        raise ValueError("image must be an immutable sha256:<64 lowercase hex> ID")
    return image


def _validate_manifest_hash(value: object) -> str:
    if not isinstance(value, str) or not _HEX_RE.fullmatch(value):
        raise ValueError("expected_manifest_sha256 must be 64 lowercase hex characters")
    return value


def _validate_argv(argv: object) -> list[str]:
    if not isinstance(argv, list) or not argv:
        raise ValueError("argv must be a nonempty list")
    if any(not isinstance(arg, str) or "\x00" in arg for arg in argv):
        raise ValueError("argv entries must be NUL-free strings")
    if not argv[0] or not Path(argv[0]).is_absolute():
        raise ValueError("argv[0] must be an absolute program path")
    return argv


def _nano_cpus(cpus: float) -> int:
    return int(round(float(cpus) * 1_000_000_000))


def _tmpfs_arg(destination: str, size: int) -> str:
    execution = "exec" if destination == "/work" else "noexec"
    return (
        f"{destination}:rw,{execution},nosuid,nodev,size={size},"
        "uid=65534,gid=65534,mode=700"
    )


def _create_argv(image: str, limits: Limits) -> list[str]:
    """Build the fixed Docker create argv; no user text enters this list."""
    return [
        "docker",
        "create",
        "--read-only",
        "--network=none",
        "--ipc=private",
        "--cgroupns=private",
        "--user=65534:65534",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges:true",
        f"--pids-limit={limits.pids}",
        f"--memory={limits.memorybytes}",
        f"--memory-swap={limits.memorybytes}",
        f"--cpus={float(limits.cpus):.9f}".rstrip("0").rstrip("."),
        f"--tmpfs={_tmpfs_arg('/work', limits.workbytes)}",
        f"--tmpfs={_tmpfs_arg('/tmp', limits.workbytes)}",
        "--log-driver=none",
        "--entrypoint=/usr/bin/env",
        image,
        "-i",
        *_ENV_ASSIGNMENTS,
        *_KEEPALIVE,
    ]


def _exec_argv(
    container_id: str,
    command: Sequence[str],
    *,
    workdir: str,
    user: str = "65534:65534",
    interactive: bool = False,
) -> list[str]:
    if not isinstance(container_id, str) or not _CONTAINER_RE.fullmatch(container_id):
        raise ValueError("container_id must be a Docker container ID")
    if not isinstance(workdir, str) or "\x00" in workdir or not Path(workdir).is_absolute():
        raise ValueError("workdir must be an absolute NUL-free path")
    if not isinstance(user, str) or "\x00" in user or not user:
        raise ValueError("user must be a nonempty NUL-free string")
    if not command or any(not isinstance(arg, str) or "\x00" in arg for arg in command):
        raise ValueError("container command must be a nonempty NUL-free argv")
    if not Path(command[0]).is_absolute():
        raise ValueError("container command must use an absolute program path")
    result = [
        "docker",
        "exec",
        f"--user={user}",
        f"--workdir={workdir}",
    ]
    if interactive:
        result.append("-i")
    result.extend(
        [
            container_id,
            "/usr/bin/env",
            "-i",
            *_ENV_ASSIGNMENTS,
            *command,
        ]
    )
    return result


def _kill_process(process: subprocess.Popen[bytes]) -> None:
    """Kill the Docker client and its process group after a hard limit."""
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except (OSError, ProcessLookupError):
        pass
    try:
        process.kill()
    except (OSError, ProcessLookupError):
        pass


def _drain(
    process: subprocess.Popen[bytes],
    *,
    timeout: float,
    output_limit: int,
    stdout: bytearray,
    stderr: bytearray,
    enforce_limit: bool,
) -> tuple[bool, bool]:
    """Drain both pipes with a deadline and a combined output cap."""
    selector = selectors.DefaultSelector()
    streams: dict[int, bytearray] = {}
    for stream, target in ((process.stdout, stdout), (process.stderr, stderr)):
        if stream is not None:
            selector.register(stream, selectors.EVENT_READ)
            streams[stream.fileno()] = target

    deadline = time.monotonic() + max(0.0, timeout)
    timed_out = False
    overflow = False
    try:
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                timed_out = True
                break
            events = selector.select(remaining)
            if not events:
                timed_out = True
                break
            for key, _ in events:
                fd = key.fileobj.fileno()
                try:
                    chunk = os.read(fd, 64 * 1024)
                except OSError:
                    chunk = b""
                if not chunk:
                    selector.unregister(key.fileobj)
                    streams.pop(fd, None)
                    continue
                target = streams[fd]
                used = len(stdout) + len(stderr)
                room = max(0, output_limit - used)
                target.extend(chunk[:room])
                if enforce_limit and len(chunk) > room:
                    overflow = True
                    break
            if timed_out or overflow:
                break
    finally:
        selector.close()
    return timed_out, overflow


def _run_bounded(
    argv: Sequence[str],
    *,
    timeout: float,
    output_limit: int,
    env: dict[str, str] | None,
    stdin: BinaryIO | None = None,
) -> _ProcessResult:
    """Run a typed argv with bounded pipes; ``shell=True`` is never used.

    ``stdin`` is a regular, already bounded file supplied by the controller;
    it is never a pipe written by the caller, so a large staged arm cannot
    deadlock the Docker client.
    """
    if not argv or any(not isinstance(arg, str) or "\x00" in arg for arg in argv):
        raise ValueError("process argv must be a nonempty NUL-free sequence")
    _positive_real("timeout", timeout)
    _positive_int("output_limit", output_limit)
    try:
        process = subprocess.Popen(
            list(argv),
            stdin=stdin if stdin is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            shell=False,
            close_fds=True,
            start_new_session=True,
            env=env,
        )
    except OSError as exc:
        raise SandboxError("could not start the Docker client") from exc

    stdout = bytearray()
    stderr = bytearray()
    timed_out = False
    overflow = False
    deadline = time.monotonic() + float(timeout)
    try:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            timed_out = True
        else:
            timed_out, overflow = _drain(
                process,
                timeout=remaining,
                output_limit=output_limit,
                stdout=stdout,
                stderr=stderr,
                enforce_limit=True,
            )
        if not timed_out and not overflow:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                timed_out = True
            else:
                try:
                    process.wait(timeout=remaining)
                except subprocess.TimeoutExpired:
                    timed_out = True
        if timed_out or overflow:
            _kill_process(process)
            _drain(
                process,
                timeout=1.0,
                output_limit=output_limit,
                stdout=stdout,
                stderr=stderr,
                enforce_limit=False,
            )
            try:
                process.wait(timeout=1.0)
            except subprocess.TimeoutExpired:
                pass
    except BaseException:
        _kill_process(process)
        try:
            process.wait(timeout=1.0)
        except subprocess.TimeoutExpired:
            pass
        raise
    finally:
        if process.stdout is not None:
            process.stdout.close()
        if process.stderr is not None:
            process.stderr.close()
    return _ProcessResult(
        process.returncode,
        bytes(stdout),
        bytes(stderr),
        timed_out,
        overflow,
    )


def _validate_image_inspect(info: object, image: str) -> None:
    if not isinstance(info, dict) or info.get("Id") != image or info.get("Os") != "linux":
        raise SandboxError("Docker image identity or operating system did not match")
    config = info.get("Config")
    if not isinstance(config, dict):
        raise SandboxError("Docker image has no inspectable config")
    volumes = config.get("Volumes")
    if volumes not in (None, {}):
        raise SandboxError("Docker images declaring volumes are not allowed")


def _json_inspect(result: _ProcessResult, what: str) -> object:
    if result.timed_out or result.output_overflow or result.exit_code != 0:
        raise SandboxError(f"Docker {what} failed")
    try:
        value = json.loads(result.stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise SandboxError(f"Docker {what} returned invalid JSON") from exc
    return value


def _validate_container_inspect(
    info: object,
    *,
    container_id: str,
    image: str,
    limits: Limits,
) -> None:
    """Check the daemon's realized boundary, not merely the create request."""
    if not isinstance(info, dict) or info.get("Id") != container_id:
        raise SandboxError("Docker container identity did not match")
    config = info.get("Config")
    host = info.get("HostConfig")
    mounts = info.get("Mounts")
    state = info.get("State")
    if not all(isinstance(value, dict) for value in (config, host, state)):
        raise SandboxError("Docker container inspect is incomplete")
    if config.get("Image") != image:
        raise SandboxError("Docker container image identity changed")
    if config.get("Entrypoint") != ["/usr/bin/env"]:
        raise SandboxError("Docker entrypoint boundary mismatch")
    if config.get("Cmd") != ["-i", *_ENV_ASSIGNMENTS, *_KEEPALIVE]:
        raise SandboxError("Docker keepalive or environment boundary mismatch")
    if config.get("User") != "65534:65534":
        raise SandboxError("Docker container is not running as uid 65534")
    if config.get("Volumes") not in (None, {}):
        raise SandboxError("Docker container declares a volume")
    if host.get("ReadonlyRootfs") is not True:
        raise SandboxError("Docker root filesystem is writable")
    if host.get("NetworkMode") != "none":
        raise SandboxError("Docker network boundary mismatch")
    if host.get("Privileged") is True:
        raise SandboxError("privileged Docker containers are forbidden")
    if host.get("PidsLimit") != limits.pids:
        raise SandboxError("Docker process limit mismatch")
    if host.get("Memory") != limits.memorybytes or host.get("MemorySwap") != limits.memorybytes:
        raise SandboxError("Docker memory or swap limit mismatch")
    if host.get("NanoCpus") != _nano_cpus(limits.cpus):
        raise SandboxError("Docker CPU limit mismatch")
    if host.get("CapDrop") != ["ALL"]:
        raise SandboxError("Docker capability drop mismatch")
    if "no-new-privileges:true" not in (host.get("SecurityOpt") or []):
        raise SandboxError("Docker no-new-privileges boundary is missing")
    if host.get("Binds") not in (None, []):
        raise SandboxError("Docker host bind mounts are forbidden")
    if host.get("Devices") not in (None, []):
        raise SandboxError("Docker host devices are forbidden")
    pid_mode = host.get("PidMode")
    # Docker represents a new private PID namespace with the empty mode.
    # Its API accepts only empty, host, or container:<id>; "private" is not a valid flag.
    if pid_mode != "" or host.get("IpcMode") != "private":
        raise SandboxError("Docker host namespaces are forbidden")
    if host.get("UTSMode") != "" or host.get("UsernsMode") != "":
        raise SandboxError("Docker host namespaces are forbidden")
    if host.get("CgroupnsMode") != "private":
        raise SandboxError("Docker cgroup namespace is not private")
    log_config = host.get("LogConfig")
    if not isinstance(log_config, dict) or log_config.get("Type") != "none":
        raise SandboxError("Docker logging is not bounded")
    tmpfs = host.get("Tmpfs")
    if not isinstance(tmpfs, dict) or set(tmpfs) != _TMPFS_DESTINATIONS:
        raise SandboxError("Docker tmpfs boundary mismatch")
    for destination, value in tmpfs.items():
        expected = _tmpfs_arg(destination, limits.workbytes).split(":", 1)[1]
        if not isinstance(value, str) or set(value.split(",")) != set(expected.split(",")):
            raise SandboxError("Docker tmpfs options are not bounded")
    # --tmpfs is represented in HostConfig.Tmpfs, separately from the
    # volume/bind/--mount entries returned by inspect.Mounts. The fixed
    # stager checks the kernel's actual tmpfs mounts before any agent call.
    if mounts != []:
        raise SandboxError("Docker volume or bind mounts are forbidden")
    network_settings = info.get("NetworkSettings")
    if not isinstance(network_settings, dict):
        raise SandboxError("Docker network inspection is incomplete")
    networks = network_settings.get("Networks")
    if networks not in ({}, None):
        if not isinstance(networks, dict) or set(networks) != {"none"}:
            raise SandboxError("Docker network attachments are not isolated")
        none_network = networks.get("none")
        if not isinstance(none_network, dict):
            raise SandboxError("Docker none network inspection is invalid")
        for field in (
            "IPAddress",
            "GlobalIPv6Address",
            "Gateway",
            "GlobalIPv6Gateway",
            "MacAddress",
        ):
            if none_network.get(field) not in (None, ""):
                raise SandboxError("Docker none network has an assigned address")
        for field in ("Links", "Aliases", "IPAMConfig", "DriverOpts"):
            if none_network.get(field) not in (None, "", [], {}):
                raise SandboxError("Docker none network has an endpoint")
    if state.get("Running") is not True:
        raise SandboxError("Docker keepalive container is not running")


_KERNEL_MOUNTS_SCRIPT = r'''import os,stat

def fail(message):
    raise RuntimeError(message)

mount_rows = [line.split() for line in open("/proc/self/mountinfo")]
for destination, executable in (("/work", True), ("/tmp", False)):
    rows = [row for row in mount_rows if len(row) > 6 and row[4] == destination]
    if len(rows) != 1:
        fail("missing or duplicate tmpfs mount")
    row = rows[0]
    separator = row.index("-")
    options = set(row[5].split(","))
    if row[separator + 1] != "tmpfs" or not {"rw", "nosuid", "nodev"} <= options:
        fail("kernel tmpfs boundary mismatch")
    if ("noexec" not in options) != executable:
        fail("kernel tmpfs executable mode mismatch")
    info = os.stat(destination)
    if info.st_uid != 65534 or info.st_gid != 65534 or stat.S_IMODE(info.st_mode) != 0o700:
        fail("kernel tmpfs ownership or mode mismatch")
'''


_STAGE_SCRIPT = r'''import base64,hashlib,json,os,shutil,stat,sys

def fail(message):
    raise RuntimeError(message)

def pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            fail("duplicate JSON key")
        result[key] = value
    return result

def rel(value):
    if not isinstance(value, str) or not value or value.startswith("/") or "\\" in value or "\x00" in value:
        fail("invalid relative path")
    parts = value.split("/")
    if any(not part or part in (".", "..") for part in parts) or "/".join(parts) != value:
        fail("invalid relative path")
    return parts

def path(root, name):
    return os.path.join(root, *rel(name))

def canonical(value):
    return json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":")).encode("ascii")

def metadata(arm, directories, files):
    return {
        "version": 1,
        "arm": arm,
        "directories": directories,
        "files": [
            {"path": item["path"], "mode": item["mode"], "size": len(item["data"]), "sha256": hashlib.sha256(item["data"]).hexdigest()}
            for item in files
        ],
    }

def check_tree(root, arm, directories, files, frozen):
    expected = set(directories) | {item["path"] for item in files}
    for name in directories:
        target = path(root, name)
        info = os.lstat(target)
        if not stat.S_ISDIR(info.st_mode) or (info.st_mode & 0o777) != (0o555 if frozen else 0o700):
            fail("directory mode or type mismatch")
    for item in files:
        target = path(root, item["path"])
        info = os.lstat(target)
        wanted = (0o444 if frozen else 0o600) | item["mode"]
        if not stat.S_ISREG(info.st_mode) or (info.st_mode & 0o777) != wanted:
            fail("file mode or type mismatch")
        with open(target, "rb") as stream:
            data = stream.read()
        if len(data) != len(item["data"]) or data != item["data"]:
            fail("file content mismatch")
    for current, names, files_on_disk in os.walk(root, topdown=True, followlinks=False):
        relative = os.path.relpath(current, root)
        if relative == ".":
            relative = None
        for name in names + files_on_disk:
            candidate = name if relative is None else relative + "/" + name
            if candidate not in expected:
                fail("unexpected staged path")

def write_file(target, data, mode, readonly):
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(target, flags, 0o600)
    try:
        offset = 0
        while offset < len(data):
            offset += os.write(descriptor, data[offset:])
    finally:
        os.close(descriptor)
    os.chmod(target, (0o444 if readonly else 0o600) | mode)

try:
    if len(sys.argv) != 3 or sys.argv[1] not in ("docs", "skills"):
        fail("invalid staging arguments")
    arm = sys.argv[1]
    maximum = int(sys.argv[2])
    raw = sys.stdin.buffer.read(maximum + 1)
    if len(raw) > maximum:
        fail("staging payload exceeds work limit")
    document = json.loads(raw.decode("utf-8"), object_pairs_hook=pairs)
    if not isinstance(document, dict) or set(document) != {"version", "arm", "directories", "files", "digest"}:
        fail("invalid staging document")
    if canonical(document) != raw:
        fail("staging document is not canonical")
    if type(document["version"]) is not int or document["version"] != 1 or document["arm"] != arm or not isinstance(document["directories"], list) or not isinstance(document["files"], list):
        fail("invalid staging document")
    directories = document["directories"]
    if directories != sorted(set(directories), key=lambda item: (len(rel(item)), item)):
        fail("directory list is not canonical")
    for name in directories:
        rel(name)
    directory_set = set(directories)
    for name in directories:
        parts = rel(name)
        for count in range(1, len(parts)):
            if "/".join(parts[:count]) not in directory_set:
                fail("directory parent is missing")
    files = document["files"]
    if files != sorted(files, key=lambda item: item.get("path", "")):
        fail("file list is not canonical")
    names = set()
    total = 0
    for item in files:
        if not isinstance(item, dict) or set(item) != {"path", "mode", "data"}:
            fail("invalid file record")
        name = item["path"]
        rel(name)
        if name in names or name in directory_set:
            fail("file path overlaps")
        names.add(name)
        mode = item["mode"]
        if type(mode) is not int or mode < 0 or mode & ~0o111:
            fail("invalid file mode")
        try:
            data = base64.b64decode(item["data"], validate=True)
        except Exception:
            fail("invalid file data")
        total += len(data)
        if total > maximum:
            fail("staged files exceed work limit")
        item["data"] = data
        parts = rel(name)
        for count in range(1, len(parts)):
            if "/".join(parts[:count]) not in directory_set:
                fail("file parent is missing")
    if document["digest"] != hashlib.sha256(canonical(metadata(arm, directories, files))).hexdigest():
        fail("staging digest does not match payload")
    for directory in ("/work/home", "/work/config", "/tmp/cache"):
        os.makedirs(directory, mode=0o700, exist_ok=True)
        os.chmod(directory, 0o700)
    frozen_parent = "/work/frozen"
    frozen = os.path.join(frozen_parent, arm)
    os.mkdir(frozen_parent, 0o700)
    os.mkdir(frozen, 0o700)
    for name in directories:
        os.mkdir(path(frozen, name), 0o700)
    for item in files:
        write_file(path(frozen, item["path"]), item["data"], item["mode"], True)
    for name in reversed(directories):
        os.chmod(path(frozen, name), 0o555)
    os.chmod(frozen, 0o555)
    check_tree(frozen, arm, directories, files, True)
    workspace = "/work/workspace"
    shutil.copytree(frozen, workspace, copy_function=shutil.copy2)
    os.chmod(workspace, 0o700)
    for name in directories:
        os.chmod(path(workspace, name), 0o700)
    for item in files:
        os.chmod(path(workspace, item["path"]), 0o600 | item["mode"])
    check_tree(workspace, arm, directories, files, False)
    sys.stdout.write(document["digest"] + "\n")
except Exception as error:
    sys.stderr.write("staging failed: " + str(error) + "\n")
    raise
'''


def _read_stage_file(path: Path, maximum: int) -> bytes:
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise SandboxError("staged arm file cannot be opened") from exc
    try:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or info.st_size > maximum:
            raise SandboxError("staged arm file exceeds work limit")
        data = bytearray()
        while len(data) <= maximum:
            chunk = os.read(descriptor, min(64 * 1024, maximum + 1 - len(data)))
            if not chunk:
                return bytes(data)
            data.extend(chunk)
        raise SandboxError("staged arm file exceeds work limit")
    except OSError as exc:
        raise SandboxError("staged arm file cannot be read") from exc
    finally:
        os.close(descriptor)


def _stage_payload(pair: Path, arm: str, maximum: int) -> tuple[bytes, str]:
    """Encode one arm; the other arm never enters the container."""
    root = pair / arm
    try:
        root_info = os.lstat(root)
    except OSError as exc:
        raise SandboxError("selected material arm cannot be inspected") from exc
    if not stat.S_ISDIR(root_info.st_mode):
        raise SandboxError("selected material arm is not a directory")
    directories: list[str] = []
    files: list[dict[str, object]] = []
    total_bytes = 0
    for current, dirnames, filenames in os.walk(root, topdown=True, followlinks=False):
        dirnames.sort()
        filenames.sort()
        current_path = Path(current)
        for name in dirnames:
            child = current_path / name
            info = os.lstat(child)
            if not stat.S_ISDIR(info.st_mode):
                raise SandboxError("selected arm contains a non-directory entry")
            directories.append(child.relative_to(root).as_posix())
        for name in filenames:
            child = current_path / name
            info = os.lstat(child)
            if not stat.S_ISREG(info.st_mode):
                raise SandboxError("selected arm contains a non-regular entry")
            data = _read_stage_file(child, maximum)
            total_bytes += len(data)
            if total_bytes > maximum:
                raise SandboxError("selected arm exceeds work limit")
            files.append(
                {
                    "path": child.relative_to(root).as_posix(),
                    "mode": info.st_mode & 0o111,
                    "data": data,
                }
            )
    directories.sort(key=lambda item: (len(item.split("/")), item))
    files.sort(key=lambda item: str(item["path"]))
    metadata = {
        "version": 1,
        "arm": arm,
        "directories": directories,
        "files": [
            {
                "path": item["path"],
                "mode": item["mode"],
                "size": len(item["data"]),
                "sha256": hashlib.sha256(item["data"]).hexdigest(),
            }
            for item in files
        ],
    }
    digest = hashlib.sha256(
        json.dumps(metadata, ensure_ascii=True, sort_keys=True, separators=(",", ":")).encode("ascii")
    ).hexdigest()
    document = {
        "version": 1,
        "arm": arm,
        "directories": directories,
        "files": [
            {
                "path": item["path"],
                "mode": item["mode"],
                "data": base64.b64encode(item["data"]).decode("ascii"),
            }
            for item in files
        ],
        "digest": digest,
    }
    payload = json.dumps(document, ensure_ascii=True, sort_keys=True, separators=(",", ":")).encode("ascii")
    if len(payload) > maximum:
        raise SandboxError("staging payload exceeds work limit")
    return payload, digest


class Sandbox:
    """A disposable, Linux-only Docker boundary for one evaluation arm."""

    def __init__(self, image: str, limits: Limits) -> None:
        self.image = _validate_image(image)
        if not isinstance(limits, Limits):
            raise TypeError("limits must be a Limits instance")
        self.limits = limits
        self._container_id: str | None = None
        self._entered = False
        self._closed = False
        self._poisoned = False
        self._staged = False
        self._run_started: float | None = None
        self._toolcalls = 0
        self._lock = threading.RLock()
        self._watchdog: threading.Timer | None = None
        self._watchdog_expired = False
        self._watchdog_error: SandboxError | None = None
        self._docker_config_dir: Path | None = None

    def __enter__(self) -> Sandbox:
        if self._entered:
            raise SandboxError("sandbox is already entered")
        self._entered = True
        try:
            self._prepare_docker_config()
            self._require_linux_daemon()
            self._inspect_image()
            self._create()
            self._start()
            self._inspect_container()
            self._run_started = time.monotonic()
            self._watchdog = threading.Timer(float(self.limits.wall), self._expire_wall_budget)
            self._watchdog.daemon = True
            self._watchdog.start()
            return self
        except BaseException as original:
            cleanup_error: SandboxError | None = None
            try:
                self._destroy()
            except SandboxError as cleanup:
                cleanup_error = cleanup
            try:
                self._cleanup_docker_config()
            except SandboxError as cleanup:
                self._closed = True
                if cleanup_error is not None:
                    raise cleanup from cleanup_error
                raise cleanup from original
            self._closed = True
            if cleanup_error is not None:
                raise cleanup_error from original
            raise

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> bool:
        with self._lock:
            self._closed = True
            self._cancel_watchdog()
            cleanup_error: SandboxError | None = None
            try:
                self._destroy()
            except SandboxError as cleanup:
                cleanup_error = cleanup
            try:
                self._cleanup_docker_config()
            except SandboxError as cleanup:
                if cleanup_error is not None:
                    raise cleanup from cleanup_error
                raise
            if cleanup_error is not None:
                raise cleanup_error
            if self._watchdog_error is not None:
                raise self._watchdog_error
            if self._watchdog_expired and exc_type is None:
                raise SandboxError("sandbox wall-time budget expired while idle")
        return False

    def _docker_env(self) -> dict[str, str]:
        # The Docker client receives only a path plus a private empty config and
        # the fixed local endpoint. Credentials, context, and user-agent
        # configuration are never inherited from the caller.
        if self._docker_config_dir is None:
            raise SandboxError("Docker client config is not initialized")
        return {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "DOCKER_CONFIG": str(self._docker_config_dir),
            "DOCKER_HOST": _DOCKER_ENDPOINT,
            "DOCKER_CONTEXT": "default",
        }

    def _prepare_docker_config(self) -> None:
        if self._docker_config_dir is not None:
            return
        try:
            directory = Path(tempfile.mkdtemp(prefix="harmony-docker-config-"))
            os.chmod(directory, 0o700)
        except OSError as exc:
            raise SandboxError("could not create private Docker config") from exc
        self._docker_config_dir = directory

    def _cleanup_docker_config(self) -> None:
        directory = self._docker_config_dir
        if directory is None:
            return
        try:
            shutil.rmtree(directory)
        except OSError as exc:
            raise SandboxError("private Docker config cleanup failed") from exc
        self._docker_config_dir = None

    def _docker_call(
        self,
        args: Sequence[str],
        *,
        timeout: float,
        output_limit: int,
        stdin: BinaryIO | None = None,
    ) -> _ProcessResult:
        return _run_bounded(
            ["docker", *args],
            timeout=timeout,
            output_limit=output_limit,
            env=self._docker_env(),
            stdin=stdin,
        )

    def _cancel_watchdog(self) -> None:
        watchdog = self._watchdog
        self._watchdog = None
        if watchdog is not None:
            watchdog.cancel()

    def _expire_wall_budget(self) -> None:
        """Destroy an idle container when its total wall budget expires."""
        with self._lock:
            if self._closed or self._container_id is None:
                return
            self._watchdog_expired = True
            self._poisoned = True
            try:
                self._destroy()
            except SandboxError as error:
                self._watchdog_error = error

    def _require_linux_daemon(self) -> None:
        if platform.system() != "Linux":
            raise SandboxError("the evaluator requires a Linux Docker daemon")
        result = self._docker_call(
            ["version", "--format", "{{.Server.Os}}"],
            timeout=_DOCKER_COMMAND_TIMEOUT,
            output_limit=4096,
        )
        if result.timed_out or result.output_overflow or result.exit_code != 0:
            raise SandboxError("Docker Linux daemon is unavailable")
        try:
            server_os = result.stdout.decode("ascii").strip()
        except UnicodeDecodeError as exc:
            raise SandboxError("Docker daemon identity is invalid") from exc
        if server_os != "linux":
            raise SandboxError("Docker daemon is not Linux")

    def _inspect_image(self) -> None:
        result = self._docker_call(
            ["image", "inspect", self.image],
            timeout=_DOCKER_COMMAND_TIMEOUT,
            output_limit=_UNTRUSTED_OUTPUT_LIMIT,
        )
        payload = _json_inspect(result, "image inspect")
        if not isinstance(payload, list) or len(payload) != 1:
            raise SandboxError("Docker image inspect returned an unexpected shape")
        _validate_image_inspect(payload[0], self.image)

    def _create(self) -> None:
        result = self._docker_call(
            _create_argv(self.image, self.limits)[1:],
            timeout=_DOCKER_COMMAND_TIMEOUT,
            output_limit=4096,
        )
        if result.timed_out or result.output_overflow or result.exit_code != 0:
            raise SandboxError("Docker container creation failed")
        container_id = result.stdout.decode("ascii", "strict").strip()
        if not _CONTAINER_RE.fullmatch(container_id):
            raise SandboxError("Docker returned an invalid container ID")
        self._container_id = container_id

    def _start(self) -> None:
        container_id = self._require_container()
        result = self._docker_call(
            ["start", container_id],
            timeout=_DOCKER_COMMAND_TIMEOUT,
            output_limit=4096,
        )
        if result.timed_out or result.output_overflow or result.exit_code != 0:
            raise SandboxError("Docker container start failed")

    def _inspect_container(self) -> None:
        container_id = self._require_container()
        result = self._docker_call(
            ["inspect", container_id],
            timeout=_DOCKER_COMMAND_TIMEOUT,
            output_limit=_UNTRUSTED_OUTPUT_LIMIT,
        )
        payload = _json_inspect(result, "container inspect")
        if not isinstance(payload, list) or len(payload) != 1:
            raise SandboxError("Docker container inspect returned an unexpected shape")
        _validate_container_inspect(
            payload[0],
            container_id=container_id,
            image=self.image,
            limits=self.limits,
        )

    def _require_container(self) -> str:
        if self._container_id is None:
            raise SandboxError("sandbox has no live container")
        return self._container_id

    def _ensure_open(self) -> str:
        if not self._entered or self._closed or self._poisoned:
            raise SandboxError("sandbox is closed or poisoned")
        return self._require_container()

    def _controller_exec(
        self,
        command: Sequence[str],
        *,
        stdin: BinaryIO | None = None,
        interactive: bool = False,
    ) -> _ProcessResult:
        with self._lock:
            container_id = self._ensure_open()
            timeout = _DOCKER_COMMAND_TIMEOUT
            if self._run_started is not None:
                timeout = min(
                    timeout,
                    float(self.limits.wall) - (time.monotonic() - self._run_started),
                )
                if timeout <= 0:
                    raise SandboxError("wall-time budget exhausted during staging")
            result = self._docker_call(
                _exec_argv(
                    container_id,
                    command,
                    workdir="/",
                    interactive=interactive,
                )[1:],
                timeout=timeout,
                output_limit=64 * 1024,
                stdin=stdin,
            )
            if result.timed_out or result.output_overflow or result.exit_code != 0:
                raise SandboxError("container staging command failed")
            return result

    @staticmethod
    def _manifest_digest(pair: Path) -> str:
        try:
            data = (pair / "manifest.json").read_bytes()
        except OSError as exc:
            raise SandboxError("frozen pair manifest cannot be read") from exc
        return hashlib.sha256(data).hexdigest()

    def _verify_pair(self, pair: Path, expected: str) -> None:
        try:
            materials.verify_pair(pair)
            first = self._manifest_digest(pair)
            materials.verify_pair(pair)
            second = self._manifest_digest(pair)
        except (OSError, materials.MaterialError) as exc:
            raise SandboxError("frozen pair verification failed") from exc
        if first != second or first != expected:
            raise SandboxError("frozen pair manifest hash does not match its anchor")

    def stage(
        self,
        pair: Path,
        arm: Literal["docs", "skills"],
        expected_manifest_sha256: str,
    ) -> None:
        """Transfer one verified arm through a bounded regular-file stdin."""
        with self._lock:
            self._ensure_open()
            if self._staged:
                raise SandboxError("sandbox staging is already complete")
            if not isinstance(arm, str) or arm not in {"docs", "skills"}:
                raise ValueError("arm must be docs or skills")
            expected = _validate_manifest_hash(expected_manifest_sha256)
            pair_path = Path(pair)
            try:
                self._verify_pair(pair_path, expected)
                payload, payload_digest = _stage_payload(pair_path, arm, self.limits.workbytes)
                self._verify_pair(pair_path, expected)
                with tempfile.TemporaryFile(mode="w+b") as input_file:
                    input_file.write(payload)
                    input_file.flush()
                    input_file.seek(0)
                    result = self._controller_exec(
                        [
                            "/usr/bin/python3",
                            "-c",
                            _KERNEL_MOUNTS_SCRIPT + "\n" + _STAGE_SCRIPT,
                            arm,
                            str(self.limits.workbytes),
                        ],
                        stdin=input_file,
                        interactive=True,
                    )
                if result.stdout.decode("ascii", "strict").strip() != payload_digest:
                    raise SandboxError("container staging digest did not match")
                self._verify_pair(pair_path, expected)
            except BaseException as original:
                self._poisoned = True
                try:
                    self._destroy()
                except SandboxError as cleanup:
                    raise cleanup from original
                raise
            self._staged = True

    def run(self, argv: list[str]) -> RunResult:
        """Run one absolute-path command under the remaining total budgets."""
        with self._lock:
            container_id = self._ensure_open()
            if not self._staged:
                raise SandboxError("stage must complete before run")
            _validate_argv(argv)
            if self._toolcalls >= self.limits.toolcalls:
                raise SandboxError("tool-call budget exhausted before dispatch")
            if self._run_started is None:
                raise SandboxError("sandbox clock was not initialized")
            remaining = float(self.limits.wall) - (time.monotonic() - self._run_started)
            if remaining <= 0:
                error = SandboxError("wall-time budget exhausted before dispatch")
                self._poisoned = True
                try:
                    self._destroy()
                except SandboxError as cleanup:
                    raise cleanup from error
                raise error
            self._toolcalls += 1
            try:
                result = self._docker_call(
                    _exec_argv(container_id, argv, workdir="/work/workspace")[1:],
                    timeout=remaining,
                    output_limit=self.limits.outputbytes,
                )
            except BaseException as original:
                self._poisoned = True
                try:
                    self._destroy()
                except SandboxError as cleanup:
                    raise cleanup from original
                raise
            if result.timed_out or result.output_overflow:
                self._poisoned = True
                termination = "timeout" if result.timed_out else "output_limit"
                self._destroy()
                return RunResult(None, result.stdout, result.stderr, termination)
            if result.exit_code is None:
                self._poisoned = True
                self._destroy()
                raise SandboxError("Docker command ended without an exit status")
            termination = "exited" if result.exit_code >= 0 else "signaled"
            return RunResult(result.exit_code, result.stdout, result.stderr, termination)

    def _destroy(self) -> None:
        self._cancel_watchdog()
        container_id = self._container_id
        if container_id is None:
            return
        result = self._docker_call(
            ["rm", "-f", container_id],
            timeout=_CLEANUP_TIMEOUT,
            output_limit=4096,
        )
        if result.timed_out or result.output_overflow or result.exit_code != 0:
            self._poisoned = True
            raise SandboxError("Docker container cleanup failed")
        self._container_id = None


__all__ = ["Limits", "RunResult", "Sandbox", "SandboxError"]
