#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Pack one assembled workload root into a deterministic OCI layout."""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import tarfile
import tempfile


ARCHITECTURES = {
    "amd64": 62,
    "arm64": 183,
}


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def relative_name(root, path):
    relative = path.relative_to(root)
    if not relative.parts or any(part in ("", ".", "..") for part in relative.parts):
        raise ValueError(f"invalid rootfs path: {path}")
    name = "/".join(relative.parts)
    if len(name.encode()) > 255:
        raise ValueError(f"rootfs path is too long for deterministic ustar: {name}")
    return name


def walk(root):
    """Yield every rootfs entry in parent-before-child bytewise order."""

    def descend(directory):
        entries = sorted(os.scandir(directory), key=lambda entry: os.fsencode(entry.name))
        for entry in entries:
            path = Path(entry.path)
            yield path
            if entry.is_dir(follow_symlinks=False):
                yield from descend(path)

    yield from descend(root)


def layer_bytes(root):
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        for path in walk(root):
            metadata = os.lstat(path)
            entry = tarfile.TarInfo(relative_name(root, path))
            entry.mode = stat.S_IMODE(metadata.st_mode)
            entry.uid = metadata.st_uid
            entry.gid = metadata.st_gid
            entry.mtime = 0
            entry.uname = ""
            entry.gname = ""
            if stat.S_ISDIR(metadata.st_mode):
                entry.type = tarfile.DIRTYPE
                entry.size = 0
                archive.addfile(entry)
            elif stat.S_ISREG(metadata.st_mode):
                entry.type = tarfile.REGTYPE
                entry.size = metadata.st_size
                with path.open("rb") as source:
                    archive.addfile(entry, source)
            elif stat.S_ISLNK(metadata.st_mode):
                entry.type = tarfile.SYMTYPE
                entry.linkname = os.readlink(path)
                entry.size = 0
                archive.addfile(entry)
            else:
                raise ValueError(
                    f"rootfs contains unsupported special file {path}; "
                    "runtime devices must be supplied by the platform"
                )
    return stream.getvalue()


def package(rootfs, architecture, output, entrypoint, cmd, env, user, working_dir):
    if architecture not in ARCHITECTURES:
        raise ValueError(f"unsupported architecture: {architecture}")
    if not rootfs.is_dir():
        raise ValueError(f"rootfs is not a directory: {rootfs}")
    layer = layer_bytes(rootfs)
    layer_digest = hashlib.sha256(layer).hexdigest()
    config = encoded(
        {
            "architecture": architecture,
            "os": "linux",
            "config": {
                "Entrypoint": entrypoint,
                "Cmd": cmd,
                "Env": env,
                "WorkingDir": working_dir,
                "User": user,
            },
            "rootfs": {
                "type": "layers",
                "diff_ids": [f"sha256:{layer_digest}"],
            },
        }
    )

    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".oci-package-", dir=output.parent) as scratch:
        root = Path(scratch) / "image"
        blobs = root / "blobs" / "sha256"
        blobs.mkdir(parents=True)

        def blob(contents, media_type):
            digest = hashlib.sha256(contents).hexdigest()
            (blobs / digest).write_bytes(contents)
            return {
                "mediaType": media_type,
                "digest": f"sha256:{digest}",
                "size": len(contents),
            }

        config_descriptor = blob(config, "application/vnd.oci.image.config.v1+json")
        layer_descriptor = blob(layer, "application/vnd.oci.image.layer.v1.tar")
        manifest = encoded(
            {
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "config": config_descriptor,
                "layers": [layer_descriptor],
            }
        )
        manifest_descriptor = blob(
            manifest, "application/vnd.oci.image.manifest.v1+json"
        )
        manifest_descriptor["platform"] = {
            "os": "linux",
            "architecture": architecture,
            "variant": "v8" if architecture == "arm64" else None,
        }
        manifest_descriptor["platform"] = {
            key: value
            for key, value in manifest_descriptor["platform"].items()
            if value is not None
        }
        (root / "index.json").write_bytes(
            encoded({"schemaVersion": 2, "manifests": [manifest_descriptor]})
        )
        (root / "oci-layout").write_bytes(
            encoded({"imageLayoutVersion": "1.0.0"})
        )
        if output.exists():
            raise ValueError(f"OCI output already exists: {output}")
        os.replace(root, output)
    return manifest_descriptor["digest"]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--rootfs", type=Path, required=True)
    parser.add_argument("--architecture", choices=sorted(ARCHITECTURES), required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--entrypoint", nargs="+", required=True)
    parser.add_argument("--cmd", nargs="*", default=[])
    parser.add_argument("--env", action="append", default=[])
    parser.add_argument("--user", default="0:0")
    parser.add_argument("--working-dir", default="/")
    args = parser.parse_args()
    print(
        package(
            args.rootfs,
            args.architecture,
            args.output,
            args.entrypoint,
            args.cmd,
            args.env,
            args.user,
            args.working_dir,
        )
    )


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, tarfile.TarError) as error:
        raise SystemExit(f"FAIL: {error}") from error
