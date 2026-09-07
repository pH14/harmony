#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Focused, network-free tests for check-dependency-boundaries.py."""

from __future__ import annotations

import importlib.util
import json
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any


SCRIPT = Path(__file__).with_name("check-dependency-boundaries.py")
SPEC = importlib.util.spec_from_file_location("check_dependency_boundaries", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
CHECKER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CHECKER
SPEC.loader.exec_module(CHECKER)


class Fixture:
    """Small Cargo metadata repository driven by a local fake cargo command."""

    def __init__(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.metadata: dict[str, dict[str, Any]] = {}
        self.fake_cargo = self.root / "fake-cargo.py"
        self.mapping = self.root / "metadata.json"
        self.fake_cargo.write_text(
            """#!/usr/bin/env python3
import json
import sys
from pathlib import Path

manifest = Path(sys.argv[sys.argv.index('--manifest-path') + 1]).resolve()
mapping = json.loads(Path(__import__('os').environ['FAKE_METADATA']).read_text())
try:
    print(json.dumps(mapping[str(manifest)]))
except KeyError:
    print('missing fake metadata for ' + str(manifest), file=sys.stderr)
    raise SystemExit(17)
"""
        )
        self.fake_cargo.chmod(self.fake_cargo.stat().st_mode | stat.S_IXUSR)

    def close(self) -> None:
        self.temp.cleanup()

    def manifest(self, relative: str, package: str | None = None, workspace: bool = False) -> Path:
        directory = self.root / relative
        directory.mkdir(parents=True, exist_ok=True)
        path = directory / "Cargo.toml"
        if workspace:
            path.write_text("[workspace]\nmembers = []\n")
        elif package is not None:
            path.write_text(
                f'[package]\nname = "{package}"\nversion = "0.1.0"\nedition = "2021"\n'
            )
        return path

    def package(self, relative: str, name: str, workspace: bool = False) -> Path:
        path = self.manifest(relative, name, workspace=workspace)
        return path

    def metadata_for(self, query: Path, packages: list[dict[str, Any]]) -> None:
        self.metadata[str(query.resolve())] = {"packages": packages}

    def package_record(
        self, package: str, path: Path, dependencies: list[dict[str, Any]] | None = None
    ) -> dict[str, Any]:
        return {
            "name": package,
            "id": f"path+file://{path.parent}#{package}@0.1.0",
            "manifest_path": str(path.resolve()),
            "dependencies": dependencies or [],
        }

    def policy(
        self,
        groups: dict[str, list[str]],
        allowed: dict[str, list[str]],
        exceptions: str = "",
    ) -> Path:
        lines = ["version = 1", "", "[groups]"]
        for group, members in groups.items():
            values = ", ".join(json.dumps(member) for member in members)
            lines.append(f"{group} = [{values}]")
        lines += ["", "[allowed]"]
        for group, destinations in allowed.items():
            values = ", ".join(json.dumps(destination) for destination in destinations)
            lines.append(f"{group} = [{values}]")
        if exceptions:
            lines += ["", exceptions]
        policy = self.root / "dependency-boundaries.toml"
        policy.write_text("\n".join(lines) + "\n")
        self.mapping.write_text(json.dumps(self.metadata))
        return policy

    def check(self, policy: Path) -> list[str]:
        old = os.environ.get("FAKE_METADATA")
        os.environ["FAKE_METADATA"] = str(self.mapping)
        try:
            return CHECKER.check_repository(self.root, policy, str(self.fake_cargo))
        finally:
            if old is None:
                os.environ.pop("FAKE_METADATA", None)
            else:
                os.environ["FAKE_METADATA"] = old


class DependencyBoundaryTests(unittest.TestCase):
    def test_cross_workspace_alias_optional_target_build_and_dev_edges(self) -> None:
        fixture = Fixture()
        self.addCleanup(fixture.close)
        workspace = fixture.manifest("workspace", workspace=True)
        workspace.write_text('[workspace]\nmembers = ["source"]\n')
        source = fixture.package("workspace/source", "source")
        destination = fixture.package("standalone/destination", "destination")
        destination_record = fixture.package_record("destination", destination)
        dependencies = [
            {
                "name": "destination_alias",
                "rename": "destination",
                "path": str(destination.parent),
                "kind": None,
                "optional": True,
                "target": "cfg(target_os = \"linux\")",
            },
            {
                "name": "destination-build",
                "path": str(destination.parent),
                "kind": "build",
                "optional": False,
                "target": None,
            },
            {
                "name": "destination-dev",
                "path": str(destination.parent),
                "kind": "dev",
                "optional": False,
                "target": None,
            },
        ]
        fixture.metadata_for(workspace, [fixture.package_record("source", source, dependencies)])
        fixture.metadata_for(destination, [destination_record])
        # The source manifest models a workspace-inherited dependency.  The
        # resolved path/kind/target data comes solely from fake Cargo metadata.
        source.write_text(
            '[package]\nname = "source"\nversion = "0.1.0"\n'
            '[workspace.dependencies]\ndestination = { path = "../../standalone/destination" }\n'
            '[dependencies]\ndestination.workspace = true\n'
        )
        (fixture.root / "vendor" / "ignored").mkdir(parents=True)
        (fixture.root / "vendor" / "ignored" / "Cargo.toml").write_text(
            '[package]\nname = "vendored"\nversion = "0.1.0"\n'
        )
        policy = fixture.policy(
            {
                "source_group": ["workspace/source"],
                "destination_group": ["standalone/destination"],
            },
            {"source_group": ["destination_group"], "destination_group": []},
        )
        self.assertEqual(fixture.check(policy), [])

    def test_forbidden_edge_reports_kind_target_optional_and_alias(self) -> None:
        fixture = Fixture()
        self.addCleanup(fixture.close)
        workspace = fixture.manifest("workspace", workspace=True)
        workspace.write_text('[workspace]\nmembers = ["source"]\n')
        source = fixture.package("workspace/source", "source")
        destination = fixture.package("standalone/destination", "destination")
        dependency = {
            "name": "dest_alias",
            "rename": "destination",
            "path": str(destination.parent),
            "kind": "build",
            "optional": True,
            "target": "cfg(unix)",
        }
        fixture.metadata_for(workspace, [fixture.package_record("source", source, [dependency])])
        fixture.metadata_for(destination, [fixture.package_record("destination", destination)])
        policy = fixture.policy(
            {
                "source_group": ["workspace/source"],
                "destination_group": ["standalone/destination"],
            },
            {"source_group": [], "destination_group": []},
        )
        errors = fixture.check(policy)
        self.assertEqual(len(errors), 1)
        self.assertIn(
            "forbidden source (source_group) -> destination (destination_group)", errors[0]
        )
        self.assertIn("build, optional, target cfg(unix)", errors[0])
        self.assertIn("dest_alias as destination", errors[0])

    def test_unclassified_crate_and_unknown_exception_are_errors(self) -> None:
        fixture = Fixture()
        self.addCleanup(fixture.close)
        workspace = fixture.manifest("workspace", workspace=True)
        workspace.write_text('[workspace]\nmembers = ["source"]\n')
        source = fixture.package("workspace/source", "source")
        unclassified = fixture.package("standalone/unclassified", "unclassified")
        fixture.metadata_for(workspace, [fixture.package_record("source", source)])
        fixture.metadata_for(unclassified, [fixture.package_record("unclassified", unclassified)])
        policy = fixture.policy(
            {"source_group": ["workspace/source"]},
            {"source_group": []},
            "[[exceptions]]\n"
            'source = "workspace/source"\n'
            'destination = "standalone/missing"\n'
            'kinds = ["normal"]\n'
            'reason = "no longer needed"\n'
            'issue = "issue-123"',
        )
        errors = fixture.check(policy)
        self.assertTrue(
            any("unclassified first-party crate unclassified" in error for error in errors)
        )
        self.assertTrue(
            any("policy exception 0 names unknown destination" in error for error in errors)
        )

    def test_duplicate_group_member_is_rejected(self) -> None:
        fixture = Fixture()
        self.addCleanup(fixture.close)
        package = fixture.package("standalone/package", "package")
        fixture.metadata_for(fixture.manifest("workspace", workspace=True), [])
        fixture.metadata_for(package, [fixture.package_record("package", package)])
        policy = fixture.policy(
            {"group": ["standalone/package", "standalone/package"]},
            {"group": []},
        )
        with self.assertRaises(CHECKER.BoundaryError):
            CHECKER.load_policy(policy, fixture.root)

    def test_vendored_path_dependency_is_not_a_first_party_edge(self) -> None:
        fixture = Fixture()
        self.addCleanup(fixture.close)
        workspace = fixture.manifest("workspace", workspace=True)
        workspace.write_text('[workspace]\nmembers = ["source"]\n')
        source = fixture.package("workspace/source", "source")
        vendor = fixture.root / "vendor" / "dependency"
        vendor.mkdir(parents=True)
        vendor_manifest = vendor / "Cargo.toml"
        vendor_manifest.write_text(
            '[package]\nname = "vendored-dependency"\nversion = "0.1.0"\n'
        )
        fixture.metadata_for(
            workspace,
            [
                fixture.package_record(
                    "source",
                    source,
                    [{"name": "vendored-dependency", "path": str(vendor)}],
                )
            ],
        )
        policy = fixture.policy(
            {"source_group": ["workspace/source"]},
            {"source_group": []},
        )
        self.assertEqual(fixture.check(policy), [])


if __name__ == "__main__":
    unittest.main()
