#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Examples pin the PR fan-out contract across product boundaries."""

import unittest
from ci_scope import selected


class ScopeTests(unittest.TestCase):
    def active(self, *paths):
        return {key for key, value in selected(paths).items() if value}

    def test_nes_searcher_pr_does_not_run_database_or_vm_smokes(self):
        self.assertEqual(self.active("dissonance/searcher/src/search/archive.rs", "workloads/nes/src/metroid/target.rs"), {"native"})

    def test_stb_change_selects_its_real_game(self):
        for path in ("workloads/nes/src/stb/target.rs", "workloads/nes/src/bin/stb-probe.rs", "workloads/nes/scripts/build-stb-rom.sh"):
            self.assertEqual(self.active(path), {"stb"})

    def test_fault_changes_select_database(self):
        self.assertEqual(self.active("workloads/faults/src/target.rs"), {"faults"})

    def test_historical_publisher_and_report_select_database_consumer(self):
        for path in (".github/workflows/historical-bugs.yml", "scripts/render-historical-bugs.py"):
            self.assertEqual(self.active(path), {"faults"})

    def test_minimal_guest_admission_selects_platform_consumer(self):
        self.assertEqual(self.active("workloads/guest-images/admission/minimal-component.json"), {"platform"})

    def test_controlled_profile_catalog_selects_runtime_consumers(self):
        self.assertEqual(self.active("workloads/guest-images/admission/controlled-profiles.rs"), {"platform", "kvm", "faults", "public_api"})

    def test_backend_changes_select_both_platform_seams(self):
        self.assertEqual(self.active("consonance/vmm-backend/src/kvm.rs"), {"platform", "kvm", "public_api"})

    def test_shared_process_protocol_selects_consumers(self):
        self.assertEqual(self.active("consonance/process-proto/src/events.rs"), {"platform", "faults", "public_api"})

    def test_docs_and_evaluator_reporting_do_not_launch_smokes(self):
        self.assertFalse(self.active("consonance/vmm-core/README.md", "docs/WORKFLOWS.md", "benchmarks/search/eval.py"))

    def test_dependency_and_selector_changes_expand_conservatively(self):
        for path in ("Cargo.lock", "workloads/nes/Cargo.toml", "scripts/ci_scope.py", ".github/workflows/product-smoke.yml"):
            self.assertEqual(self.active(path), {"native", "stb", "faults", "platform", "kvm", "public_api"})

    def test_shared_emulator_build_selects_both_source_built_games(self):
        self.assertEqual(self.active("scripts/build-quicknes-core.sh"), {"native", "stb"})


if __name__ == "__main__":
    unittest.main()
