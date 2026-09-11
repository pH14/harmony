# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run the fixed benign guest checker and validate its endpoint evidence.

The checker receives a compiled artifact and a controller-selected argument
tuple.  Expected outcomes stay in the controller; the guest image contains
only the supplied command and its input arguments.
"""

from __future__ import annotations

import json
import os
import re
import stat
from pathlib import Path

try:
    from . import artifact_run, build, guest_evidence, guest_files, guest_image, qualify_guest, sandbox
except ImportError:  # unittest discovery can load this directory as top-level.
    import artifact_run  # type: ignore[no-redef]
    import build  # type: ignore[no-redef]
    import guest_evidence  # type: ignore[no-redef]
    import guest_files  # type: ignore[no-redef]
    import guest_image  # type: ignore[no-redef]
    import qualify_guest  # type: ignore[no-redef]
    import sandbox  # type: ignore[no-redef]


MAX_ARGUMENTS = 16
MAX_ARGUMENT_BYTES = 20
_HEX_RE = re.compile(r"^[0-9a-f]{64}$")
_INTEGER_RE = re.compile(r"(?:0|-?[1-9][0-9]*)\Z")
_PATH_KEYS = frozenset({"harmony", "kernel", "base", "agent", "preparer"})
_PROGRAM = "/app/app"
_ACTIONS = b'[{"Hook":1},"Wait"]\n'


def _validate_arguments(arguments: object) -> tuple[str, ...]:
    if type(arguments) is not tuple:
        raise ValueError("checker arguments must be a tuple")
    if not 1 <= len(arguments) <= MAX_ARGUMENTS or arguments[0] != "check":
        raise ValueError("checker arguments must start with check and contain at most 16 arguments")
    for index, argument in enumerate(arguments):
        if type(argument) is not str or len(argument) > MAX_ARGUMENT_BYTES:
            raise ValueError("checker arguments must be short strings")
        if index and _INTEGER_RE.fullmatch(argument) is None:
            raise ValueError("checker arguments must be canonical signed decimal integers")
    return arguments


def _validate_paths_and_pins(
    paths: object,
    pins: object,
) -> tuple[dict[str, Path], dict[str, str]]:
    if type(paths) is not dict or set(paths) != _PATH_KEYS:
        raise ValueError("paths must contain exactly the trusted controller inputs")
    if type(pins) is not dict or set(pins) != _PATH_KEYS:
        raise ValueError("pins must contain exactly the trusted controller input digests")
    checked_paths: dict[str, Path] = {}
    checked_pins: dict[str, str] = {}
    for name in sorted(_PATH_KEYS):
        path = paths[name]
        if not isinstance(path, Path):
            raise ValueError(f"paths[{name}] must be a pathlib.Path")
        pin = pins[name]
        if type(pin) is not str or _HEX_RE.fullmatch(pin) is None:
            raise ValueError(f"pins[{name}] must be lowercase hexadecimal")
        checked_paths[name] = path
        checked_pins[name] = pin
    return checked_paths, checked_pins


def _verify_pins(paths: dict[str, Path], pins: dict[str, str]) -> None:
    for name in sorted(_PATH_KEYS):
        try:
            found = qualify_guest._digest(paths[name])
        except (OSError, ValueError) as exc:
            raise ValueError(f"trusted input {name} cannot be read") from exc
        if found != pins[name]:
            raise ValueError(f"trusted input {name} changed")


def _write_bytes(path: Path, data: bytes, mode: int) -> None:
    try:
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        descriptor = os.open(path, flags, mode)
        try:
            offset = 0
            while offset < len(data):
                offset += os.write(descriptor, data[offset:])
        finally:
            os.close(descriptor)
        os.chmod(path, mode)
    except OSError as exc:
        raise ValueError(f"cannot write checker fixture {path.name}") from exc


def _open_case(case: Path) -> int:
    if not isinstance(case, Path):
        raise ValueError("case must be a pathlib.Path")
    try:
        info = os.lstat(case)
    except OSError as exc:
        raise ValueError("checker case must be an existing directory") from exc
    if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode):
        raise ValueError("checker case must be an existing directory")
    try:
        flags = os.O_RDONLY | os.O_DIRECTORY
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        descriptor = os.open(case, flags)
    except OSError as exc:
        raise ValueError("checker case cannot be anchored") from exc
    try:
        if os.listdir(descriptor):
            raise ValueError("checker case must be a fresh empty directory")
    except ValueError:
        os.close(descriptor)
        raise
    except OSError as exc:
        os.close(descriptor)
        raise ValueError("checker case cannot be inspected") from exc
    return descriptor


def _bundle(arguments: tuple[str, ...]) -> bytes:
    return (
        "ready /app/app ready\n"
        "node idle /app/app idle\n"
        "hook 1 /app/app "
        + " ".join(arguments)
        + "\n"
    ).encode("ascii")


def _cli_argv(
    paths: dict[str, Path],
    image: Path,
    actions: Path,
    output: Path,
) -> list[str]:
    return [
        str(paths["harmony"]),
        "search",
        "--package",
        "faults",
        str(image),
        "--kernel",
        str(paths["kernel"]),
        "--base-initramfs",
        str(paths["base"]),
        "--fault-agent",
        str(paths["agent"]),
        "--replay",
        str(actions),
        "--repeat",
        "1",
        "--seed",
        "1",
        "--workers",
        "1",
        "--horizon-ms",
        "1000",
        "--ram-mib",
        "512",
        "--wall-minutes",
        "1",
        "--out",
        str(output),
    ]


def _result(
    *,
    status: str,
    observed_holds: bool | None,
    expected_holds: bool,
    evidence: dict[str, object] | None,
    artifact_sha256: str,
    image_archive_sha256: str,
    prepared_image_sha256: str,
    actions_sha256: str,
) -> dict[str, object]:
    return {
        "status": status,
        "observed_holds": observed_holds,
        "expected_holds": expected_holds,
        "evidence": evidence,
        "artifact_sha256": artifact_sha256,
        "image_archive_sha256": image_archive_sha256,
        "prepared_image_sha256": prepared_image_sha256,
        "actions_sha256": actions_sha256,
    }


def check_artifact(
    artifact: build.Artifact,
    arguments: tuple[str, ...],
    expected_holds: bool,
    paths: dict[str, Path],
    pins: dict[str, str],
    case: Path,
) -> dict[str, object]:
    """Run one checker case and classify the observed guest decision."""

    checked_artifact = artifact_run._validate_artifact(artifact)
    if checked_artifact.path != "app":
        raise ValueError("checker artifact metadata path must be app")
    checked_arguments = _validate_arguments(arguments)
    if type(expected_holds) is not bool:
        raise ValueError("expected_holds must be a boolean")
    if not isinstance(case, Path):
        raise ValueError("case must be a pathlib.Path")
    checked_paths, checked_pins = _validate_paths_and_pins(paths, pins)
    case_fd = _open_case(case)
    try:
        _verify_pins(checked_paths, checked_pins)

        bundle = _bundle(checked_arguments)
        archive = guest_image.package_artifacts((checked_artifact,), bundle)
        image_path = case / "fixture.tar"
        app_path = case / "app.bin"
        actions_path = case / "actions.json"
        output_path = case / "run"
        _write_bytes(app_path, checked_artifact.data, 0o644)
        _write_bytes(image_path, archive, 0o644)
        _write_bytes(actions_path, _ACTIONS, 0o644)
        archive_sha256 = qualify_guest._digest(image_path)
        actions_sha256 = qualify_guest._digest(actions_path)
        input_pins = {
            "archive": archive_sha256,
            "actions": actions_sha256,
            "artifact": checked_artifact.sha256,
        }
        for path in (app_path, image_path, actions_path):
            try:
                os.chmod(path, 0o444)
            except OSError as exc:
                raise ValueError(f"checker fixture {path.name} cannot be made read-only") from exc
        for name, path in (("artifact", app_path), ("archive", image_path), ("actions", actions_path)):
            if qualify_guest._digest(path) != input_pins[name]:
                raise ValueError(f"checker input {name} changed")

        home = case / "home"
        temporary = case / "tmp"
        try:
            home.mkdir()
            temporary.mkdir()
        except OSError as exc:
            raise ValueError("checker case cannot create private home and temporary directories") from exc
        environment = {
            "PATH": "/usr/bin:/bin",
            "HOME": str(home),
            "TMPDIR": str(temporary),
            "XDG_CACHE_HOME": str(home / "cache"),
            "LC_ALL": "C.UTF-8",
        }
        prepared = qualify_guest._prepared_digest(
            checked_paths["preparer"],
            image_path,
            checked_paths["base"],
            checked_paths["agent"],
            environment,
        )
        _verify_pins(checked_paths, checked_pins)
        invocation = _cli_argv(checked_paths, image_path, actions_path, output_path)
        _write_bytes(case / "invocation.json", json.dumps(invocation, indent=2).encode("utf-8"), 0o644)
        outcome = qualify_guest.sandbox._run_bounded(
            invocation,
            timeout=90,
            output_limit=1024**2,
            env=environment,
        )
        _write_bytes(case / "cli.stdout", outcome.stdout, 0o644)
        _write_bytes(case / "cli.stderr", outcome.stderr, 0o644)
        if outcome.exit_code != 0 or outcome.timed_out or outcome.output_overflow:
            detail = outcome.stderr.decode("utf-8", errors="replace").strip()
            raise ValueError("checker CLI did not complete within its bounds" + (f": {detail}" if detail else ""))
        _verify_pins(checked_paths, checked_pins)
        for name, path in (("artifact", app_path), ("archive", image_path), ("actions", actions_path)):
            if qualify_guest._digest(path) != input_pins[name]:
                raise ValueError(f"checker input {name} changed")
        report = guest_files.read_json_at(case_fd, "run/report.json")
        events = guest_files.read_json_at(case_fd, "run/replay-1-events.json")
    finally:
        os.close(case_fd)

    _verify_pins(checked_paths, checked_pins)
    for name, path in (("artifact", app_path), ("archive", image_path), ("actions", actions_path)):
        if qualify_guest._digest(path) != input_pins[name]:
            raise ValueError(f"checker input {name} changed")
    if report.get("image_sha256") != prepared:
        raise ValueError("checker report image does not match the prepared image digest")
    bug_found = report.get("bug_found")
    if type(bug_found) is not bool:
        raise ValueError("checker report bug_found must be a boolean")
    observed_holds = not bug_found
    try:
        evidence = guest_evidence.validate_fixture(
            report,
            events,
            checked_pins["kernel"],
            checked_pins["agent"],
            violation=bug_found,
        )
    except guest_evidence.EvidenceError as error:
        if error.code != "missing_hit":
            raise
        return _result(
            status="no_telemetry",
            observed_holds=None,
            expected_holds=expected_holds,
            evidence=None,
            artifact_sha256=checked_artifact.sha256,
            image_archive_sha256=archive_sha256,
            prepared_image_sha256=prepared,
            actions_sha256=actions_sha256,
        )
    status = "passed" if observed_holds == expected_holds else "mismatch"
    return _result(
        status=status,
        observed_holds=observed_holds,
        expected_holds=expected_holds,
        evidence=evidence,
        artifact_sha256=checked_artifact.sha256,
        image_archive_sha256=archive_sha256,
        prepared_image_sha256=prepared,
        actions_sha256=actions_sha256,
    )


__all__ = ["check_artifact"]
