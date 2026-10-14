#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

import os
from pathlib import Path
import re
import subprocess
import tempfile


SOURCE = Path(__file__).with_name("uuid-super.c")


def run(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs)


with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    object_file = root / "uuid-super.o"
    run("cc", "-O2", "-Wall", "-Wextra", "-c", str(SOURCE), "-o", str(object_file))
    disassembly = run("objdump", "-dr", str(object_file)).stdout
    if re.search(r"\b(?:rdrand|rdseed)\b", disassembly, re.IGNORECASE):
        raise SystemExit("uuid-super embeds native hardware entropy instructions")

    device = root / "harmony-device"
    expected_word = 0x47E4CE4B896CDD1D
    device.write_bytes(b"\xff" + expected_word.to_bytes(8, "little"))
    binary = root / "uuid-super"
    run(
        "cc", "-O2", "-Wall", "-Wextra", "-DITERS=8193",
        f'-DHARMONY_DEVICE_PATH="{device}"',
        str(SOURCE), "-o", str(binary),
    )
    result = run(str(binary), env={**os.environ, "UUID_DEBUG": "1"}, timeout=10)
    if f"UUID_DRAW: draw=0x{expected_word:x}" not in result.stdout:
        raise SystemExit(f"seeded device word was not used:\n{result.stdout}")
    if device.read_bytes()[0] != 0:
        raise SystemExit("uuid-super did not send the entropy request byte")

    device.unlink()
    failed = subprocess.run(
        [str(binary)], text=True, capture_output=True,
        env={**os.environ, "UUID_DEBUG": "1"}, timeout=10,
    )
    if failed.returncode == 0 or "UUID_ENTROPY_FAIL" not in failed.stdout:
        raise SystemExit("missing seeded device did not fail loudly")
    if "UUID_DRAW" in failed.stdout or "UUID_BUG" in failed.stdout:
        raise SystemExit("missing seeded device reached the rare-entropy decision")

print("ok: uuid supervisor uses the seeded device and rejects native entropy")
