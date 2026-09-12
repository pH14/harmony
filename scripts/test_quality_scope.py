#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Focused tests for changed-path formal-safety selection."""

from __future__ import annotations

import unittest

try:
    from quality_scope import kani_required
except ModuleNotFoundError:  # Supports ``python -m unittest scripts/...``.
    from scripts.quality_scope import kani_required


class KaniScopeTests(unittest.TestCase):
    def test_unsafe_crates_are_selected(self) -> None:
        self.assertTrue(kani_required(["consonance/vtime/src/clock.rs"]))
        self.assertTrue(kani_required(["consonance/lapic/src/device.rs"]))
        self.assertTrue(kani_required(["consonance/vmm-backend/src/region.rs"]))
        self.assertTrue(kani_required(["workloads/fault-policy/src/envcodec.rs"]))

    def test_workspace_dependency_and_configuration_changes_are_selected(self) -> None:
        self.assertTrue(kani_required(["Cargo.toml"]))
        self.assertTrue(kani_required(["consonance/vmm-core/Cargo.toml"]))
        self.assertTrue(kani_required(["workloads/fault-policy/Cargo.lock"]))
        self.assertTrue(kani_required(["rust-toolchain.toml"]))
        self.assertTrue(kani_required([".cargo/config.toml"]))
        self.assertTrue(kani_required(["flake.lock"]))

    def test_unrelated_changes_do_not_select_kani(self) -> None:
        self.assertFalse(kani_required(["cli/src/main.rs"]))
        self.assertFalse(kani_required(["workloads/nes/src/lib.rs"]))
        self.assertFalse(kani_required(["docs/WORKFLOWS.md"]))

    def test_an_empty_change_set_does_not_select_kani(self) -> None:
        self.assertFalse(kani_required([]))


if __name__ == "__main__":
    unittest.main()
