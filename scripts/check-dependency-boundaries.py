#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check first-party Cargo dependency edges against Harmony's architecture policy.

Cargo's package metadata is the source of truth for this check.  In particular,
the dependency records returned by ``cargo metadata`` retain optional,
target-specific, build, and development declarations after Cargo has resolved
workspace inheritance and dependency renames.  The checker therefore does not
parse dependency tables from manifests itself.

The repository may contain more than one Cargo workspace.  Workspace roots are
discovered from repository manifests, and a metadata query is also made for an
otherwise uncovered package manifest.  This catches new standalone crates as
well as crates added to an existing workspace.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10/3.9 on the portable quality hosts.
    import tomli as tomllib  # type: ignore[no-redef]
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


POLICY_VERSION = 1
DEFAULT_POLICY = Path(__file__).resolve().parent / "dependency-boundaries.toml"
IGNORED_MANIFEST_PARTS = {
    ".git",
    "target",
    "vendor",
    "vendored",
    "generated",
    "out",
    "build",
    "node_modules",
}
KNOWN_DEPENDENCY_KINDS = {"normal", "build", "dev"}


class BoundaryError(Exception):
    """A malformed policy, metadata response, or repository graph."""


@dataclass(frozen=True)
class Package:
    """One first-party package identified by Cargo identity and manifest path."""

    name: str
    package_id: str
    manifest_path: Path

    @property
    def label(self) -> str:
        return f"{self.name} ({self.manifest_path})"


@dataclass(frozen=True)
class Dependency:
    source: Package
    destination_manifest: Path
    name: str
    rename: str | None
    kind: str
    optional: bool
    target: str | None

    @property
    def label(self) -> str:
        alias = f" as {self.rename}" if self.rename else ""
        target = f", target {self.target}" if self.target else ""
        optional = ", optional" if self.optional else ""
        return f"{self.kind}{optional}{target} dependency {self.name}{alias}"


@dataclass
class Policy:
    groups: dict[str, set[Path]]
    allowed: dict[str, set[str]]
    exceptions: list[dict[str, Any]]


def _canonical(path: Path) -> Path:
    """Resolve a path without requiring a path to exist."""

    return path.expanduser().resolve(strict=False)


