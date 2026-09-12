#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def package(binary, architecture, output):
    data = binary.read_bytes()
    if len(data) < 64 or data[:6] != b"\x7fELF\x02\x01":
        raise ValueError("fixture must be a 64-bit little-endian ELF executable")
    if int.from_bytes(data[18:20], "little") != {"amd64": 62, "arm64": 183}[architecture]:
        raise ValueError("fixture ELF architecture does not match the image platform")
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        for name in ["app", "input", "tmp", "work"]:
            entry = tarfile.TarInfo(name)
            entry.type = tarfile.DIRTYPE
            entry.mode = 0o1777 if name == "tmp" else 0o755
            archive.addfile(entry)
        entry = tarfile.TarInfo("app/runtime-fixture")
        entry.mode = 0o755
        entry.size = len(data)
        archive.addfile(entry, io.BytesIO(data))
    layer = stream.getvalue()
    config = encoded({
        "architecture": architecture,
        "os": "linux",
        "config": {
            "User": "1000:1000",
            "Entrypoint": ["/app/runtime-fixture"],
            "Cmd": ["verify"],
            "Env": ["HARMONY_FIXTURE=platform"],  # pragma: allowlist secret
            "WorkingDir": "/work",
        },
        "rootfs": {"type": "layers", "diff_ids": ["sha256:" + hashlib.sha256(layer).hexdigest()]},
    })
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".runtime-fixture-", dir=output.parent) as scratch:
        root = Path(scratch) / "image"
        blobs = root / "blobs" / "sha256"
        blobs.mkdir(parents=True)

        def blob(contents, media_type):
            digest = hashlib.sha256(contents).hexdigest()
            (blobs / digest).write_bytes(contents)
            return {"mediaType": media_type, "digest": "sha256:" + digest, "size": len(contents)}

        manifest = encoded({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": blob(config, "application/vnd.oci.image.config.v1+json"),
            "layers": [blob(layer, "application/vnd.oci.image.layer.v1.tar")],
        })
        descriptor = blob(manifest, "application/vnd.oci.image.manifest.v1+json")
        descriptor["platform"] = {"os": "linux", "architecture": architecture}
        (root / "index.json").write_bytes(encoded({"schemaVersion": 2, "manifests": [descriptor]}))
        (root / "oci-layout").write_bytes(encoded({"imageLayoutVersion": "1.0.0"}))
        os.rename(root, output)
    return descriptor["digest"]


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--architecture", required=True, choices=["amd64", "arm64"])
    args = parser.parse_args()
    print(package(args.binary, args.architecture, args.output))
