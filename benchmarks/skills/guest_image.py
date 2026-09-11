# SPDX-License-Identifier: AGPL-3.0-or-later
"""Deterministic Docker-save packaging for a verified guest artifact set.

The archive is a transport for opaque bytes and a controller-owned bundle.  It
does not run the artifact, boot a guest, or make a semantic qualification
claim.
"""

from __future__ import annotations

import hashlib
import io
import json
import tarfile

try:
    from . import build
except ImportError:  # unittest discovery can load this directory as top-level.
    import build  # type: ignore[no-redef]


MAX_ARTIFACT_BYTES = 16 * 1024 * 1024
MAX_ARTIFACTS = 64
MAX_BUNDLE_BYTES = 16 * 1024
MAX_PATH_COMPONENT_BYTES = 255


class GuestImageError(ValueError):
    """The supplied artifacts or bundle cannot form a safe guest image."""


def _json_bytes(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":")).encode("ascii")


def _validate_artifacts(
    artifacts: tuple[build.Artifact, ...],
) -> list[tuple[str, bytes, bool]]:
    if not isinstance(artifacts, tuple) or not 1 <= len(artifacts) <= MAX_ARTIFACTS:
        raise GuestImageError("artifacts must contain between 1 and 64 records")
    records: list[tuple[str, bytes, bool]] = []
    names: set[str] = set()
    total = 0
    for artifact in artifacts:
        if not isinstance(artifact, build.Artifact):
            raise GuestImageError("artifacts must contain Artifact records")
        try:
            path = build._output_path(artifact.path)
        except (TypeError, ValueError) as exc:
            raise GuestImageError("artifact path is not a normalized relative POSIX path") from exc
        if any(len(component.encode("utf-8")) > MAX_PATH_COMPONENT_BYTES for component in path.split("/")):
            raise GuestImageError("artifact path contains an overlong filesystem component")
        if any(component.startswith(".wh.") for component in path.split("/")):
            # OCI layer application reserves .wh.* entries for whiteouts.
            raise GuestImageError("artifact path uses an OCI-reserved whiteout name")
        if path in names:
            raise GuestImageError("artifact paths must be unique")
        names.add(path)
        data = artifact.data
        if type(data) is not bytes:
            raise GuestImageError("artifact data must be bytes")
        digest = artifact.sha256
        if type(digest) is not str or len(digest) != 64 or any(
            character not in "0123456789abcdef" for character in digest
        ):
            raise GuestImageError("artifact digest must be lowercase hexadecimal")
        if hashlib.sha256(data).hexdigest() != digest:
            raise GuestImageError("artifact digest does not match artifact bytes")
        if type(artifact.executable) is not bool:
            raise GuestImageError("artifact executable flag must be boolean")
        total += len(data)
        if total > MAX_ARTIFACT_BYTES:
            raise GuestImageError("artifacts exceed the 16 MiB byte limit")
        records.append((path, data, artifact.executable))

    # A regular file cannot also be the directory containing another output.
    for path in names:
        parts = path.split("/")
        parent = ""
        for component in parts[:-1]:
            parent = component if not parent else parent + "/" + component
            if parent in names:
                raise GuestImageError("artifact paths overlap a file and directory")
    return records


def _tar_info(name: str, *, directory: bool, executable: bool, size: int) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name)
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mtime = 0
    info.mode = 0o755 if directory or executable else 0o644
    info.size = size
    info.type = tarfile.DIRTYPE if directory else tarfile.REGTYPE
    info.linkname = ""
    # PAX path records are generated only when a supported name needs them;
    # no caller metadata is carried into the archive.
    info.pax_headers = {}
    return info


def _layer(records: list[tuple[str, bytes, bool]], bundle: bytes) -> bytes:
    entries: dict[str, tuple[bytes | None, bool]] = {
        "app": (None, False),
        "etc": (None, False),
        "etc/harmony": (None, False),
        "etc/harmony/bundle": (bundle, False),
    }
    for path, data, executable in records:
        full = "app/" + path
        parts = full.split("/")
        for index in range(1, len(parts)):
            directory = "/".join(parts[:index])
            existing = entries.get(directory)
            if existing is not None and existing[0] is not None:
                raise GuestImageError("artifact paths overlap a file and directory")
            entries[directory] = (None, False)
        if full in entries:
            raise GuestImageError("artifact paths are duplicated")
        entries[full] = (data, executable)

    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for name in sorted(entries):
            data, executable = entries[name]
            directory = data is None
            info = _tar_info(
                name,
                directory=directory,
                executable=executable,
                size=0 if directory else len(data),
            )
            archive.addfile(info, None if directory else io.BytesIO(data))
    return output.getvalue()


def package_artifacts(
    artifacts: tuple[build.Artifact, ...],
    bundle: bytes,
) -> bytes:
    """Return a deterministic Docker-save tarball for opaque guest bytes."""
    records = _validate_artifacts(artifacts)
    if type(bundle) is not bytes:
        raise GuestImageError("bundle must be bytes")
    if len(bundle) > MAX_BUNDLE_BYTES:
        raise GuestImageError("bundle exceeds the 16 KiB byte limit")

    layer = _layer(records, bundle)
    layer_digest = hashlib.sha256(layer).hexdigest()
    config = _json_bytes(
        {
            "architecture": "amd64",
            "config": {"Entrypoint": ["/app/app"], "Env": [], "WorkingDir": "/"},
            "os": "linux",
            "rootfs": {"diff_ids": ["sha256:" + layer_digest], "type": "layers"},
        }
    )
    config_name = hashlib.sha256(config).hexdigest() + ".json"
    manifest = _json_bytes([{"Config": config_name, "Layers": ["layer.tar"]}])
    outer_entries = {
        config_name: (config, False),
        "layer.tar": (layer, False),
        "manifest.json": (manifest, False),
    }
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for name in sorted(outer_entries):
            data, executable = outer_entries[name]
            archive.addfile(
                _tar_info(name, directory=False, executable=executable, size=len(data)),
                io.BytesIO(data),
            )
    return output.getvalue()


__all__ = ["GuestImageError", "package_artifacts"]
