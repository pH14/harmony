#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Focused tests for the path-selected PR Miri matrix."""

from __future__ import annotations

import unittest

try:
    from miri_scope import selected_targets
except ModuleNotFoundError:  # Supports ``python -m unittest scripts/...``.
    from scripts.miri_scope import selected_targets


class MiriScopeTests(unittest.TestCase):
    def names(self, paths: list[str]) -> set[str]:
        return {target["name"] for target in selected_targets(paths)}

    def test_each_unsafe_crate_selects_its_own_target(self) -> None:
        vmm_core = selected_targets(["consonance/vmm-core/src/bringup.rs"])
        self.assertEqual({target["name"] for target in vmm_core}, {"vmm-core"})
        self.assertIn(
            "vendor::x86::bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping",
            vmm_core[0]["command"],
        )
        self.assertEqual(self.names(["consonance/hypercall-doorbell/src/lib.rs"]), {"hypercall-doorbell"})
        self.assertEqual(self.names(["consonance/client/src/watchdog.rs"]), {"consonance-client"})
        self.assertEqual(self.names(["consonance/vm-state/src/lib.rs"]), {"vm-state"})
        self.assertEqual(self.names(["consonance/vmm-backend/src/kvm.rs"]), {"vmm-backend"})
        self.assertEqual(self.names(["consonance/snapshot-store/src/lib.rs"]), {"snapshot-store"})
        self.assertEqual(self.names(["workloads/nes-machine/src/lib.rs"]), {"machine"})
        self.assertEqual(self.names(["workloads/nes-guest/src/lib.rs"]), {"nes-guest"})
        self.assertEqual(
            self.names(["consonance/harmony-linux/supervisor/src/lib.rs"]),
            {"harmony-supervisor"},
        )

    def test_dependency_and_toolchain_changes_select_every_target(self) -> None:
        names = self.names(["workloads/nes-guest/Cargo.toml"])
        self.assertEqual(names, {target["name"] for target in selected_targets(["Cargo.lock"])})
        self.assertEqual(names, {target["name"] for target in selected_targets(["rust-toolchain.toml"])})

    def test_unrelated_paths_and_empty_diffs_select_none(self) -> None:
        self.assertEqual(
            self.names(["docs/WORKFLOWS.md", "workloads/nes/src/lib.rs", "cli/src/main.rs"]),
            set(),
        )
        self.assertEqual(self.names([]), set())


if __name__ == "__main__":
    unittest.main()
