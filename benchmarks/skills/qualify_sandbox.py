# SPDX-License-Identifier: AGPL-3.0-or-later

"""Qualify the paired-skill Docker boundary with a fixed, no-model canary.

This command is deliberately a boundary check rather than an evaluator.  It
only runs the constant canary below in a disposable :class:`Sandbox`; it never
executes a staged file on the host, calls a model, grades an answer, or claims
that a guest workload was built or booted.  A successful report therefore
means that the requested Linux/Docker observations were made on this host.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import sys
import tempfile
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any

if __package__:
    from . import materials, sandbox
else:  # Allow ``python benchmarks/skills/qualify_sandbox.py`` as well.
    sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
    from benchmarks.skills import materials, sandbox


_DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
_HEX_RE = re.compile(r"^[0-9a-f]{64}$")
_CONTAINER_ID_RE = re.compile(r"^[0-9a-f]{64}$")
_REPORT_FORMAT = "harmony-skill-sandbox-qualification-v1"
_CLEANUP_ALLOWANCE = float(getattr(sandbox, "_CLEANUP_TIMEOUT", 5.0))
_CANARY_SCRIPT = r'''
import hashlib
import os
from pathlib import Path
import re
import socket
import subprocess
import sys


def fail(message):
    print("canary: " + message, file=sys.stderr)
    raise SystemExit(1)


(
    arm,
    host_private,
    secret_name,
    common_path,
    common_hash,
    treatment_path,
    treatment_hash,
    expected_memory,
    expected_work,
    expected_pids,
    expected_cpu,
    treatment_mode,
    execute_treatment,
) = sys.argv[1:]
root = Path.cwd()

if Path(host_private).exists():
    fail("host-private canary is visible")
expected_environment = {
    "PATH": "/usr/bin:/bin",
    "HOME": "/work/home",
    "XDG_CONFIG_HOME": "/work/config",
    "XDG_CACHE_HOME": "/tmp/cache",
    "TMPDIR": "/tmp",
    "LC_ALL": "C.UTF-8",
}
if secret_name in os.environ or "DOCKER_CONFIG" in os.environ:
    fail("host environment leaked")
if dict(os.environ) != expected_environment:
    fail("fresh environment boundary mismatch")
if os.getuid() != 65534 or os.getgid() != 65534:
    fail("process is not the requested nonroot identity")

status = Path("/proc/self/status").read_text(encoding="ascii")
no_new_privs = re.search(r"^NoNewPrivs:\s+(\d+)$", status, re.MULTILINE)
cap_eff = re.search(r"^CapEff:\s+([0-9a-fA-F]+)$", status, re.MULTILINE)
if no_new_privs is None or no_new_privs.group(1) != "1":
    fail("no-new-privileges is not active")
if cap_eff is None or int(cap_eff.group(1), 16) != 0:
    fail("effective capabilities are not empty")

mountinfo = Path("/proc/self/mountinfo").read_text(encoding="ascii")


def mount_options(destination):
    for line in mountinfo.splitlines():
        fields = line.split()
        if len(fields) > 5 and fields[4] == destination:
            return fields[5].split(",")
    fail("mount is absent: " + destination)


root_options = mount_options("/")
if "ro" not in root_options:
    fail("root mount is writable")
work_options = mount_options("/work")
tmp_options = mount_options("/tmp")
if "rw" not in work_options or "exec" not in work_options:
    fail("work tmpfs is not writable and executable")
if "rw" not in tmp_options or "noexec" not in tmp_options:
    fail("tmp tmpfs is executable or read-only")


def option_size(options):
    for option in options:
        if option.startswith("size="):
            value = option[5:]
            if not value:
                continue
            units = {"k": 1024, "m": 1024**2, "g": 1024**3, "t": 1024**4}
            multiplier = units.get(value[-1].lower(), 1)
            number = value[:-1] if multiplier != 1 else value
            try:
                return int(number) * multiplier
            except ValueError:
                break
    fail("tmpfs size is unavailable")


if option_size(work_options) != int(expected_work) or option_size(tmp_options) != int(expected_work):
    fail("tmpfs size does not match the requested work limit")
probe = Path("/.harmony-skill-readonly-canary")
try:
    probe.write_bytes(b"must fail")
except OSError:
    pass
else:
    try:
        probe.unlink()
    except OSError:
        pass
    fail("writable root accepted a write")

if (
    Path("/run/docker.sock").exists()
    or Path("/var/run/docker.sock").exists()
    or Path("/dev/kvm").exists()
):
    fail("host control socket or KVM device is visible")


def optional_cgroup_value(*candidates):
    for candidate in candidates:
        path = Path(candidate)
        try:
            return path.read_text(encoding="ascii").strip()
        except OSError:
            continue
    return None


def cgroup_value(*candidates):
    value = optional_cgroup_value(*candidates)
    if value is not None:
        return value
    fail("requested cgroup limit is unavailable")


if cgroup_value("/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory/memory.limit_in_bytes") != expected_memory:
    fail("memory cgroup limit does not match")
if cgroup_value("/sys/fs/cgroup/pids.max", "/sys/fs/cgroup/pids/pids.max") != expected_pids:
    fail("pids cgroup limit does not match")
cpu_limit = optional_cgroup_value("/sys/fs/cgroup/cpu.max")
if cpu_limit is None:
    cpu_limit = " ".join(
        (
            cgroup_value("/sys/fs/cgroup/cpu/cpu.cfs_quota_us"),
            cgroup_value("/sys/fs/cgroup/cpu/cpu.cfs_period_us"),
        )
    )
cpu_parts = cpu_limit.split()
if not cpu_parts or cpu_parts[0] in {"max", "-1"}:
    fail("cpu cgroup limit is unbounded")
try:
    cpu_quota, cpu_period = (int(item) for item in cpu_parts)
    expected_cpu_nano = int(float(expected_cpu) * 1_000_000)
except (TypeError, ValueError):
    fail("cpu cgroup limit is malformed")
if (
    len(cpu_parts) != 2
    or expected_cpu_nano <= 0
    or cpu_quota <= 0
    or cpu_period <= 0
    or cpu_quota * 1_000_000 != cpu_period * expected_cpu_nano
):
    fail("cpu cgroup limit does not match")

try:
    interfaces = {name for _, name in socket.if_nameindex()}
except OSError as error:
    fail("could not inspect network interfaces: " + str(error))
if interfaces != {"lo"}:
    fail("network namespace has non-loopback interfaces")
probe_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
probe_socket.settimeout(0.25)
try:
    probe_socket.connect(("203.0.113.1", 80))
except OSError:
    pass
else:
    fail("network connection unexpectedly succeeded")
finally:
    probe_socket.close()

def check_hash(relative, expected):
    path = root / relative
    if not path.is_file():
        fail("expected staged file is absent: " + relative)
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual != expected:
        fail("staged file hash changed: " + relative)


if common_path:
    check_hash(common_path, common_hash)
treatment = root / treatment_path
if arm == "docs":
    if (root / ".agent" / "skills").exists() or treatment.exists():
        fail("docs arm contains treatment material")
elif arm == "skills":
    if not (root / ".agent" / "skills").is_dir():
        fail("skills arm has no treatment directory")
    check_hash(treatment_path, treatment_hash)
    try:
        actual_mode = (treatment.stat().st_mode & 0o111)
        wanted_mode = int(treatment_mode, 10)
    except (OSError, ValueError):
        fail("treatment executable mode is malformed")
    if actual_mode != wanted_mode:
        fail("staged treatment executable mode changed")
    if execute_treatment == "1":
        if treatment.read_bytes() != b"#!/bin/sh\nexit 0\n":
            fail("unexpected executable witness contents")
        completed = subprocess.run(
            [str(treatment)],
            cwd=str(root),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if completed.returncode != 0:
            fail("executable staged witness did not run")
else:
    fail("unknown arm")
if not (root / "TASK.txt").is_file() or not (root / "SETTINGS.json").is_file():
    fail("staged metadata is incomplete")
'''


class QualificationError(RuntimeError):
    """A canary or input failed qualification."""


class UnsupportedEnvironment(QualificationError):
    """The host cannot make the requested Docker observation."""


class CanaryFailure(QualificationError):
    """A bounded in-container canary returned an unexpected result."""


@dataclass(frozen=True)
class PairSpec:
    path: Path
    manifest_sha256: str
    common_path: str
    common_sha256: str
    treatment_path: str
    treatment_sha256: str
    treatment_mode: int = 0
    execute_treatment: bool = False


def _digest(value: object, *, image: bool = False) -> str:
    if not isinstance(value, str):
        raise ValueError("digest must be text")
    pattern = _DIGEST_RE if image else _HEX_RE
    if not pattern.fullmatch(value):
        expected = "sha256:<64 lowercase hex>" if image else "64 lowercase hex"
        raise ValueError(f"expected {expected}")
    return value


def _host_env() -> dict[str, str]:
    # The Docker client must not inherit credentials, Docker configuration,
    # proxy settings, or arbitrary caller variables.  A nonexistent config
    # home prevents the CLI from falling back to the invoking user's files.
    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": "/nonexistent",
        "DOCKER_CONFIG": "/nonexistent",
        "DOCKER_HOST": "unix:///var/run/docker.sock",
        "DOCKER_CONTEXT": "default",
    }


def _docker_call(
    args: list[str], *, timeout: float = 30.0, output_limit: int = 256 * 1024
) -> Any:
    runner = getattr(sandbox, "_run_bounded", None)
    if runner is None:
        raise UnsupportedEnvironment("sandbox process runner is unavailable")
    try:
        return runner(
            ["docker", *args],
            timeout=timeout,
            output_limit=output_limit,
            env=_host_env(),
        )
    except sandbox.SandboxError as error:
        raise UnsupportedEnvironment(str(error)) from error


def _docker_json(args: list[str], what: str) -> Any:
    result = _docker_call(args)
    if result.timed_out or result.output_overflow or result.exit_code != 0:
        raise UnsupportedEnvironment(f"Docker {what} failed")
    try:
        return json.loads(result.stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise UnsupportedEnvironment(f"Docker {what} returned invalid JSON") from error


def _require_linux_daemon() -> None:
    result = _docker_call(
        ["version", "--format", "{{.Server.Os}}"],
        timeout=30.0,
        output_limit=4096,
    )
    if result.timed_out or result.output_overflow or result.exit_code != 0:
        raise UnsupportedEnvironment("Docker Linux daemon is unavailable")
    try:
        server_os = result.stdout.decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise UnsupportedEnvironment("Docker daemon identity is invalid") from error
    if server_os != "linux":
        raise UnsupportedEnvironment("Docker daemon is not Linux")


def _inspect_image(image: str, *, expected_id: str | None = None) -> dict[str, Any]:
    payload = _docker_json(["image", "inspect", image], "image inspection")
    if not isinstance(payload, list) or len(payload) != 1 or not isinstance(payload[0], dict):
        raise UnsupportedEnvironment("Docker image inspection returned an unexpected shape")
    info = payload[0]
    image_id = info.get("Id")
    if not isinstance(image_id, str) or not _DIGEST_RE.fullmatch(image_id):
        raise UnsupportedEnvironment("Docker image inspection returned a non-immutable ID")
    if expected_id is not None and image_id != expected_id:
        raise UnsupportedEnvironment("Docker image identity changed")
    try:
        sandbox._validate_image_inspect(info, image_id)
    except sandbox.SandboxError as error:
        raise UnsupportedEnvironment(str(error)) from error
    return info


def _build_from_local_base(base: str) -> tuple[str, str]:
    """Build a no-RUN image from one already-local immutable base image."""
    base = _digest(base, image=True)
    info = _inspect_image(base, expected_id=base)
    config = info.get("Config")
    if isinstance(config, dict) and config.get("OnBuild") not in (None, []):
        raise UnsupportedEnvironment("base image has ONBUILD instructions")

    tag = f"harmony-skill-qualify-{uuid.uuid4().hex}:local"
    tagged = False
    built: str | None = None
    try:
        result = _docker_call(["tag", base, tag], output_limit=4096)
        if result.exit_code != 0 or result.timed_out or result.output_overflow:
            raise UnsupportedEnvironment("could not tag the supplied local base image")
        tagged = True
        with tempfile.TemporaryDirectory(prefix="harmony-skill-dockerfile-") as context:
            context_path = Path(context)
            # The context is created by this trusted script and contains no
            # caller files.  The Dockerfile has no RUN/COPY/ADD instruction:
            # Python and shell availability are properties of the supplied base.
            (context_path / "Dockerfile").write_text(f"FROM {tag}\n", encoding="ascii")
            (context_path / ".dockerignore").write_text("*\n!Dockerfile\n", encoding="ascii")
            result = _docker_call(
                [
                    "build",
                    "--pull=false",
                    "--network=none",
                    "--quiet",
                    "--file",
                    str(context_path / "Dockerfile"),
                    "--tag",
                    tag,
                    str(context_path),
                ],
                timeout=120.0,
            )
            if result.exit_code != 0 or result.timed_out or result.output_overflow:
                raise UnsupportedEnvironment("local Dockerfile image build failed")
            built_info = _inspect_image(tag)
            built = built_info.get("Id")
            if not isinstance(built, str) or not _DIGEST_RE.fullmatch(built):
                raise UnsupportedEnvironment("Docker build did not return an immutable image ID")
    finally:
        if tagged:
            cleanup = _docker_call(["image", "rm", "--no-prune", tag], output_limit=4096)
            if (
                built is not None
                and (cleanup.exit_code != 0 or cleanup.timed_out or cleanup.output_overflow)
            ):
                raise UnsupportedEnvironment("temporary Docker image tag cleanup failed")
    assert built is not None
    return built, "local Dockerfile from supplied base"


def _manifest_file(manifest: dict[str, Any], field: str) -> tuple[str, str]:
    records = manifest.get(field)
    if not isinstance(records, list) or not records:
        raise QualificationError(f"frozen pair has no {field} file")
    record = records[0]
    if not isinstance(record, dict):
        raise QualificationError(f"frozen pair {field} record is malformed")
    path, digest = record.get("path"), record.get("sha256")
    if not isinstance(path, str) or not isinstance(digest, str):
        raise QualificationError(f"frozen pair {field} record is malformed")
    return path, _digest(digest)


def _manifest_mode(manifest: dict[str, Any], field: str) -> int:
    records = manifest.get(field)
    if not isinstance(records, list) or not records:
        raise QualificationError(f"frozen pair has no {field} file")
    record = records[0]
    mode = record.get("mode") if isinstance(record, dict) else None
    if type(mode) is not int or mode < 0 or mode & ~0o111:
        raise QualificationError(f"frozen pair {field} mode is malformed")
    return mode


def _pair_from_path(path: Path, expected: str) -> PairSpec:
    try:
        manifest = materials.verify_pair(path)
        actual = hashlib.sha256((path / "manifest.json").read_bytes()).hexdigest()
    except (OSError, materials.MaterialError) as error:
        raise QualificationError("frozen pair verification failed") from error
    expected = _digest(expected)
    if actual != expected:
        raise QualificationError("frozen pair manifest hash does not match its anchor")
    common_path, common_hash = _manifest_file(manifest, "common")
    treatment_path, treatment_hash = _manifest_file(manifest, "treatment")
    treatment_mode = _manifest_mode(manifest, "treatment")
    return PairSpec(
        path,
        expected,
        common_path,
        common_hash,
        treatment_path,
        treatment_hash,
        treatment_mode,
    )


def _make_canary_pair(root: Path) -> PairSpec:
    root.mkdir(parents=True)
    sources = root / "sources"
    sources.mkdir()
    common = sources / "common.txt"
    treatment = sources / "treatment.txt"
    common.write_bytes(b"common canary material\n")
    treatment.write_bytes(b"#!/bin/sh\nexit 0\n")
    os.chmod(treatment, 0o755)
    pair = root / "pair"
    materials.freeze_pair(
        pair,
        {"common.txt": common},
        {"rules/treatment.txt": treatment},
        "A fixed Docker boundary canary; do not grade this prompt.",
        {"canary": "sandbox", "version": 1},
    )
    expected = hashlib.sha256((pair / "manifest.json").read_bytes()).hexdigest()
    frozen = _pair_from_path(pair, expected)
    return PairSpec(
        frozen.path,
        frozen.manifest_sha256,
        frozen.common_path,
        frozen.common_sha256,
        frozen.treatment_path,
        frozen.treatment_sha256,
        frozen.treatment_mode,
        True,
    )


def _limits(
    *, wall: float = 5.0, toolcalls: int = 4, outputbytes: int = 64 * 1024
) -> Any:
    return sandbox.Limits(
        wall=wall,
        toolcalls=toolcalls,
        memorybytes=128 * 1024 * 1024,
        workbytes=32 * 1024 * 1024,
        cpus=0.5,
        pids=32,
        outputbytes=outputbytes,
    )


def _canary_argv(pair: PairSpec, arm: str, host_private: Path, secret_name: str) -> list[str]:
    limits = _limits()
    return [
        *_python_command(
            _CANARY_SCRIPT,
            [
                arm,
                str(host_private),
                secret_name,
                pair.common_path,
                pair.common_sha256,
                pair.treatment_path,
                pair.treatment_sha256,
                str(limits.memorybytes),
                str(limits.workbytes),
                str(limits.pids),
                str(limits.cpus),
                str(pair.treatment_mode),
                "1" if pair.execute_treatment else "0",
            ],
        ),
    ]


def _python_command(script: str, arguments: list[str] | None = None) -> list[str]:
    # Sandbox.stage uses this same interpreter path.  Keeping the canary as a
    # direct argv also avoids a shell adding PWD/SHLVL/_ to the fresh env.
    # ``-S`` prevents a staged sitecustomize.py from being imported.
    return [
        "/usr/bin/python3",
        "-S",
        "-c",
        script,
        *(arguments or []),
    ]


_SUCCESS_COMMAND = ["/bin/sh", "-c", "exit 0"]


def _run_canary(
    image: str, pair: PairSpec, arm: str, host_private: Path, secret_name: str
) -> None:
    with sandbox.Sandbox(image, _limits()) as isolated:
        isolated.stage(pair.path, arm, pair.manifest_sha256)
        result = isolated.run(_canary_argv(pair, arm, host_private, secret_name))
        if result.termination != "exited" or result.exit_code != 0:
            detail = result.stderr[:256].decode("utf-8", "replace")
            raise CanaryFailure(f"{arm} canary failed: {detail}")


def _capture_container_id(isolated: Any) -> str:
    container_id = getattr(isolated, "_container_id", None)
    if not isinstance(container_id, str) or not _CONTAINER_ID_RE.fullmatch(container_id):
        raise CanaryFailure("sandbox did not expose a valid container identity")
    return container_id


def _assert_container_gone(container_id: str) -> None:
    result = _docker_call(
        [
            "container",
            "ls",
            "--all",
            "--no-trunc",
            "--filter",
            f"id={container_id}",
            "--format",
            "{{.ID}}",
        ],
        timeout=_CLEANUP_ALLOWANCE,
        output_limit=4096,
    )
    if result.timed_out or result.output_overflow or result.exit_code != 0:
        raise UnsupportedEnvironment("Docker container cleanup could not be observed")
    try:
        remaining = result.stdout.decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise UnsupportedEnvironment("Docker container cleanup returned invalid output") from error
    if remaining:
        raise CanaryFailure("bounded run left its container behind")


def _run_expect_budget(image: str, pair: PairSpec) -> None:
    with sandbox.Sandbox(image, _limits(toolcalls=2)) as isolated:
        isolated.stage(pair.path, "docs", pair.manifest_sha256)
        for _ in range(2):
            result = isolated.run(_SUCCESS_COMMAND)
            if result.termination != "exited" or result.exit_code != 0:
                raise CanaryFailure("budget setup command failed")
        try:
            isolated.run(_SUCCESS_COMMAND)
        except sandbox.SandboxError as error:
            if "tool-call budget exhausted" not in str(error):
                raise CanaryFailure("tool budget rejected the wrong condition") from error
        else:
            raise CanaryFailure("tool budget allowed a call after exhaustion")


def _run_expect_output_limit(image: str, pair: PairSpec) -> None:
    # Leave one call available so the follow-up proves destruction rather than
    # merely observing the tool-call budget.
    limits = _limits(wall=5.0, toolcalls=2, outputbytes=256)
    with sandbox.Sandbox(image, limits) as isolated:
        isolated.stage(pair.path, "docs", pair.manifest_sha256)
        container_id = _capture_container_id(isolated)
        started = time.monotonic()
        result = isolated.run(
            _python_command("import sys; sys.stdout.write('x' * 1048576)")
        )
        elapsed = time.monotonic() - started
        if result.termination != "output_limit" or result.exit_code is not None:
            raise CanaryFailure("output limit did not terminate the overflowing call")
        if len(result.stdout) + len(result.stderr) > 256:
            raise CanaryFailure("output limit returned more bytes than configured")
        if elapsed > limits.wall + _CLEANUP_ALLOWANCE + 0.5:
            raise CanaryFailure("output limit exceeded its wall bound")
        _assert_container_gone(container_id)
        try:
            isolated.run(_SUCCESS_COMMAND)
        except sandbox.SandboxError as error:
            if "closed or poisoned" not in str(error):
                raise CanaryFailure("output-limit follow-up hit an unrelated error") from error
            return
        raise CanaryFailure("output-limit container remained usable")


def _run_expect_timeout(image: str, pair: PairSpec) -> None:
    # Leave one call available so the follow-up proves destruction rather than
    # merely observing the tool-call budget.
    limits = _limits(wall=2.0, toolcalls=2, outputbytes=256)
    with sandbox.Sandbox(image, limits) as isolated:
        isolated.stage(pair.path, "docs", pair.manifest_sha256)
        container_id = _capture_container_id(isolated)
        started = time.monotonic()
        result = isolated.run(_python_command("import time; time.sleep(10)"))
        elapsed = time.monotonic() - started
        if result.termination != "timeout" or result.exit_code is not None:
            raise CanaryFailure("wall limit did not terminate the sleeping call")
        if elapsed < 0.25 or elapsed > limits.wall + _CLEANUP_ALLOWANCE + 0.5:
            raise CanaryFailure("wall timeout was outside its measured bound")
        _assert_container_gone(container_id)
        try:
            isolated.run(_SUCCESS_COMMAND)
        except sandbox.SandboxError as error:
            if "closed or poisoned" not in str(error):
                raise CanaryFailure("timeout follow-up hit an unrelated error") from error
            return
        raise CanaryFailure("timeout container remained usable")


def _run_expect_idle_watchdog(image: str, pair: PairSpec) -> None:
    """Observe the total wall watchdog destroying a container left idle."""
    limits = _limits(wall=2.0, toolcalls=1)
    isolated = sandbox.Sandbox(image, limits)
    entered = False
    try:
        isolated.__enter__()
        entered = True
        isolated.stage(pair.path, "docs", pair.manifest_sha256)
        container_id = _capture_container_id(isolated)
        started = time.monotonic()
        time.sleep(limits.wall + 0.25)
        try:
            isolated.run(_SUCCESS_COMMAND)
        except sandbox.SandboxError as error:
            if "closed or poisoned" not in str(error):
                raise CanaryFailure("idle watchdog follow-up hit an unrelated error") from error
        else:
            raise CanaryFailure("idle watchdog left the container usable")
        if time.monotonic() - started > limits.wall + _CLEANUP_ALLOWANCE + 1.0:
            raise CanaryFailure("idle watchdog exceeded its measured wall bound")
        _assert_container_gone(container_id)
    finally:
        if entered:
            try:
                isolated.__exit__(None, None, None)
            except sandbox.SandboxError as error:
                if "wall-time budget expired while idle" not in str(error):
                    raise


def _tampered_pair(root: Path, pair: PairSpec) -> Path:
    target = root / "tampered-pair"
    shutil.copytree(pair.path, target)
    task = target / "docs" / "TASK.txt"
    os.chmod(task, 0o644)
    task.write_bytes(task.read_bytes() + b"tampered\n")
    return target


def _run_expect_tamper_rejection(image: str, pair: PairSpec, tampered: Path) -> None:
    try:
        materials.verify_pair(tampered)
    except materials.MaterialError:
        pass
    else:
        raise CanaryFailure("tampered frozen material was not invalid before staging")
    with sandbox.Sandbox(image, _limits(toolcalls=1)) as isolated:
        try:
            isolated.stage(tampered, "docs", pair.manifest_sha256)
        except sandbox.SandboxError as error:
            if "frozen pair verification failed" not in str(error):
                raise CanaryFailure(
                    "tampered material reached an unrelated staging failure"
                ) from error
            return
        raise CanaryFailure("tampered frozen material was accepted")


def _check(name: str, fn: Any, checks: dict[str, Any], failures: list[str]) -> None:
    try:
        fn()
    except UnsupportedEnvironment:
        raise
    except sandbox.SandboxError as error:
        raise UnsupportedEnvironment(str(error)) from error
    except (CanaryFailure, QualificationError) as error:
        checks[name] = {
            "passed": False,
            "evidence": "observed Docker run failed",
            "detail": str(error),
        }
        failures.append(name)
    else:
        checks[name] = {
            "passed": True,
            "evidence": "observed in a disposable Linux container",
        }


def _base_report() -> dict[str, Any]:
    return {
        "format": _REPORT_FORMAT,
        "qualified": False,
        "status": "unqualified",
        "observed": False,
        "checks": {},
        "claims": ["bounded paired-material Docker isolation canary only"],
        "not_claimed": ["model grading", "source build", "guest boot", "workload correctness"],
        "model_calls": 0,
    }


def qualify(args: argparse.Namespace) -> dict[str, Any]:
    report = _base_report()
    if platform.system() != "Linux":
        report["reason"] = "unsupported host: a Linux Docker daemon is required"
        report["unsupported"] = True
        return report

    image: str
    temporary_image = False
    try:
        _require_linux_daemon()
        requested_image = args.image or args.base_image
        _digest(requested_image, image=True)
        if args.base_image:
            image, source = _build_from_local_base(args.base_image)
            temporary_image = True
        else:
            image, source = args.image, "supplied immutable image ID"
        report["image"] = image
        report["image_source"] = source

        with tempfile.TemporaryDirectory(prefix="harmony-skill-qualification-") as temporary:
            temporary_root = Path(temporary)
            if args.pair is None:
                pair = _make_canary_pair(temporary_root / "materials")
            else:
                pair = _pair_from_path(Path(args.pair), args.manifest_sha256)
            host_private = temporary_root / "host-private-canary"
            host_private.mkdir()
            (host_private / "secret.txt").write_text("host-only canary\n", encoding="ascii")
            (host_private / "config.json").write_text('{"host_only":true}\n', encoding="ascii")
            secret_name = f"HARMONY_HOST_ONLY_{uuid.uuid4().hex.upper()}"
            previous = os.environ.get(secret_name)
            os.environ[secret_name] = "host-secret-never-forwarded"  # pragma: allowlist secret
            checks: dict[str, Any] = report["checks"]
            failures: list[str] = []
            try:
                _check(
                    "docs_arm_staging_and_isolation",
                    lambda: _run_canary(image, pair, "docs", host_private, secret_name),
                    checks,
                    failures,
                )
                _check(
                    "skills_arm_staging_and_isolation",
                    lambda: _run_canary(image, pair, "skills", host_private, secret_name),
                    checks,
                    failures,
                )
                if not failures:
                    checks["docker_boundary_inspection"] = {
                        "passed": True,
                        "evidence": "Sandbox verified realized Docker inspect fields for each positive container",
                    }
                    _check(
                        "frozen_material_tampering_rejected",
                        lambda: _run_expect_tamper_rejection(
                            image, pair, _tampered_pair(temporary_root, pair)
                        ),
                        checks,
                        failures,
                    )
                    _check(
                        "tool_call_budget_exhaustion",
                        lambda: _run_expect_budget(image, pair),
                        checks,
                        failures,
                    )
                    _check(
                        "output_overflow_is_bounded_and_destroyed",
                        lambda: _run_expect_output_limit(image, pair),
                        checks,
                        failures,
                    )
                    _check(
                        "wall_timeout_is_bounded_and_destroyed",
                        lambda: _run_expect_timeout(image, pair),
                        checks,
                        failures,
                    )
                    _check(
                        "idle_wall_watchdog_destroys_container",
                        lambda: _run_expect_idle_watchdog(image, pair),
                        checks,
                        failures,
                    )
            finally:
                if previous is None:
                    os.environ.pop(secret_name, None)
                else:
                    os.environ[secret_name] = previous
            report["observed"] = True
            report["manifest_sha256"] = pair.manifest_sha256
            if failures:
                report["status"] = "failed"
                report["failure_checks"] = failures
            else:
                report["qualified"] = True
                report["status"] = "qualified"
    except UnsupportedEnvironment as error:
        report["reason"] = str(error)
        report["unsupported"] = True
    except (OSError, ValueError, QualificationError, sandbox.SandboxError) as error:
        report["reason"] = str(error)
        report["unsupported"] = False
        report["status"] = "failed"
    finally:
        # A Dockerfile containing only FROM may resolve to the very same image
        # ID as the caller's base.  Never remove that caller-owned image.
        if (
            temporary_image
            and "image" in report
            and report["image"] != args.base_image
        ):
            try:
                cleanup = _docker_call(
                    ["image", "rm", "--no-prune", report["image"]], output_limit=4096
                )
            except (OSError, QualificationError, sandbox.SandboxError):
                report["cleanup_warning"] = "temporary image cleanup failed"
            else:
                if cleanup.exit_code != 0 or cleanup.timed_out or cleanup.output_overflow:
                    report["cleanup_warning"] = "temporary image cleanup failed"
    return report


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    images = parser.add_mutually_exclusive_group(required=True)
    images.add_argument("--image", help="existing local sha256:<64 lowercase hex> image ID")
    images.add_argument(
        "--base-image",
        help="local immutable sha256 image ID used as the only FROM in a temporary Dockerfile",
    )
    parser.add_argument("--pair", type=Path, help="an existing verified frozen pair")
    parser.add_argument(
        "--manifest-sha256",
        help="64-hex manifest anchor required with --pair",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    if (args.pair is None) != (args.manifest_sha256 is None):
        parser.error("--pair and --manifest-sha256 must be supplied together")
    report = qualify(args)
    json.dump(report, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0 if report["qualified"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
