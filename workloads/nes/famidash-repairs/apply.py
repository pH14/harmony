# SPDX-License-Identifier: AGPL-3.0-or-later
"""Apply the reviewed Harmony ROM delta to checksum-verified official ROMs."""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import tempfile
import os
import zlib

FILES = {"famidash.nes", "famidash-b-sides.nes", "famidash-c-sides.nes", "famidash-d-sides.nes"}
ROM_BYTES = 786448


def digest(data):
    return hashlib.sha256(data).hexdigest()


def patch(original, entry):
    if len(original) != ROM_BYTES or entry["bytes"] != ROM_BYTES:
        raise ValueError("unexpected ROM size")
    if digest(original) != entry["input_sha256"]:
        raise ValueError("official ROM checksum mismatch")
    result = bytearray(original)
    end = 0
    for block in entry["blocks"]:
        offset, size = block["offset"], block["size"]
        if type(offset) is not int or type(size) is not int or offset < end or size <= 0 or offset + size > ROM_BYTES:
            raise ValueError("invalid or overlapping patch block")
        decoder = zlib.decompressobj()
        data = decoder.decompress(base64.b64decode(block["data"], validate=True), size + 1)
        if len(data) != size or not decoder.eof or decoder.unused_data or decoder.unconsumed_tail:
            raise ValueError("invalid compressed patch block")
        result[offset:offset + size] = data
        end = offset + size
    if digest(result) != entry["output_sha256"]:
        raise ValueError("patched ROM checksum mismatch")
    return result


def apply(manifest, original, output):
    entries = manifest["files"]
    if manifest["schema"] != 1 or len(entries) != len(FILES) or {e["file"] for e in entries} != FILES:
        raise ValueError("unsupported ROM patch manifest")
    results = {entry["file"]: patch((original / entry["file"]).read_bytes(), entry) for entry in entries}
    output.mkdir(parents=True, exist_ok=True)
    for name, data in results.items():
        with tempfile.NamedTemporaryFile(dir=output, delete=False) as temporary:
            temporary.write(data)
            path = Path(temporary.name)
        try:
            path.chmod(0o644)
            os.replace(path, output / name)
        finally:
            path.unlink(missing_ok=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("original", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    apply(json.loads(Path(__file__).with_name("roms.json").read_text()), args.original, args.output)
