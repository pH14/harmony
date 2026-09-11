#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Qualify durable CLI journal publication across a killed process.

This diagnostic deliberately exercises a process interruption at the journal's
exclusive hard-link publication boundary.  It does not add product hooks and
does not claim a power-loss or KVM correctness result.  Each phase runs in a
private copy of an already-populated workspace; the original workspace is
checked for byte-identical committed journal records after every operation.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import secrets
import select
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from typing import Any, TypedDict


JOURNAL_FORMAT = "harmony-workspace-v1"
JOURNAL_NAME = re.compile(r"^[0-9]{6}\.json$")
INTERCEPT_SOURCE = Path(__file__).with_suffix(".c")
ACK_PREFIX = b"HARMONY_JOURNAL_INTERCEPT:"
PROCESS_TIMEOUT_SECONDS = 900
PROCESS_CLEANUP_TIMEOUT_SECONDS = 5
ACK_READ_TIMEOUT_SECONDS = 5
COMMAND_ARGV = ["/opt/harmony/busybox", "sh", "-c", "exit 37"]


class _ValidatorFixture(TypedDict):
    transaction: Path
    point: dict[str, Any]
    command: dict[str, Any]
    blob: Path
    digest: str


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def _string_env(values: dict[str, Any]) -> dict[str, str]:
    return {str(key): str(value) for key, value in values.items()}


def _source_tree_snapshot(root: Path) -> dict[str, tuple[str, int, str, int]]:
    """Hash source files, retain modes, and reject aliases before cloning it."""
    require(root.is_dir(), f"source tree is not a directory: {root}")
    snapshot: dict[str, tuple[str, int, str, int]] = {}

    def fail(error: OSError) -> None:
        raise AssertionError(f"cannot enumerate source tree {root}: {error}") from error

    for current, directories, files in os.walk(root, followlinks=False, onerror=fail):
        names = sorted(set(directories) | set(files))
        for name in names:
            path = Path(current) / name
            relative = path.relative_to(root).as_posix()
            try:
                metadata = path.lstat()
            except OSError as error:
                raise AssertionError(f"cannot inspect source path {path}: {error}") from error
            if stat.S_ISLNK(metadata.st_mode):
                raise AssertionError(f"source workspace contains a symlink: {path}")
            if stat.S_ISDIR(metadata.st_mode):
                snapshot[relative] = ("directory", 0, "", stat.S_IMODE(metadata.st_mode))
                continue
            require(stat.S_ISREG(metadata.st_mode),
                    f"source workspace contains a non-regular path: {path}")
            digest = hashlib.sha256()
            try:
                with path.open("rb") as stream:
                    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                        digest.update(chunk)
            except OSError as error:
                raise AssertionError(f"cannot hash source file {path}: {error}") from error
            snapshot[relative] = (
                "file", metadata.st_size, digest.hexdigest(), stat.S_IMODE(metadata.st_mode)
            )
    return snapshot


def _journal_files(root: Path) -> list[Path]:
    journal = root / "journal"
    require(journal.is_dir(), f"workspace has no journal directory: {journal}")
    files = sorted(path for path in journal.iterdir() if JOURNAL_NAME.fullmatch(path.name))
    require(files, f"workspace has no committed journal transactions: {journal}")
    expected = list(range(1, len(files) + 1))
    actual = [int(path.stem) for path in files]
    require(actual == expected, f"journal sequences are not contiguous: {actual}")
    return files


def _transaction(path: Path, expected_sequence: int | None = None) -> dict[str, Any]:
    try:
        document = json.loads(path.read_text())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AssertionError(f"cannot decode journal transaction {path}: {error}") from error
    require(isinstance(document, dict), f"journal transaction {path} is not an object")
    require(document.get("format") == JOURNAL_FORMAT, f"wrong journal format in {path}")
    sequence = document.get("sequence")
    require(type(sequence) is int and sequence > 0, f"invalid journal sequence in {path}")
    if expected_sequence is not None:
        require(sequence == expected_sequence,
                f"journal {path} carries sequence {sequence}, expected {expected_sequence}")
    records = document.get("records")
    require(isinstance(records, list) and records, f"journal {path} has no records")
    require(all(isinstance(record, dict) and isinstance(record.get("record"), str)
                for record in records), f"journal {path} has malformed records")
    return document


def _committed_journal(root: Path) -> dict[str, bytes]:
    result = {}
    for path in _journal_files(root):
        _transaction(path, int(path.stem))
        result[path.name] = path.read_bytes()
    return result


def _next_transaction(root: Path) -> tuple[int, str]:
    files = _journal_files(root)
    sequence = int(files[-1].stem) + 1
    require(sequence <= 999999, "journal sequence no longer fits the six-digit filename")
    return sequence, f"{sequence:06}.json"


def _records(transaction: dict[str, Any], kind: str) -> list[dict[str, Any]]:
    return [record for record in transaction["records"] if record.get("record") == kind]


