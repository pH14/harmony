# SPDX-License-Identifier: AGPL-3.0-or-later
"""Freeze and verify paired skill-evaluation materials.

The orchestrator supplies every input explicitly.  This module copies those
inputs into two read-only arms and records enough content to verify that the
arms still describe the same cohort.  The read-only bits are artifact hygiene;
they are not an isolation boundary, and the digests are consistency checks,
not signatures.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import shutil
import stat
import tempfile
from pathlib import Path, PurePosixPath, PureWindowsPath
from typing import Any, Iterable


FileData = tuple[bytes, int]


VERSION = 1
_MANIFEST_NAME = "manifest.json"
_TASK_NAME = "TASK.txt"
_SETTINGS_NAME = "SETTINGS.json"
_TREATMENT_PREFIX = (".agent", "skills")
_DOMAIN = b"harmony-skill-materials-v1" + bytes([0])
_CHUNK_SIZE = 64 * 1024
_RESERVED = frozenset({_TASK_NAME, _SETTINGS_NAME, ".agent"})


class MaterialError(ValueError):
    """Raised when material inputs or a frozen pair are invalid."""


def _as_text_path(value: os.PathLike[str] | str, what: str) -> str:
    try:
        value = os.fspath(value)
    except TypeError as exc:
        raise MaterialError(f"{what} must be a path") from exc
    if isinstance(value, bytes):
        raise MaterialError(f"{what} must be a text path")
    return value


def _mapping_key(value: os.PathLike[str] | str) -> tuple[str, ...]:
    raw = _as_text_path(value, "mapping destination")
    if not raw or chr(0) in raw or chr(92) in raw:
        raise MaterialError("mapping destinations must be canonical relative paths")
    if PurePosixPath(raw).is_absolute() or PureWindowsPath(raw).drive:
        raise MaterialError("mapping destinations must be relative")
    parts = tuple(raw.split("/"))
    if not parts or any(not part or part in {".", ".."} for part in parts):
        raise MaterialError("mapping destinations cannot contain traversal")
    if PurePosixPath(raw).as_posix() != raw:
        raise MaterialError("mapping destinations must be canonical")
    if any(part in _RESERVED for part in parts):
        raise MaterialError("mapping destination uses a reserved name")
    return parts


def _relative_path(parts: Iterable[str]) -> str:
    return PurePosixPath(*parts).as_posix()


def _check_mapping_overlaps(mapping: dict[Any, Any]) -> list[tuple[tuple[str, ...], Path]]:
    if not isinstance(mapping, dict):
        raise MaterialError("source mappings must be dictionaries")
    entries: list[tuple[tuple[str, ...], Path]] = []
    for key, source in mapping.items():
        parts = _mapping_key(key)
        entries.append((parts, Path(_as_text_path(source, "source"))))
    entries.sort(key=lambda item: item[0])
    for previous, current in zip(entries, entries[1:]):
        if current[0][: len(previous[0])] == previous[0]:
            raise MaterialError("mapping destinations overlap")
    return entries


def _path_signature(st: os.stat_result) -> tuple[int, int, int, int, int, int]:
    return (
        st.st_mode,
        st.st_dev,
        st.st_ino,
        st.st_size,
        st.st_mtime_ns,
        st.st_ctime_ns,
    )


def _source_absolute(path: Path) -> Path:
    raw = _as_text_path(path, "source")
    if chr(0) in raw:
        raise MaterialError("source path contains NUL")
    pieces = Path(raw).parts
    if any(piece in {".", ".."} for piece in pieces):
        raise MaterialError("source paths cannot contain traversal")
    return Path(os.path.abspath(raw))


def _lstat_source(path: Path, *, final: bool = True) -> os.stat_result:
    absolute = _source_absolute(path)
    current = Path(absolute.anchor) if absolute.anchor else Path.cwd()
    components = absolute.parts[1:] if absolute.is_absolute() else absolute.parts
    for index, component in enumerate(components):
        current /= component
        try:
            st = os.lstat(current)
        except OSError as exc:
            raise MaterialError(f"source is unavailable: {path}") from exc
        if stat.S_ISLNK(st.st_mode):
            raise MaterialError(f"source contains a symlink: {path}")
        if index + 1 < len(components) and not stat.S_ISDIR(st.st_mode):
            raise MaterialError(f"source parent is not a directory: {path}")
    if not components:
        st = os.lstat(current)
    if final and not (stat.S_ISREG(st.st_mode) or stat.S_ISDIR(st.st_mode)):
        raise MaterialError(f"source is not a regular file or directory: {path}")
    return st


def _kind(st: os.stat_result) -> str:
    if stat.S_ISREG(st.st_mode):
        return "file"
    if stat.S_ISDIR(st.st_mode):
        return "directory"
    if stat.S_ISLNK(st.st_mode):
        return "symlink"
    return "special"


def _source_snapshot(path: Path) -> tuple[tuple[str, str, tuple[int, ...]], ...]:
    root = _source_absolute(path)
    _lstat_source(root)
    records: list[tuple[str, str, tuple[int, ...]]] = []

    def visit(current: Path, relative: tuple[str, ...]) -> None:
        try:
            st = os.lstat(current)
        except OSError as exc:
            raise MaterialError(f"source changed while inspecting: {path}") from exc
        kind = _kind(st)
        if kind == "symlink":
            raise MaterialError(f"source contains a symlink: {path}")
        if kind not in {"file", "directory"}:
            raise MaterialError(f"source contains a special file: {path}")
        records.append((_relative_path(relative) if relative else ".", kind, _path_signature(st)))
        if kind == "directory":
            try:
                with os.scandir(current) as iterator:
                    children = sorted(iterator, key=lambda entry: entry.name)
            except OSError as exc:
                raise MaterialError(f"source changed while inspecting: {path}") from exc
            for entry in children:
                if chr(92) in entry.name or chr(0) in entry.name:
                    raise MaterialError("source contains a non-portable file name")
                visit(Path(entry.path), relative + (entry.name,))

    visit(root, ())
    return tuple(records)


def _source_snapshots(
    entries: list[tuple[str, Path]],
) -> tuple[tuple[str, tuple[tuple[str, str, tuple[int, ...]], ...]], ...]:
    return tuple((label, _source_snapshot(path)) for label, path in entries)


def _stable_read(path: Path) -> FileData:
    _lstat_source(path)
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(_source_absolute(path), flags)
    except OSError as exc:
        raise MaterialError(f"source cannot be opened: {path}") from exc
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode):
            raise MaterialError(f"source is not a regular file: {path}")
        chunks: list[bytes] = []
        while True:
            chunk = os.read(descriptor, _CHUNK_SIZE)
            if not chunk:
                break
            chunks.append(chunk)
        data = b"".join(chunks)
        first_hash = hashlib.sha256(data).digest()
        os.lseek(descriptor, 0, os.SEEK_SET)
        second_hash = hashlib.sha256()
        while True:
            chunk = os.read(descriptor, _CHUNK_SIZE)
            if not chunk:
                break
            second_hash.update(chunk)
        after = os.fstat(descriptor)
        if _path_signature(before) != _path_signature(after) or first_hash != second_hash.digest():
            raise MaterialError(f"source mutated while copying: {path}")
        executable_bits = before.st_mode & 0o111
    except OSError as exc:
        raise MaterialError(f"source changed while copying: {path}") from exc
    finally:
        os.close(descriptor)
    try:
        final = os.lstat(_source_absolute(path))
    except OSError as exc:
        raise MaterialError(f"source changed while copying: {path}") from exc
    if _path_signature(final) != _path_signature(before):
        raise MaterialError(f"source mutated while copying: {path}")
    return data, executable_bits


def _output_is_safe(parts: tuple[str, ...], prefix: tuple[str, ...]) -> None:
    if len(parts) < len(prefix) or parts[: len(prefix)] != prefix:
        raise MaterialError("invalid output path")
    suffix = parts[len(prefix) :]
    if not suffix or any(part in _RESERVED for part in suffix):
        raise MaterialError("source contains a reserved output name")


def _source_plan(
    entries: list[tuple[tuple[str, ...], Path]],
    prefix: tuple[str, ...],
) -> tuple[dict[str, FileData], set[str]]:
    files: dict[str, FileData] = {}
    directories: set[str] = set()

    def walk(source: Path, output: tuple[str, ...]) -> None:
        st = _lstat_source(source)
        _output_is_safe(output, prefix)
        output_name = _relative_path(output)
        if stat.S_ISREG(st.st_mode):
            if output_name in files or output_name in directories:
                raise MaterialError("source mappings collide")
            files[output_name] = _stable_read(source)
            return
        directories.add(output_name)
        try:
            with os.scandir(source) as iterator:
                children = sorted(iterator, key=lambda entry: entry.name)
        except OSError as exc:
            raise MaterialError(f"source changed while copying: {source}") from exc
        for entry in children:
            if chr(92) in entry.name or chr(0) in entry.name:
                raise MaterialError("source contains a non-portable file name")
            walk(Path(entry.path), output + (entry.name,))

    for destination, source in entries:
        walk(_source_absolute(source), prefix + destination)
    return files, directories


def _validate_settings_value(value: Any, stack: set[int] | None = None) -> None:
    if stack is None:
        stack = set()
    if value is None or isinstance(value, (bool, int, str)):
        return
    if isinstance(value, float):
        if not math.isfinite(value):
            raise MaterialError("settings must contain finite numbers")
        return
    if isinstance(value, (list, tuple)):
        marker = id(value)
        if marker in stack:
            raise MaterialError("settings cannot contain cycles")
        stack.add(marker)
        try:
            for item in value:
                _validate_settings_value(item, stack)
        finally:
            stack.remove(marker)
        return
    if isinstance(value, dict):
        marker = id(value)
        if marker in stack:
            raise MaterialError("settings cannot contain cycles")
        stack.add(marker)
        try:
            for key, item in value.items():
                if not isinstance(key, str):
                    raise MaterialError("settings keys must be strings")
                _validate_settings_value(item, stack)
        finally:
            stack.remove(marker)
        return
    raise MaterialError("settings must be JSON-serializable")


def _canonical_settings(value: Any) -> bytes:
    _validate_settings_value(value)
    try:
        return json.dumps(
            value,
            ensure_ascii=False,
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
    except (TypeError, UnicodeError, ValueError) as exc:
        raise MaterialError("settings are not canonically serializable") from exc


def _canonical_manifest(value: dict[str, Any]) -> bytes:
    try:
        return json.dumps(value, ensure_ascii=True, allow_nan=False, sort_keys=True, separators=(",", ":")).encode(
            "ascii"
        )
    except (TypeError, ValueError) as exc:
        raise MaterialError("manifest is not canonical JSON") from exc


def _length_bytes(length: int) -> bytes:
    if length < 0 or length >= 1 << 64:
        raise MaterialError("material is too large")
    return length.to_bytes(8, "big")


def _feed(hasher: Any, data: bytes) -> None:
    hasher.update(_length_bytes(len(data)))
    hasher.update(data)


def _arm_digest(files: dict[str, FileData], directories: set[str]) -> str:
    hasher = hashlib.sha256(_DOMAIN + b"arm" + bytes([0]))
    _feed(hasher, b"directories")
    for name in sorted(directories):
        _feed(hasher, name.encode("utf-8"))
    _feed(hasher, b"files")
    for name in sorted(files):
        _feed(hasher, name.encode("utf-8"))
        _feed(hasher, _length_bytes(files[name][1]))
        _feed(hasher, files[name][0])
    return hasher.hexdigest()


def _cohort_digest(
    prompt: bytes,
    settings: bytes,
    common: dict[str, FileData],
    treatment: dict[str, FileData],
    common_dirs: set[str],
    treatment_dirs: set[str],
) -> str:
    hasher = hashlib.sha256(_DOMAIN + b"cohort" + bytes([0]))
    for label, data in ((b"prompt", prompt), (b"settings", settings)):
        _feed(hasher, label)
        _feed(hasher, data)
    for label, directories in ((b"common_dirs", common_dirs), (b"treatment_dirs", treatment_dirs)):
        _feed(hasher, label)
        for name in sorted(directories):
            _feed(hasher, name.encode("utf-8"))
    for label, files in ((b"common", common), (b"treatment", treatment)):
        _feed(hasher, label)
        for name in sorted(files):
            _feed(hasher, name.encode("utf-8"))
            _feed(hasher, _length_bytes(files[name][1]))
            _feed(hasher, files[name][0])
    return hasher.hexdigest()


def _records(files: dict[str, FileData]) -> list[dict[str, Any]]:
    return [
        {
            "path": name,
            "sha256": hashlib.sha256(files[name][0]).hexdigest(),
            "mode": files[name][1],
        }
        for name in sorted(files)
    ]


def _parents(paths: Iterable[str]) -> set[str]:
    result: set[str] = set()
    for name in paths:
        parts = PurePosixPath(name).parts
        for count in range(1, len(parts)):
            result.add(_relative_path(parts[:count]))
    return result


def _write_plan(root: Path, files: dict[str, FileData], directories: set[str]) -> None:
    root.mkdir(parents=True, exist_ok=False)
    all_directories = set(directories)
    all_directories |= _parents(all_directories)
    all_directories |= _parents(files)
    for name in sorted(all_directories, key=lambda item: (len(PurePosixPath(item).parts), item)):
        target = root.joinpath(*PurePosixPath(name).parts)
        target.mkdir()
    for name in sorted(files):
        target = root.joinpath(*PurePosixPath(name).parts)
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("xb") as stream:
            stream.write(files[name][0])
        os.chmod(target, 0o644 | files[name][1])


def _freeze_read_only(root: Path) -> None:
    for current, directories, files in os.walk(root, topdown=False, followlinks=False):
        for name in files:
            target = Path(current) / name
            os.chmod(target, 0o444 | (os.lstat(target).st_mode & 0o111))
        for name in directories:
            os.chmod(Path(current) / name, 0o555)
    os.chmod(root, 0o555)


def _remove_tree(path: Path) -> None:
    if not os.path.lexists(path):
        return
    for current, directories, files in os.walk(path, topdown=False, followlinks=False):
        for name in files:
            candidate = Path(current) / name
            try:
                if not os.path.islink(candidate):
                    os.chmod(candidate, 0o600)
            except OSError:
                pass
        for name in directories:
            candidate = Path(current) / name
            try:
                if not os.path.islink(candidate):
                    os.chmod(candidate, 0o700)
            except OSError:
                pass
    try:
        os.chmod(path, 0o700)
    except OSError:
        pass
    shutil.rmtree(path, ignore_errors=True)


def _validate_destination(destination: Path) -> tuple[Path, Path]:
    raw = _as_text_path(destination, "destination")
    if chr(0) in raw:
        raise MaterialError("destination contains NUL")
    path = Path(raw)
    if path.exists() or os.path.lexists(path):
        raise MaterialError("destination must not already exist")
    parent = path.parent
    if not parent.exists() or not parent.is_dir() or parent.is_symlink():
        raise MaterialError("destination parent must be an existing directory")
    return path, parent


def freeze_pair(
    destination: Path,
    common: dict[str, Path],
    skills: dict[str, Path],
    prompt: str,
    settings: dict[str, Any],
) -> dict[str, Any]:
    """Freeze a pair of evaluation arms and return its version-1 manifest."""
    destination, parent = _validate_destination(destination)
    if not isinstance(prompt, str):
        raise MaterialError("prompt must be a string")
    if not isinstance(settings, dict):
        raise MaterialError("settings must be a dictionary")
    try:
        prompt_bytes = prompt.encode("utf-8")
    except UnicodeError as exc:
        raise MaterialError("prompt must be valid UTF-8") from exc
    settings_bytes = _canonical_settings(settings)
    common_entries = _check_mapping_overlaps(common)
    treatment_entries = _check_mapping_overlaps(skills)
    source_entries = [
        (f"common:{_relative_path(key)}", source) for key, source in common_entries
    ] + [
        (f"treatment:{_relative_path(key)}", source) for key, source in treatment_entries
    ]
    before = _source_snapshots(source_entries)
    common_files, common_dirs = _source_plan(common_entries, ())
    treatment_files, treatment_dirs = _source_plan(treatment_entries, _TREATMENT_PREFIX)
    after = _source_snapshots(source_entries)
    if before != after:
        raise MaterialError("source mutated while copying")

    docs_files = dict(common_files)
    docs_files[_TASK_NAME] = (prompt_bytes, 0)
    docs_files[_SETTINGS_NAME] = (settings_bytes, 0)
    skills_files = dict(docs_files)
    skills_files.update(treatment_files)
    if treatment_dirs:
        treatment_dirs = set(treatment_dirs) | {
            _relative_path((_TREATMENT_PREFIX[0],)),
            _relative_path(_TREATMENT_PREFIX),
        }

    manifest: dict[str, Any] = {
        "version": VERSION,
        "common": _records(common_files),
        "common_dirs": sorted(common_dirs),
        "treatment": _records(treatment_files),
        "treatment_dirs": sorted(treatment_dirs),
        "cohort_sha256": _cohort_digest(
            prompt_bytes,
            settings_bytes,
            common_files,
            treatment_files,
            set(common_dirs),
            set(treatment_dirs),
        ),
        "docs_sha256": _arm_digest(docs_files, set(common_dirs) | _parents(docs_files)),
        "skills_sha256": _arm_digest(
            skills_files,
            set(common_dirs) | set(treatment_dirs) | _parents(skills_files),
        ),
    }
    manifest_bytes = _canonical_manifest(manifest)
    stage = Path(tempfile.mkdtemp(prefix=f".{destination.name}.", dir=parent))
    try:
        _write_plan(stage / "docs", docs_files, common_dirs)
        _write_plan(stage / "skills", skills_files, set(common_dirs) | set(treatment_dirs))
        with (stage / _MANIFEST_NAME).open("xb") as stream:
            stream.write(manifest_bytes)
        after_write = _source_snapshots(source_entries)
        if after_write != before:
            raise MaterialError("source mutated while copying")
        _freeze_read_only(stage)
        if os.path.lexists(destination):
            raise MaterialError("destination appeared while freezing")
        os.rename(stage, destination)
    except Exception:
        _remove_tree(stage)
        raise
    return manifest


def _strict_object_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise MaterialError("JSON contains duplicate keys")
        result[key] = value
    return result


def _load_manifest(path: Path) -> dict[str, Any]:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise MaterialError("manifest cannot be read") from exc
    try:
        value = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_strict_object_pairs,
            parse_constant=lambda token: (_ for _ in ()).throw(MaterialError(f"invalid JSON constant {token}")),
        )
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise MaterialError("manifest is not valid JSON") from exc
    if not isinstance(value, dict) or _canonical_manifest(value) != raw:
        raise MaterialError("manifest is not canonical JSON")
    expected = {
        "version",
        "common",
        "common_dirs",
        "treatment",
        "treatment_dirs",
        "cohort_sha256",
        "docs_sha256",
        "skills_sha256",
    }
    if (
        set(value) != expected
        or not isinstance(value["version"], int)
        or isinstance(value["version"], bool)
        or value["version"] != VERSION
    ):
        raise MaterialError("unsupported manifest shape")
    return value


def _manifest_entries(value: Any, *, treatment: bool) -> dict[str, tuple[str, int]]:
    if not isinstance(value, list):
        raise MaterialError("manifest file list is invalid")
    result: dict[str, tuple[str, int]] = {}
    prefix = _TREATMENT_PREFIX if treatment else ()
    for record in value:
        if not isinstance(record, dict) or set(record) != {"path", "sha256", "mode"}:
            raise MaterialError("manifest file record is invalid")
        raw = record["path"]
        if not isinstance(raw, str) or chr(0) in raw or chr(92) in raw:
            raise MaterialError("manifest path is invalid")
        parts = tuple(raw.split("/"))
        if not parts or any(not part or part in {".", ".."} for part in parts):
            raise MaterialError("manifest path is invalid")
        if PurePosixPath(raw).as_posix() != raw:
            raise MaterialError("manifest path is not canonical")
        if treatment:
            if len(parts) <= len(prefix) or parts[: len(prefix)] != prefix:
                raise MaterialError("manifest treatment path is invalid")
            _output_is_safe(parts, prefix)
        elif any(part in _RESERVED for part in parts):
            raise MaterialError("manifest common path is reserved")
        digest = record["sha256"]
        if not isinstance(digest, str) or len(digest) != 64 or any(
            character not in "0123456789abcdef" for character in digest
        ):
            raise MaterialError("manifest hash is invalid")
        mode = record["mode"]
        if (
            not isinstance(mode, int)
            or isinstance(mode, bool)
            or mode < 0
            or mode & ~0o111
        ):
            raise MaterialError("manifest executable mode is invalid")
        if raw in result:
            raise MaterialError("manifest contains duplicate paths")
        result[raw] = (digest, mode)
    if list(result) != sorted(result):
        raise MaterialError("manifest paths are not sorted")
    return result


def _manifest_dirs(value: Any, *, treatment: bool) -> set[str]:
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        raise MaterialError("manifest directory list is invalid")
    if value != sorted(set(value)) or len(value) != len(set(value)):
        raise MaterialError("manifest directories are not sorted")
    result: set[str] = set()
    prefix = _TREATMENT_PREFIX if treatment else ()
    for raw in value:
        parts = tuple(raw.split("/"))
        if not raw or chr(0) in raw or chr(92) in raw or any(part in {".", "..", ""} for part in parts):
            raise MaterialError("manifest directory path is invalid")
        if PurePosixPath(raw).as_posix() != raw:
            raise MaterialError("manifest directory path is not canonical")
        if treatment:
            if len(parts) < len(prefix):
                if parts != prefix[: len(parts)]:
                    raise MaterialError("manifest treatment directory is invalid")
            elif parts[: len(prefix)] != prefix:
                raise MaterialError("manifest treatment directory is invalid")
            if len(parts) > len(prefix):
                _output_is_safe(parts, prefix)
        elif any(part in _RESERVED for part in parts):
            raise MaterialError("manifest common directory is reserved")
        result.add(raw)
    return result


def _collect_arm(root: Path) -> tuple[dict[str, FileData], set[str]]:
    files: dict[str, FileData] = {}
    directories: set[str] = set()

    def visit(current: Path, relative: tuple[str, ...]) -> None:
        try:
            st = os.lstat(current)
        except OSError as exc:
            raise MaterialError("frozen arm changed while verifying") from exc
        kind = _kind(st)
        if kind == "symlink":
            raise MaterialError("frozen arm contains a symlink")
        if kind == "special":
            raise MaterialError("frozen arm contains a special file")
        if st.st_mode & 0o222:
            raise MaterialError("frozen arm is not read-only")
        if kind == "file":
            name = _relative_path(relative)
            if name in files or name in directories:
                raise MaterialError("frozen arm has duplicate paths")
            files[name] = _stable_read(current)
            return
        if relative:
            directories.add(_relative_path(relative))
        try:
            with os.scandir(current) as iterator:
                children = sorted(iterator, key=lambda entry: entry.name)
        except OSError as exc:
            raise MaterialError("frozen arm changed while verifying") from exc
        for entry in children:
            if chr(92) in entry.name or chr(0) in entry.name:
                raise MaterialError("frozen arm has a non-portable name")
            visit(Path(entry.path), relative + (entry.name,))

    visit(root, ())
    return files, directories


def _check_arm(
    root: Path,
    expected_hashes: dict[str, tuple[str, int]],
    expected_dirs: set[str],
    *,
    treatment: bool,
) -> dict[str, FileData]:
    files, actual_dirs = _collect_arm(root)
    expected_files = set(expected_hashes) | {_TASK_NAME, _SETTINGS_NAME}
    if set(files) != expected_files:
        raise MaterialError("frozen arm has missing or extra files")
    if actual_dirs != expected_dirs | _parents(expected_files):
        raise MaterialError("frozen arm has missing or extra directories")
    for name, (digest, mode) in expected_hashes.items():
        if hashlib.sha256(files[name][0]).hexdigest() != digest or files[name][1] != mode:
            raise MaterialError("frozen arm file hash mismatch")
    for name in (_TASK_NAME, _SETTINGS_NAME):
        if files[name][1] != 0:
            raise MaterialError("generated file has executable bits")
    if treatment:
        for name in expected_hashes:
            if name.startswith(".agent/skills/"):
                _output_is_safe(tuple(name.split("/")), _TREATMENT_PREFIX)
    return files


def verify_pair(path: Path) -> dict[str, Any]:
    """Recompute and validate a frozen pair, returning its manifest."""
    root = Path(_as_text_path(path, "pair"))
    if not root.exists() or not root.is_dir() or root.is_symlink():
        raise MaterialError("pair must be a directory")
    if os.lstat(root).st_mode & 0o222:
        raise MaterialError("pair is not read-only")
    try:
        with os.scandir(root) as iterator:
            root_entries = sorted(iterator, key=lambda entry: entry.name)
    except OSError as exc:
        raise MaterialError("pair cannot be inspected") from exc
    if {entry.name for entry in root_entries} != {_MANIFEST_NAME, "docs", "skills"}:
        raise MaterialError("pair has missing or extra root entries")
    for entry in root_entries:
        st = os.lstat(entry.path)
        kind = _kind(st)
        if kind == "symlink" or kind == "special":
            raise MaterialError("pair contains a symlink or special file")
        if st.st_mode & 0o222:
            raise MaterialError("pair is not read-only")
    manifest_path = root / _MANIFEST_NAME
    try:
        manifest_stat = os.lstat(manifest_path)
    except OSError as exc:
        raise MaterialError("pair has no manifest") from exc
    if not stat.S_ISREG(manifest_stat.st_mode):
        raise MaterialError("pair manifest is not a regular file")
    manifest = _load_manifest(manifest_path)
    common_hashes = _manifest_entries(manifest["common"], treatment=False)
    treatment_hashes = _manifest_entries(manifest["treatment"], treatment=True)
    common_dirs = _manifest_dirs(manifest["common_dirs"], treatment=False)
    treatment_dirs = _manifest_dirs(manifest["treatment_dirs"], treatment=True)
    docs_root = root / "docs"
    skills_root = root / "skills"
    for arm in (docs_root, skills_root):
        if arm.is_symlink() or not arm.is_dir():
            raise MaterialError("pair is missing an arm")
    docs = _check_arm(docs_root, common_hashes, common_dirs, treatment=False)
    skills = _check_arm(
        skills_root,
        {**common_hashes, **treatment_hashes},
        common_dirs | treatment_dirs,
        treatment=True,
    )
    for name in common_hashes:
        if docs[name] != skills[name]:
            raise MaterialError("common file differs between arms")
    if docs[_TASK_NAME] != skills[_TASK_NAME] or docs[_SETTINGS_NAME] != skills[_SETTINGS_NAME]:
        raise MaterialError("arm metadata differs")
    try:
        settings = json.loads(
            docs[_SETTINGS_NAME][0].decode("utf-8"),
            object_pairs_hook=_strict_object_pairs,
            parse_constant=lambda token: (_ for _ in ()).throw(MaterialError(f"invalid settings constant {token}")),
        )
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise MaterialError("settings are not valid JSON") from exc
    if not isinstance(settings, dict):
        raise MaterialError("settings must be a JSON object")
    if _canonical_settings(settings) != docs[_SETTINGS_NAME][0]:
        raise MaterialError("settings are not canonical JSON")
    common_bytes = {name: docs[name] for name in common_hashes}
    treatment_bytes = {name: skills[name] for name in treatment_hashes}
    expected_cohort = _cohort_digest(
        docs[_TASK_NAME][0],
        docs[_SETTINGS_NAME][0],
        common_bytes,
        treatment_bytes,
        common_dirs,
        treatment_dirs,
    )
    if expected_cohort != manifest["cohort_sha256"]:
        raise MaterialError("cohort hash mismatch")
    docs_dirs = common_dirs | _parents(docs)
    skills_dirs = common_dirs | treatment_dirs | _parents(skills)
    if (
        _arm_digest(docs, docs_dirs) != manifest["docs_sha256"]
        or _arm_digest(skills, skills_dirs) != manifest["skills_sha256"]
    ):
        raise MaterialError("arm hash mismatch")
    return manifest


__all__ = ["MaterialError", "freeze_pair", "verify_pair"]
