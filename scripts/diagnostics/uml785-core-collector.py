#!/usr/bin/python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import fcntl
import hashlib
import json
import os
from pathlib import Path
import sys


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def process_metadata(process):
    result = {}
    for name in ("maps", "status", "coredump_filter"):
        try:
            with (process / name).open("rb") as source:
                content = source.read(1024 * 1024 + 1)
            result[name] = {"text": content[:1024 * 1024].decode("utf-8", errors="replace"),
                            "truncated": len(content) > 1024 * 1024}
        except OSError as error:
            result[name] = {"error": str(error)}
    return result


def collect(config, pid, uid, timestamp, source, proc=Path("/proc")):
    if not all(value.isdecimal() for value in (pid, uid, timestamp)):
        return
    if int(uid) != config["uid"]:
        return
    process = proc / pid
    real_uid = next(line.split()[1] for line in (process / "status").read_text().splitlines()
                    if line.startswith("Uid:"))
    if real_uid != uid:
        return
    executable_sha256 = digest(process / "exe")
    role = config["executables"].get(executable_sha256)
    if role is None:
        return
    process_state = process_metadata(process)
    output = Path(config["output"])
    with (output / "collector.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        existing = list(output.glob("*.core"))
        remaining = config["total_limit"] - sum(path.stat().st_size for path in existing)
        if len(existing) >= config["count_limit"] or remaining <= 0:
            return
        limit = min(config["per_core_limit"], remaining)
        stem = output / f"{pid}-{timestamp}-{role}"
        written = 0
        checksum = hashlib.sha256()
        truncated = False
        with stem.with_suffix(".core").open("xb") as target:
            os.chmod(target.name, 0o644)
            while chunk := source.read(1024 * 1024):
                accepted = chunk[:max(0, limit - written)]
                target.write(accepted)
                checksum.update(accepted)
                written += len(accepted)
                if len(accepted) < len(chunk):
                    truncated = True
                    break
        metadata = {"pid": int(pid), "uid": int(uid), "timestamp": int(timestamp),
                    "role": role, "executable_sha256": executable_sha256,
                    "bytes": written, "sha256": checksum.hexdigest(), "truncated": truncated,
                    "process": process_state}
        stem.with_suffix(".json").write_text(json.dumps(metadata, indent=2) + "\n")


if __name__ == "__main__":
    os.umask(0o022)
    try:
        configuration = json.loads(Path(__file__).with_name("config.json").read_text())
        collect(configuration, *sys.argv[1:], sys.stdin.buffer)
    except (OSError, ValueError, StopIteration, TypeError):
        pass