def _inside(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
    except ValueError:
        return False
    return True


def _is_ignored(path: Path, root: Path) -> bool:
    try:
        relative = path.relative_to(root)
    except ValueError:
        return True
    return any(part in IGNORED_MANIFEST_PARTS for part in relative.parts)


def discover_manifests(repo_root: Path) -> list[Path]:
    """Return repository Cargo manifests, excluding generated/vendored trees."""

    root = _canonical(repo_root)
    manifests = []
    for path in root.rglob("Cargo.toml"):
        if path.is_file() and not _is_ignored(path, root):
            manifests.append(_canonical(path))
    return sorted(manifests)


def _toml_table(path: Path) -> dict[str, Any]:
    try:
        with path.open("rb") as stream:
            value = tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise BoundaryError(f"{path}: cannot parse Cargo.toml: {exc}") from exc
    if not isinstance(value, dict):
        raise BoundaryError(f"{path}: Cargo.toml did not contain a table")
    return value


def _is_workspace_root(path: Path) -> bool:
    # ``[workspace.dependencies]`` is inherited by a package but does not make
    # that package a workspace root.  Match Cargo's exact table header rather
    # than merely looking for a top-level ``workspace`` key in parsed TOML.
    try:
        return any(line.strip() == "[workspace]" for line in path.read_text().splitlines())
    except OSError as exc:
        raise BoundaryError(f"{path}: cannot read Cargo.toml: {exc}") from exc


def _is_package_manifest(path: Path) -> bool:
    return isinstance(_toml_table(path).get("package"), dict)


def _run_metadata(cargo: str, manifest: Path, repo_root: Path) -> dict[str, Any]:
    command = [
        cargo,
        "metadata",
        "--format-version",
        "1",
        "--no-deps",
        "--offline",
        "--manifest-path",
        str(manifest),
    ]
    try:
        completed = subprocess.run(
            command,
            cwd=repo_root,
            text=True,
            capture_output=True,
            check=False,
        )
    except OSError as exc:
        raise BoundaryError(f"{manifest}: could not run {cargo!r}: {exc}") from exc
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip() or "no cargo output"
        raise BoundaryError(
            f"{manifest}: cargo metadata failed with exit {completed.returncode}: {detail}"
        )
    try:
        metadata = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise BoundaryError(f"{manifest}: cargo metadata returned invalid JSON: {exc}") from exc
    if not isinstance(metadata, dict) or not isinstance(metadata.get("packages"), list):
        raise BoundaryError(f"{manifest}: cargo metadata JSON has no packages array")
    return metadata


def _metadata_package(item: Any, root: Path) -> Package:
    if not isinstance(item, dict):
        raise BoundaryError(f"{root}: cargo metadata contained a non-object package")
    name = item.get("name")
    package_id = item.get("id")
    manifest = item.get("manifest_path")
    if not all(isinstance(value, str) and value for value in (name, package_id, manifest)):
        raise BoundaryError(f"{root}: package metadata is missing name, id, or manifest_path")
    return Package(name=name, package_id=package_id, manifest_path=_canonical(Path(manifest)))


def collect_metadata(
    repo_root: Path, manifests: Iterable[Path], cargo: str = "cargo"
) -> tuple[dict[Path, Package], dict[Path, dict[str, Any]]]:
    """Query each workspace and uncovered package, returning merged metadata.

    ``cargo metadata --no-deps`` intentionally does not include path packages
    outside the selected workspace.  Those manifests are queried separately,
    then all package records are merged by canonical manifest path.
    """

    root = _canonical(repo_root)
    all_manifests = list(manifests)
    roots = [path for path in all_manifests if _is_workspace_root(path)]
    if not roots:
        roots = [path for path in all_manifests if _is_package_manifest(path)]

    packages: dict[Path, Package] = {}
    package_records: dict[Path, dict[str, Any]] = {}
    queried: set[Path] = set()

    def merge(metadata: dict[str, Any], query: Path) -> None:
        for raw in metadata["packages"]:
            package = _metadata_package(raw, query)
            if not _inside(package.manifest_path, root):
                # A path dependency can point at a sibling checkout.  It is
                # outside this repository's ownership policy and is not a
                # crate that this check should classify.
                continue
            previous = packages.get(package.manifest_path)
            if previous is not None and (
                previous.name != package.name or previous.package_id != package.package_id
            ):
                raise BoundaryError(
                    f"{package.manifest_path}: Cargo metadata gave conflicting package "
                    f"identities ({previous.name}/{previous.package_id} vs "
                    f"{package.name}/{package.package_id})"
                )
            packages[package.manifest_path] = package
            package_records[package.manifest_path] = raw

    for workspace_root in roots:
        queried.add(workspace_root)
        merge(_run_metadata(cargo, workspace_root, root), workspace_root)

    # Package manifests not returned by a workspace query are standalone
    # packages (including path dependencies that live in a different
    # workspace).  Query them directly.  This also makes fixture repositories
    # with a single package and no [workspace] table useful in tests.
    for manifest in all_manifests:
        if not _is_package_manifest(manifest) or manifest in packages:
            continue
        queried.add(manifest)
        merge(_run_metadata(cargo, manifest, root), manifest)

    # If a workspace metadata query returned no package for a package manifest,
    # a direct query gives Cargo's useful diagnostic (and supports unusual
    # workspace globs).  It also avoids silently treating a source manifest as
    # generated output.
    for manifest in all_manifests:
        if _is_package_manifest(manifest) and manifest not in packages and manifest not in queried:
            queried.add(manifest)
            merge(_run_metadata(cargo, manifest, root), manifest)

    return packages, package_records


def _policy_path(root: Path, value: Any, field: str) -> Path:
    if not isinstance(value, str) or not value:
        raise BoundaryError(f"policy {field}: expected a non-empty manifest path")
    path = _canonical(Path(value) if Path(value).is_absolute() else root / value)
    if path.is_dir() or path.suffix != ".toml":
        path = _canonical(path / "Cargo.toml")
    if not _inside(path, root):
        raise BoundaryError(f"policy {field}: path {value!r} is outside repository {root}")
    return path


def load_policy(policy_path: Path, repo_root: Path) -> Policy:
    try:
        with policy_path.open("rb") as stream:
            raw = tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise BoundaryError(f"{policy_path}: cannot parse dependency policy: {exc}") from exc
    if raw.get("version") != POLICY_VERSION:
        raise BoundaryError(
            f"{policy_path}: unsupported policy version {raw.get('version')!r}; "
            f"expected {POLICY_VERSION}"
        )

    raw_groups = raw.get("groups")
    raw_allowed = raw.get("allowed")
    if not isinstance(raw_groups, dict) or not isinstance(raw_allowed, dict):
        raise BoundaryError(f"{policy_path}: expected [groups] and [allowed] tables")

    groups: dict[str, set[Path]] = {}
    for group, values in raw_groups.items():
        if not isinstance(group, str) or not isinstance(values, list):
            raise BoundaryError(f"{policy_path}: group names and members must be strings/lists")
        members: set[Path] = set()
        for index, value in enumerate(values):
            member = _policy_path(repo_root, value, f"groups.{group}[{index}]")
            if member in members:
                raise BoundaryError(
                    f"{policy_path}: groups.{group} lists {member} more than once"
                )
            members.add(member)
        groups[group] = members

    allowed: dict[str, set[str]] = {}
    for group, values in raw_allowed.items():
        if not isinstance(group, str) or not isinstance(values, list) or not all(
            isinstance(value, str) for value in values
        ):
            raise BoundaryError(f"{policy_path}: allowed group edges must be string lists")
        allowed[group] = set(values)

    unknown_allowed_sources = sorted(set(allowed) - set(groups))
    unknown_allowed_destinations = sorted(
        {destination for values in allowed.values() for destination in values} - set(groups)
    )
    missing_allowed = sorted(set(groups) - set(allowed))
    if unknown_allowed_sources or unknown_allowed_destinations or missing_allowed:
        details = []
        if unknown_allowed_sources:
            details.append(f"unknown sources {unknown_allowed_sources}")
        if unknown_allowed_destinations:
            details.append(f"unknown destinations {unknown_allowed_destinations}")
        if missing_allowed:
            details.append(f"groups without allowed edges {missing_allowed}")
        raise BoundaryError(f"{policy_path}: invalid allowed edges: {'; '.join(details)}")

    raw_exceptions = raw.get("exceptions", [])
    if not isinstance(raw_exceptions, list):
        raise BoundaryError(f"{policy_path}: exceptions must be an array of tables")
    exceptions: list[dict[str, Any]] = []
    for index, exception in enumerate(raw_exceptions):
        if not isinstance(exception, dict):
            raise BoundaryError(f"{policy_path}: exceptions[{index}] must be a table")
        source = _policy_path(repo_root, exception.get("source"), f"exceptions[{index}].source")
        destination = _policy_path(
            repo_root, exception.get("destination"), f"exceptions[{index}].destination"
        )
        kinds = exception.get("kinds")
        if not isinstance(kinds, list) or not kinds or not all(
            isinstance(kind, str) and kind in KNOWN_DEPENDENCY_KINDS for kind in kinds
        ):
            raise BoundaryError(
                f"{policy_path}: exceptions[{index}].kinds must list normal/build/dev"
            )
        reason = exception.get("reason")
        issue = exception.get("issue")
        if (
            not isinstance(reason, str)
            or not reason.strip()
            or not isinstance(issue, str)
            or not issue.strip()
        ):
            raise BoundaryError(
                f"{policy_path}: exceptions[{index}] needs non-empty reason and issue"
            )
        optional = exception.get("optional")
        if optional is not None and not isinstance(optional, bool):
            raise BoundaryError(f"{policy_path}: exceptions[{index}].optional must be boolean")
        target = exception.get("target")
        if target is not None and not isinstance(target, str):
            raise BoundaryError(f"{policy_path}: exceptions[{index}].target must be a string")
        rename = exception.get("rename")
        if rename is not None and not isinstance(rename, str):
            raise BoundaryError(f"{policy_path}: exceptions[{index}].rename must be a string")
        exceptions.append(
            {
                "source": source,
                "destination": destination,
                "kinds": set(kinds),
                "optional": optional,
                "target": target,
                "rename": rename,
                "reason": reason.strip(),
                "issue": issue.strip(),
                "index": index,
            }
        )

    return Policy(groups=groups, allowed=allowed, exceptions=exceptions)


def _classify(packages: dict[Path, Package], policy: Policy) -> tuple[dict[Path, str], list[str]]:
    membership: dict[Path, str] = {}
    errors: list[str] = []
    for group, paths in policy.groups.items():
        for path in paths:
            if path not in packages:
                errors.append(
                    f"policy group {group!r} references unrecognized first-party manifest {path}"
                )
            elif path in membership:
                errors.append(
                    f"manifest {path} is classified in both {membership[path]!r} and {group!r}"
                )
            else:
                membership[path] = group
    for path, package in sorted(packages.items()):
        if path not in membership:
            errors.append(
                f"unclassified first-party crate {package.name} at {path}; add it to a policy group"
            )
    return membership, errors


def _dependencies(
    packages: dict[Path, Package], records: dict[Path, dict[str, Any]], root: Path
) -> tuple[list[Dependency], list[str]]:
    dependencies: list[Dependency] = []
    errors: list[str] = []
    for manifest, package in sorted(packages.items()):
        raw_dependencies = records[manifest].get("dependencies", [])
        if not isinstance(raw_dependencies, list):
            errors.append(f"{package.label}: cargo metadata dependencies is not an array")
            continue
        for index, raw in enumerate(raw_dependencies):
            if not isinstance(raw, dict):
                errors.append(f"{package.label}: dependency {index} is not an object")
                continue
            path_value = raw.get("path")
            if path_value is None:
                continue  # Registry, git, or other external dependency.
            if not isinstance(path_value, str):
                errors.append(f"{package.label}: dependency {index} has a non-string path")
                continue
            destination = _canonical(Path(path_value))
            if destination.is_dir() or destination.suffix != ".toml":
                destination = _canonical(destination / "Cargo.toml")
            if not _inside(destination, root):
                continue  # A sibling checkout is outside this repository's policy.
            if _is_ignored(destination, root):
                # A path dependency into vendored/generated output is not a
                # repository-owned crate. It must still be built and audited
                # by the package that owns that output, but it is outside the
                # first-party architecture graph.
                continue
            kind = raw.get("kind") or "normal"
            if kind not in KNOWN_DEPENDENCY_KINDS:
                errors.append(f"{package.label}: dependency {index} has unknown kind {kind!r}")
                continue
            name = raw.get("name")
            if not isinstance(name, str) or not name:
                errors.append(f"{package.label}: dependency {index} has no name")
                continue
            dependencies.append(
                Dependency(
                    source=package,
                    destination_manifest=destination,
                    name=name,
                    rename=raw.get("rename") if isinstance(raw.get("rename"), str) else None,
                    kind=kind,
                    optional=bool(raw.get("optional", False)),
                    target=raw.get("target") if isinstance(raw.get("target"), str) else None,
                )
            )
    return dependencies, errors


def _exception_matches(exception: dict[str, Any], dependency: Dependency) -> bool:
    return (
        exception["source"] == dependency.source.manifest_path
        and exception["destination"] == dependency.destination_manifest
        and dependency.kind in exception["kinds"]
        and (exception["optional"] is None or exception["optional"] == dependency.optional)
        and (exception["target"] is None or exception["target"] == dependency.target)
        and (exception["rename"] is None or exception["rename"] == dependency.rename)
    )


def check_repository(
    repo_root: Path, policy_path: Path = DEFAULT_POLICY, cargo: str = "cargo"
) -> list[str]:
    """Return all policy violations; an empty list means the graph is valid."""

    root = _canonical(repo_root)
    policy = load_policy(_canonical(policy_path), root)
    manifests = discover_manifests(root)
    packages, records = collect_metadata(root, manifests, cargo)
    membership, errors = _classify(packages, policy)
    dependencies, dependency_errors = _dependencies(packages, records, root)
    errors.extend(dependency_errors)

    unknown_exception_refs: set[int] = set()
    for exception in policy.exceptions:
        for field in ("source", "destination"):
            path = exception[field]
            if path not in packages:
                unknown_exception_refs.add(exception["index"])
                errors.append(
                    f"policy exception {exception['index']} names unknown {field} "
                    f"manifest {path}"
                )

    for dependency in dependencies:
        destination = packages.get(dependency.destination_manifest)
        if destination is None:
            errors.append(
                f"{dependency.source.label}: {dependency.label} points at unrecognized "
                f"first-party manifest {dependency.destination_manifest}"
            )
            continue
        source_group = membership.get(dependency.source.manifest_path)
        destination_group = membership.get(dependency.destination_manifest)
        if source_group is None or destination_group is None:
            continue  # The unclassified diagnostics above are more actionable.
        matches = [
            exception
            for exception in policy.exceptions
            if _exception_matches(exception, dependency)
        ]
        if len(matches) > 1:
            errors.append(
                f"{dependency.source.label}: {dependency.label} has multiple matching "
                f"policy exceptions {[item['index'] for item in matches]}"
            )
        allowed = destination_group in policy.allowed[source_group]
        if not allowed and not matches:
            errors.append(
                f"forbidden {dependency.source.name} ({source_group}) -> "
                f"{destination.name} ({destination_group}): {dependency.label}; "
                f"allowed destinations for {source_group!r}: "
                f"{sorted(policy.allowed[source_group])}"
            )

    for exception in policy.exceptions:
        if exception["index"] in unknown_exception_refs:
            continue
        if not any(_exception_matches(exception, dependency) for dependency in dependencies):
            errors.append(
                f"policy exception {exception['index']} is stale or unknown: "
                f"{exception['source']} -> {exception['destination']} "
                f"kinds={sorted(exception['kinds'])}; {exception['issue']}: {exception['reason']}"
            )
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root (default: the parent of scripts/)",
    )
    parser.add_argument(
        "--policy",
        type=Path,
        default=DEFAULT_POLICY,
        help="dependency policy TOML (default: scripts/dependency-boundaries.toml)",
    )
    parser.add_argument(
        "--cargo",
        default=os.environ.get("CARGO", "cargo"),
        help="cargo executable used for metadata queries",
    )
    args = parser.parse_args(argv)
    try:
        errors = check_repository(args.repo_root, args.policy, args.cargo)
    except BoundaryError as exc:
        print(f"dependency boundary check failed: {exc}", file=sys.stderr)
        return 2
    if errors:
        print(f"dependency boundary check failed with {len(errors)} issue(s):", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print("dependency boundary check passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