def _source_branch(root: Path, branch: str) -> dict[str, Any]:
    current = None
    moments: set[str] = set()
    for path in _journal_files(root):
        transaction = _transaction(path, int(path.stem))
        for record in transaction["records"]:
            if record.get("record") == "moment":
                identifier = record.get("id")
                if isinstance(identifier, str):
                    moments.add(identifier)
            if record.get("record") == "branch" and record.get("name") == branch:
                current = record
    require(current is not None, f"source workspace has no branch {branch!r}")
    require(isinstance(current.get("head"), str) and current["head"] in moments,
            f"source branch {branch!r} has no retained head")
    return current


def _compile_interceptor(destination: Path) -> Path:
    require(platform.system() == "Linux", "journal interruption qualification is Linux-only")
    require(INTERCEPT_SOURCE.is_file(), f"missing interposer source: {INTERCEPT_SOURCE}")
    output = destination / "harmony-journal-intercept.so"
    completed = subprocess.run(
        ["cc", "-shared", "-fPIC", "-o", str(output), str(INTERCEPT_SOURCE), "-ldl"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    require(completed.returncode == 0,
            f"cannot compile journal interposer ({completed.returncode}): {completed.stderr}")
    require(output.is_file(), "journal interposer compiler returned without an output")
    return output


def _preload_environment(base: dict[str, str], interceptor: Path, phase: str,
                         final_path: Path, ack_fd: int) -> dict[str, str]:
    child = dict(base)
    existing = child.get("LD_PRELOAD")
    child["LD_PRELOAD"] = str(interceptor) if not existing else f"{interceptor}:{existing}"
    child["HARMONY_JOURNAL_FINAL"] = str(final_path.resolve())
    child["HARMONY_JOURNAL_PHASE"] = phase
    child["HARMONY_JOURNAL_ACK_FD"] = str(ack_fd)
    return child


def _clean_environment(base: dict[str, str]) -> dict[str, str]:
    child = dict(base)
    child.pop("LD_PRELOAD", None)
    for key in ("HARMONY_JOURNAL_FINAL", "HARMONY_JOURNAL_PHASE",
                "HARMONY_JOURNAL_ACK_FD"):
        child.pop(key, None)
    return child


def _artifact_args(environment: dict[str, str]) -> list[str]:
    # The shipping CLI officially accepts HARMONY_GUEST_DIR and
    # HARMONY_FAULT_AGENT.  These optional explicit paths make the diagnostic
    # useful in CI layouts that do not expose the guest directory and let the
    # missing-artifact retry prove that no VM boot was attempted.
    result: list[str] = []
    for variable, option in (("HARMONY_KERNEL", "--kernel"),
                             ("HARMONY_BASE_INITRAMFS", "--base-initramfs"),
                             ("HARMONY_FAULT_AGENT", "--fault-agent")):
        value = environment.get(variable)
        if value:
            result.extend([option, value])
    return result


def _working_directory(cli: Path, source_workspace: Path,
                       environment: dict[str, str]) -> Path:
    explicit = environment.get("HARMONY_CWD")
    if explicit:
        return Path(explicit).resolve()
    candidates = [cli.resolve().parent.parent, source_workspace.resolve().parent]
    for candidate in candidates:
        if (candidate / "Cargo.toml").is_file():
            return candidate
    return candidates[0]


def _command(cli: Path, workspace: Path, branch: str, request_id: str,
             environment: dict[str, str]) -> list[str]:
    return [str(cli), "-w", str(workspace), "--json", *_artifact_args(environment),
            "exec", branch, "--within", "0ns", "--extend", "--wall-seconds", "600",
            "--request-id", request_id, "--", *COMMAND_ARGV]


def _write_process_output(directory: Path, label: str, stdout: bytes, stderr: bytes) -> None:
    (directory / f"{label}.stdout").write_bytes(stdout)
    (directory / f"{label}.stderr").write_bytes(stderr)


def _as_bytes(value: bytes | str | None) -> bytes:
    if value is None:
        return b""
    if isinstance(value, bytes):
        return value
    return value.encode()


def _kill_process_group(process: subprocess.Popen[bytes]) -> None:
    """Bound cleanup for the session created by an interrupted CLI process."""
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except (OSError, ProcessLookupError):
        pass
    try:
        process.kill()
    except (OSError, ProcessLookupError):
        pass


def _bounded_communicate(
    process: subprocess.Popen[bytes], timeout: int
) -> tuple[bytes, bytes, bool]:
    """Collect process output without waiting forever on inherited pipe fds."""
    try:
        stdout, stderr = process.communicate(timeout=timeout)
        return stdout, stderr, False
    except subprocess.TimeoutExpired as error:
        stdout = _as_bytes(error.stdout)
        stderr = _as_bytes(error.stderr)
        _kill_process_group(process)
        try:
            stdout, stderr = process.communicate(timeout=PROCESS_CLEANUP_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired as cleanup_error:
            stdout = _as_bytes(cleanup_error.stdout) or stdout
            stderr = _as_bytes(cleanup_error.stderr) or stderr
            _kill_process_group(process)
            for stream in (process.stdout, process.stderr):
                if stream is not None:
                    stream.close()
            try:
                process.wait(timeout=PROCESS_CLEANUP_TIMEOUT_SECONDS)
            except subprocess.TimeoutExpired:
                pass
        return stdout, stderr, True


def _read_acknowledgement(read_fd: int, expected: bytes) -> bytes:
    """Read an acknowledgement with a deadline even if a descendant kept fd open."""
    os.set_blocking(read_fd, False)
    received = bytearray()
    deadline = time.monotonic() + ACK_READ_TIMEOUT_SECONDS
    while len(received) < len(expected):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        try:
            readable, _, _ = select.select([read_fd], [], [], remaining)
        except InterruptedError:
            continue
        if not readable:
            break
        try:
            chunk = os.read(read_fd, 4096)
        except BlockingIOError:
            continue
        if not chunk:
            break
        received.extend(chunk)

    # Drain bytes already available so an acknowledgement with trailing data
    # cannot pass as an exact boundary marker.  The zero-timeout select keeps
    # this independent of descendants that retain the write descriptor.
    while True:
        try:
            readable, _, _ = select.select([read_fd], [], [], 0)
        except InterruptedError:
            continue
        if not readable:
            break
        try:
            chunk = os.read(read_fd, 4096)
        except BlockingIOError:
            break
        if not chunk:
            break
        received.extend(chunk)
    return bytes(received)


def _interrupted(directory: Path, cli: Path, workspace: Path, branch: str,
                 request_id: str, phase: str, interceptor: Path,
                 environment: dict[str, str], final_path: Path,
                 cwd: Path) -> dict[str, Any]:
    read_fd, write_fd = os.pipe()
    try:
        child_environment = _preload_environment(
            environment, interceptor, phase, final_path, write_fd)
        process = subprocess.Popen(
            _command(cli, workspace, branch, request_id, child_environment),
            cwd=cwd,
            env=child_environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            pass_fds=(write_fd,),
            start_new_session=True,
        )
        os.close(write_fd)
        write_fd = -1
        stdout, stderr, timed_out = _bounded_communicate(process, PROCESS_TIMEOUT_SECONDS)
        _kill_process_group(process)
        _write_process_output(directory, f"{phase}-interrupted", stdout, stderr)
        require(not timed_out,
                f"{phase} interposition timed out; child was killed after the bounded timeout")
        expected_ack = ACK_PREFIX + phase.encode("ascii") + b"\n"
        acknowledgement = _read_acknowledgement(read_fd, expected_ack)
        require(acknowledgement == expected_ack,
                f"{phase} interposition did not acknowledge the exact boundary: "
                f"{acknowledgement!r}")
        require(process.returncode == -signal.SIGKILL,
                f"{phase} interposition returned {process.returncode}, not SIGKILL")
        require(stdout == b"",
                f"{phase} killed process emitted a successful CLI reply before SIGKILL")
        return {"phase": phase, "request_id": request_id,
                "acknowledgement": acknowledgement.decode("ascii").rstrip(),
                "returncode": process.returncode}
    finally:
        if write_fd >= 0:
            os.close(write_fd)
        os.close(read_fd)


def _invoke(directory: Path, label: str, cli: Path, workspace: Path, args: list[str],
            environment: dict[str, str], cwd: Path, *, timeout: int = PROCESS_TIMEOUT_SECONDS,
            parse: bool = True) -> Any:
    child_environment = _clean_environment(environment)
    command = [str(cli), "-w", str(workspace), "--json", *_artifact_args(child_environment), *args]
    try:
        completed = subprocess.run(
            command,
            cwd=cwd,
            env=child_environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        stdout = error.stdout if isinstance(error.stdout, bytes) else (error.stdout or "").encode()
        stderr = error.stderr if isinstance(error.stderr, bytes) else (error.stderr or "").encode()
        _write_process_output(directory, label, stdout, stderr)
        raise AssertionError(f"{label} exceeded its bounded timeout") from error
    _write_process_output(directory, label, completed.stdout, completed.stderr)
    require(completed.returncode == 0,
            f"{label} failed ({completed.returncode}); see {label}.stderr")
    if not parse:
        return None
    try:
        return json.loads(completed.stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AssertionError(f"{label} did not return JSON: {error}") from error


def _pending_command(point: dict[str, Any], request_id: str) -> dict[str, Any]:
    command = point.get("command")
    require(isinstance(command, dict), "exec result has no retained command")
    invocation = command.get("invocation")
    require(isinstance(invocation, dict), "retained command has no invocation")
    require(type(invocation.get("engine_id")) is int and invocation["engine_id"] >= 0,
            "retained command has no engine identity")
    require(invocation.get("request_id") == request_id,
            "retained command has a different request id")
    require(invocation.get("argv") == COMMAND_ARGV,
            "retained command has different argv")
    require(command.get("completion") == "pending", "exec did not retain a pending command")
    evidence = command.get("evidence")
    require(isinstance(evidence, str) and evidence in point.get("evidence", []),
            "pending command evidence is not present in the reply")
    require(point.get("operation") == "exec", "reply is not an exec result")
    require(point.get("history") == "modified", "exec did not mark history modified")
    require(isinstance(point.get("branch"), str) and point["branch"],
            "exec result has no branch")
    require(isinstance(point.get("moment"), str) and point["moment"],
            "exec result has no immutable moment")
    require(type(point.get("virtual_time")) is int and point["virtual_time"] >= 0,
            "exec result has no virtual endpoint")
    state_hash = point.get("state_hash")
    require(isinstance(state_hash, str) and re.fullmatch(r"[0-9a-f]{64}", state_hash),
            "exec result has no engine state digest")
    return command


def _strip_runtime_fields(value: Any) -> Any:
    if isinstance(value, dict):
        return {
            key: _strip_runtime_fields(item)
            for key, item in value.items()
            if key not in {"replayed_request", "virtual_time_nanos", "virtual_time_duration"}
        }
    if isinstance(value, list):
        return [_strip_runtime_fields(item) for item in value]
    return value


def _rust_lossy_lines(value: bytes) -> list[str]:
    """Match String::from_utf8_lossy(...).lines() used by the shipping CLI."""
    text = value.decode("utf-8", errors="replace")
    if not text:
        return []
    lines = text.split("\n")
    terminated = lines[-1] == ""
    if terminated:
        lines.pop()
    last = len(lines) - 1
    return [line[:-1] if line.endswith("\r") and (terminated or index < last) else line
            for index, line in enumerate(lines)]


def _validate_transaction(path: Path, sequence: int, branch: str, request_id: str,
                          point: dict[str, Any], command: dict[str, Any]) -> dict[str, Any]:
    transaction = _transaction(path, sequence)
    records = transaction["records"]
    moment_records = [record for record in _records(transaction, "moment")
                      if record.get("id") == point.get("moment")]
    require(len(moment_records) == 1, "new transaction does not publish the result moment")
    moment = moment_records[0]
    require(moment.get("branch") == branch and moment.get("history") == "modified",
            "published moment has the wrong branch or history")
    require(moment.get("virtual_time") == point.get("virtual_time"),
            "published moment has a different virtual time")
    require(moment.get("command") == command,
            "published moment command differs from the CLI result")

    branch_records = [record for record in _records(transaction, "branch")
                      if record.get("name") == branch]
    require(len(branch_records) == 1, "new transaction does not publish the branch head")
    branch_record = branch_records[0]
    require(branch_record.get("head") == moment["id"],
            "published branch head does not name the result moment")
    pending = branch_record.get("pending_command")
    require(isinstance(pending, dict), "published branch lost pending command metadata")
    require(pending.get("engine_id") == command["invocation"]["engine_id"]
            and pending.get("request_id") == request_id
            and pending.get("argv") == COMMAND_ARGV
            and pending.get("evidence") == command["evidence"],
            "published pending metadata differs from the command record")

    evidence_records = [record for record in _records(transaction, "evidence")
                        if record.get("moment") == moment["id"]]
    require(len(evidence_records) >= 3,
            "new transaction does not publish console, event, and command evidence")
    command_evidence = [record for record in evidence_records
                        if record.get("id") == command["evidence"]]
    require(len(command_evidence) == 1 and command_evidence[0].get("kind") == "command",
            "new transaction does not publish the command evidence record")

    request_records = [record for record in _records(transaction, "request")
                       if record.get("id") == request_id]
    require(len(request_records) == 1, "new transaction does not publish the request result")
    request = request_records[0]
    require(request.get("operation") == "exec" and request.get("sequence") == sequence,
            "published request has the wrong operation or sequence")
    result = request.get("result")
    require(isinstance(result, dict), "published request has no JSON result")
    require(_strip_runtime_fields(result) == _strip_runtime_fields(point),
            "journal request result differs from the CLI result")
    # A transaction that contains the request must contain all of its endpoint
    # records together; this rejects a plausible-looking request-only journal.
    require(any(record.get("record") == "moment" and record.get("id") == moment["id"]
                for record in records), "request transaction omitted its moment")
    return transaction


def _validate_interrupted_transaction(path: Path, sequence: int, branch: str,
                                      request_id: str) -> dict[str, Any]:
    """Validate the durable request before any retry is allowed to inspect it."""
    transaction = _transaction(path, sequence)
    request_records = [record for record in _records(transaction, "request")
                       if record.get("id") == request_id]
    require(len(request_records) == 1,
            "published interruption transaction has no unique matching request")
    result = request_records[0].get("result")
    require(isinstance(result, dict) and result.get("replayed_request") is False,
            "published interruption request is not the original result")
    command = _pending_command(result, request_id)
    require(result.get("branch") == branch,
            "published interruption request names a different branch")
    return _validate_transaction(path, sequence, branch, request_id, result, command)


def _verified_blob(blob: Path, digest: str) -> bytes:
    try:
        metadata = blob.lstat()
    except OSError as error:
        raise AssertionError(f"cannot inspect command evidence blob {blob}: {error}") from error
    require(stat.S_ISREG(metadata.st_mode),
            f"command evidence blob is not a regular file: {blob}")
    try:
        bytes_value = blob.read_bytes()
    except OSError as error:
        raise AssertionError(f"cannot read command evidence blob {blob}: {error}") from error
    require(hashlib.sha256(bytes_value).hexdigest() == digest,
            "command evidence blob digest does not verify its raw bytes")
    return bytes_value


def _validate_command_evidence(directory: Path, label: str, cli: Path, workspace: Path,
                               point: dict[str, Any], command: dict[str, Any],
                               environment: dict[str, str], cwd: Path) -> dict[str, Any]:
    summary = _invoke(directory, f"{label}-summary", cli, workspace,
                      ["inspect", point["moment"]], environment, cwd)
    require(summary.get("format") == "harmony-inspect-v1",
            "inspect summary has the wrong format")
    require(summary.get("moment") == point["moment"]
            and summary.get("virtual_time") == point["virtual_time"],
            "inspect summary names a different endpoint")
    require(summary.get("command") == command,
            "inspect summary command differs from the exec result")
    require(summary.get("state_hash") == point.get("state_hash"),
            "inspect summary hash differs from the exec result")
    command_records = [item for item in summary.get("evidence", [])
                       if item.get("kind") == "command"]
    require(len(command_records) == 1,
            "endpoint does not expose exactly one command evidence record")
    command_record = command_records[0]
    require(command_record.get("evidence") == command.get("evidence"),
            "inspect summary command evidence differs from the command record")
    digest = command_record.get("sha256")
    require(isinstance(digest, str) and re.fullmatch(r"[0-9a-f]{64}", digest),
            "command evidence has no blob digest")
    blob = workspace / "blobs" / digest
    require(blob.exists(), f"command evidence blob is missing: {blob}")
    bytes_value = _verified_blob(blob, digest)
    require(command_record.get("bytes") == len(bytes_value),
            "command evidence byte count differs from its blob")
    require(command_record.get("truncated") is False, "command evidence is truncated")

    view = _invoke(directory, f"{label}-evidence", cli, workspace,
                   ["inspect", point["moment"], "command", "--limit", "1000000"],
                   environment, cwd)
    require(view.get("format") == "harmony-inspect-evidence-v1"
            and view.get("view") == "command"
            and view.get("moment") == point["moment"],
            "command evidence inspect has the wrong endpoint")
    require(view.get("evidence") == command.get("evidence")
            and view.get("offset") == 0 and view.get("truncated") is False,
            "command evidence inspect is not a complete immutable view")
    lines = view.get("lines")
    require(isinstance(lines, list) and view.get("shown") == view.get("matched") == len(lines),
            "command evidence inspect is truncated or incomplete")
    require(lines == _rust_lossy_lines(bytes_value),
            "command evidence inspect lines differ from the retained raw blob")
    return summary


def _validate_branch_head(directory: Path, label: str, cli: Path, workspace: Path,
                          branch: str, point: dict[str, Any], command: dict[str, Any],
                          environment: dict[str, str], cwd: Path) -> None:
    branches = _invoke(directory, f"{label}-branches", cli, workspace,
                       ["branches"], environment, cwd)
    matching = [item for item in branches.get("branches", [])
                if item.get("branch") == branch]
    require(len(matching) == 1, "workspace does not expose exactly one target branch")
    branch_record = matching[0]
    require(branch_record.get("head") == point.get("moment")
            and branch_record.get("history") == "modified",
            "branch head does not name the published modified endpoint")
    require(branch_record.get("pending_command"), "branch head lost pending command metadata")
    _validate_command_evidence(directory, label, cli, workspace, point, command, environment, cwd)


def _missing_artifact_environment(environment: dict[str, str], directory: Path) -> dict[str, str]:
    child = _clean_environment(environment)
    missing = str((directory / "absent-guest-artifacts").resolve())
    # Explicit flags take precedence over installed-artifact discovery and make
    # a successful retry a proof that the VM path was never entered.
    child["HARMONY_GUEST_DIR"] = missing
    child["HARMONY_KERNEL"] = str(Path(missing) / "kernel")
    child["HARMONY_BASE_INITRAMFS"] = str(Path(missing) / "base-initramfs")
    child["HARMONY_FAULT_AGENT"] = str(Path(missing) / "fault-agent")
    return child


def _phase(root: Path, phase: str, cli: Path, source_workspace: Path, branch: str,
           source_tree: dict[str, tuple[str, int, str, int]],
           source_journal: dict[str, bytes],
           next_sequence: int, next_name: str, interceptor: Path,
           environment: dict[str, str]) -> dict[str, Any]:
    phase_root = root / phase
    phase_root.mkdir()
    workspace = phase_root / "workspace"
    require(_source_tree_snapshot(source_workspace) == source_tree,
            f"{phase} source workspace changed before cloning")
    shutil.copytree(source_workspace, workspace, symlinks=True)
    require(_source_tree_snapshot(workspace) == source_tree,
            f"{phase} private workspace differs from the source tree before execution")
    require(_committed_journal(workspace) == source_journal,
            f"{phase} private workspace differs from the source journal before execution")
    final_path = workspace / "journal" / next_name
    partial_path = workspace / "journal" / f".{next_name}.partial"
    require(not final_path.exists(), f"{phase} workspace already has the next final journal")
    request_id = f"journal-interrupt-{phase}-{os.getpid()}-{secrets.token_hex(8)}"
    cwd = _working_directory(cli, source_workspace, environment)
    process = _interrupted(phase_root, cli, workspace, branch, request_id, phase,
                           interceptor, environment, final_path, cwd)
    require(_source_tree_snapshot(source_workspace) == source_tree,
            f"{phase} interruption changed the original workspace files")
    require(_committed_journal(source_workspace) == source_journal,
            f"{phase} interruption changed the original workspace journal")
    after_kill = _committed_journal(workspace)
    if phase == "before":
        require(after_kill == source_journal,
                "before-publication interruption changed committed journal bytes")
        require(not final_path.exists(), "before-publication interruption left a final transaction")
    else:
        require(set(after_kill) == set(source_journal) | {next_name},
                "after-publication interruption did not leave exactly one new final transaction")
        require(final_path.exists(), "after-publication interruption left no final transaction")
    require(partial_path.is_file(),
            f"{phase} interruption did not leave the expected publication staging file")
    partial = _transaction(partial_path, next_sequence)
    if phase == "after":
        require(final_path.read_bytes() == partial_path.read_bytes(),
                "published final transaction differs from its surviving hard-link staging file")
        _validate_interrupted_transaction(final_path, next_sequence, branch, request_id)

    retry_environment = _clean_environment(environment)
    if phase == "after":
        retry_environment = _missing_artifact_environment(retry_environment, phase_root)
    retry = _invoke(phase_root, f"{phase}-retry", cli, workspace,
                    ["exec", branch, "--within", "0ns", "--extend", "--wall-seconds", "600",
                     "--request-id", request_id, "--", *COMMAND_ARGV],
                    retry_environment, cwd)
    command = _pending_command(retry, request_id)
    require(retry.get("branch") == branch, f"{phase} retry returned a different branch")
    if phase == "before":
        require(retry.get("replayed_request") is False,
                "before-publication retry did not execute and publish a new request")
        require(_committed_journal(workspace)
                == source_journal | {next_name: final_path.read_bytes()},
                "clean retry did not publish exactly the next journal transaction")
        require(final_path.is_file() and not partial_path.exists(),
                "clean retry did not replace the unpublished staging transaction")
    else:
        require(retry.get("replayed_request") is True,
                "after-publication retry did not use the committed request result")
        require(_committed_journal(workspace) == after_kill,
                "after-publication cached retry changed journal bytes")
    transaction = _validate_transaction(final_path, next_sequence, branch, request_id,
                                        retry, command)
    _validate_branch_head(phase_root, f"{phase}-retry", cli, workspace, branch, retry,
                          command, retry_environment, cwd)
    require(_source_tree_snapshot(source_workspace) == source_tree,
            f"{phase} retry changed the original workspace files")
    require(_committed_journal(source_workspace) == source_journal,
            f"{phase} retry changed the original workspace journal")
    return {
        "phase": phase,
        "request_id": request_id,
        "workspace": str(workspace),
        "next_journal": next_name,
        "killed": process,
        "transaction_sequence": transaction["sequence"],
        "pending_reply": retry,
        "retry": retry,
    }


def _validator_fixture(root: Path) -> _ValidatorFixture:
    """Build one schema-shaped transaction for validator-only self-tests."""
    journal = root / "journal"
    blobs = root / "blobs"
    journal.mkdir(parents=True)
    blobs.mkdir()
    request_id = "journal-validator-request"
    command = {
        "invocation": {
            "engine_id": 7,
            "request_id": request_id,
            "argv": list(COMMAND_ARGV),
        },
        "completion": "pending",
        "evidence": "ev-command",
    }
    point = {
        "operation": "exec",
        "branch": "main",
        "moment": "moment-1",
        "virtual_time": 123,
        "history": "modified",
        "state_hash": "a" * 64,
        "replayed_request": False,
        "command": command,
        "evidence": ["ev-console", "ev-events", "ev-command"],
    }
    raw_output = b"guest line one\r\nguest line two\n"
    digest = hashlib.sha256(raw_output).hexdigest()
    blob = blobs / digest
    blob.write_bytes(raw_output)
    transaction = {
        "format": JOURNAL_FORMAT,
        "sequence": 1,
        "records": [
            {
                "record": "moment",
                "id": "moment-1",
                "branch": "main",
                "history": "modified",
                "virtual_time": 123,
                "command": command,
            },
            {
                "record": "branch",
                "name": "main",
                "head": "moment-1",
                "history": "modified",
                "pending_command": {
                    "engine_id": 7,
                    "request_id": request_id,
                    "argv": list(COMMAND_ARGV),
                    "evidence": "ev-command",
                },
            },
            {"record": "evidence", "id": "ev-console", "kind": "console",
             "moment": "moment-1"},
            {"record": "evidence", "id": "ev-events", "kind": "events",
             "moment": "moment-1"},
            {"record": "evidence", "id": "ev-command", "kind": "command",
             "moment": "moment-1", "blob": digest, "bytes": len(raw_output),
             "truncated": False},
            {"record": "request", "id": request_id, "operation": "exec",
             "sequence": 1, "result": point},
        ],
    }
    transaction_path = journal / "000001.json"
    transaction_path.write_text(json.dumps(transaction))
    return {
        "transaction": transaction_path,
        "point": point,
        "command": command,
        "blob": blob,
        "digest": digest,
    }


def _expect_validation_failure(label: str, operation: Any) -> None:
    try:
        operation()
    except AssertionError:
        return
    raise AssertionError(f"validator negative control unexpectedly passed: {label}")


def _self_test_validators() -> None:
    """Exercise typed relation and raw-evidence rejection without a model or VM."""
    with tempfile.TemporaryDirectory(prefix="harmony-journal-validator-") as temporary:
        root = Path(temporary)
        source_root = root / "source-tree"
        source_root.mkdir()
        source_file = source_root / "mode-sensitive"
        source_file.write_bytes(b"source")
        source_file.chmod(0o600)
        source_snapshot = _source_tree_snapshot(source_root)
        source_file.chmod(0o644)
        require(_source_tree_snapshot(source_root) != source_snapshot,
                "source snapshot omitted a regular-file mode change")
        (source_root / "alias").symlink_to(source_file)
        _expect_validation_failure(
            "source symlink", lambda: _source_tree_snapshot(source_root))

        fixture = _validator_fixture(root / "positive")
        transaction = _validate_transaction(
            fixture["transaction"], 1, "main", "journal-validator-request",
            fixture["point"], fixture["command"])
        require(transaction["sequence"] == 1, "validator positive control did not pass")
        require(_rust_lossy_lines(fixture["blob"].read_bytes())
                == ["guest line one", "guest line two"],
                "Rust line decoding control is inconsistent")
        require(_rust_lossy_lines(b"terminated\r\nlone\r")
                == ["terminated", "lone\r"],
                "Rust line decoding incorrectly strips a lone carriage return")

        wrong_command = copy.deepcopy(fixture["point"])
        wrong_command["command"]["invocation"]["argv"] = ["wrong-command"]
        _expect_validation_failure(
            "wrong command", lambda: _pending_command(wrong_command, "journal-validator-request"))

        missing_command = copy.deepcopy(fixture["point"])
        missing_command["command"] = None
        _expect_validation_failure(
            "missing command", lambda: _pending_command(missing_command,
                                                         "journal-validator-request"))

        for label, mutate in (
            ("moment relation", lambda records: next(record for record in records
                                                       if record["record"] == "moment")
             .update(id="other-moment")),
            ("evidence relation", lambda records: records.__setitem__(
                4, {"record": "evidence", "id": "ev-command-removed", "kind": "events",
                    "moment": "moment-1"})),
            ("request relation", lambda records: records.pop()),
            ("head relation", lambda records: next(record for record in records
                                                    if record["record"] == "branch")
            .update(head="other-moment")),
        ):
            case = _validator_fixture(root / label.replace(" ", "-"))
            document = json.loads(case["transaction"].read_text())
            mutate(document["records"])
            case["transaction"].write_text(json.dumps(document))
            _expect_validation_failure(
                label,
                lambda case=case: _validate_transaction(
                    case["transaction"], 1, "main", "journal-validator-request",
                    case["point"], case["command"]),
            )

        mutated_blob = _validator_fixture(root / "mutated-raw-hash")
        mutated_blob["blob"].write_bytes(b"mutated raw evidence")
        _expect_validation_failure(
            "mutated raw evidence hash",
            lambda: _verified_blob(mutated_blob["blob"], mutated_blob["digest"]),
        )
    print("journal validator self-test: typed relations and raw hash negatives passed")


def qualify(cli: Path, source_workspace: Path, branch: str, output: Path,
            env: dict[str, Any]) -> dict[str, Any]:
    """Run both hard-link interruption phases and return a JSON-safe summary."""
    require(platform.system() == "Linux", "journal interruption qualification is Linux-only")
    cli = Path(cli).resolve()
    source_workspace = Path(source_workspace).resolve()
    output = Path(output).resolve()
    require(cli.is_file() and os.access(cli, os.X_OK), f"shipping CLI is not executable: {cli}")
    require(source_workspace.is_dir(), f"source workspace is not a directory: {source_workspace}")
    require(branch and "@" not in branch, "branch must be a nonempty name without '@'")
    require(output != source_workspace and source_workspace not in output.parents,
            "diagnostic output must be outside the source workspace")
    require(not output.exists(), f"diagnostic output already exists: {output}")
    output.mkdir(parents=True)
    environment = _string_env(env)
    source_tree = _source_tree_snapshot(source_workspace)
    source_journal = _committed_journal(source_workspace)
    _source_branch(source_workspace, branch)
    next_sequence, next_name = _next_transaction(source_workspace)
    interceptor = _compile_interceptor(output)
    phases = {}
    for phase in ("before", "after"):
        phases[phase] = _phase(
            output, phase, cli, source_workspace, branch, source_tree,
            source_journal, next_sequence, next_name, interceptor, environment)
    require(_source_tree_snapshot(source_workspace) == source_tree,
            "original workspace files changed during qualification")
    require(_committed_journal(source_workspace) == source_journal,
            "original workspace journal changed during qualification")
    summary = {
        "format": "harmony-cli-journal-interruption-v1",
        "status": "passed",
        "source_workspace": str(source_workspace),
        "branch": branch,
        "next_journal": next_name,
        "phases": phases,
        "scope": "shipping CLI process interruption at durable journal hard-link publication",
    }
    (output / "result.json").write_text(json.dumps(summary, indent=2) + "\n")
    return summary


def _self_test_interceptor() -> None:
    """Exercise the interposer on plain Linux file links without a VM."""
    if platform.system() != "Linux":
        print("journal interposer self-test: skipped on non-Linux host")
        return
    with tempfile.TemporaryDirectory(prefix="harmony-journal-interposer-") as temporary:
        root = Path(temporary)
        interceptor = _compile_interceptor(root)
        for phase in ("before", "after"):
            phase_root = root / phase
            phase_root.mkdir()
            source = phase_root / "source"
            final = phase_root / "journal" / "000001.json"
            unrelated = phase_root / "journal" / "000001.json.other"
            final.parent.mkdir()
            source.write_bytes(b"journal")
            read_fd, write_fd = os.pipe()
            child_environment = _preload_environment(
                {}, interceptor, phase, final, write_fd)
            child = subprocess.Popen(
                [sys.executable, "-c", "import os,sys; os.link(sys.argv[1], sys.argv[2])",
                 str(source), str(final)],
                env=child_environment,
                pass_fds=(write_fd,),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
            os.close(write_fd)
            stdout, stderr, timed_out = _bounded_communicate(child, 10)
            _kill_process_group(child)
            acknowledgement = _read_acknowledgement(
                read_fd, ACK_PREFIX + phase.encode() + b"\n")
            os.close(read_fd)
            require(not timed_out, f"{phase} self-test exceeded its bounded timeout")
            require(child.returncode == -signal.SIGKILL,
                    f"{phase} self-test was not killed: {child.returncode}; {stderr!r}")
            require(stdout == b"" and acknowledgement == ACK_PREFIX + phase.encode() + b"\n",
                    f"{phase} self-test acknowledgement/output mismatch")
            require((final.exists()) is (phase == "after"),
                    f"{phase} self-test final-link state is wrong")

            unrelated_child_environment = _preload_environment(
                {}, interceptor, phase, final, -1)
            unrelated_child = subprocess.Popen(
                [sys.executable, "-c", "import os,sys; os.link(sys.argv[1], sys.argv[2])",
                 str(source), str(unrelated)],
                env=unrelated_child_environment,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
            unrelated_stdout, unrelated_stderr, unrelated_timed_out = _bounded_communicate(
                unrelated_child, 10)
            _kill_process_group(unrelated_child)
            require(not unrelated_timed_out and unrelated_child.returncode == 0
                    and unrelated.exists(),
                    f"{phase} self-test intercepted an unrelated path: "
                    f"{unrelated_stderr!r}; stdout={unrelated_stdout!r}")
    print("journal interposer self-test: exact before/after links and unrelated path passed")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true",
                        help="test the Linux interposer on plain file links")
    parser.add_argument("--cli", type=Path)
    parser.add_argument("--source-workspace", type=Path)
    parser.add_argument("--branch")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--env", action="append", default=[], metavar="KEY=VALUE",
                        help="environment override passed to the shipping CLI (repeatable)")
    args = parser.parse_args()
    if args.self_test:
        _self_test_validators()
        _self_test_interceptor()
        return 0
    missing = [name for name, value in (("--cli", args.cli),
                                        ("--source-workspace", args.source_workspace),
                                        ("--branch", args.branch), ("--output", args.output))
               if value is None]
    if missing:
        parser.error("qualification requires " + ", ".join(missing))
    environment = {}
    for assignment in args.env:
        key, separator, value = assignment.partition("=")
        if not separator or not key:
            parser.error(f"--env must be KEY=VALUE, got {assignment!r}")
        environment[key] = value
    summary = qualify(args.cli, args.source_workspace, args.branch, args.output, environment)
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
